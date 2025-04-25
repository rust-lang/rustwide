use crate::cmd::{Command, CommandError, ProcessLinesActions};
use crate::{build::CratePatch, Crate, Toolchain, Workspace};
use anyhow::Context as _;
use log::info;
use std::path::Path;
use toml::{
    value::{Array, Table},
    Value,
};

pub(crate) struct Prepare<'a> {
    workspace: &'a Workspace,
    toolchain: &'a Toolchain,
    krate: &'a Crate,
    source_dir: &'a Path,
    patches: Vec<CratePatch>,
}

impl<'a> Prepare<'a> {
    pub(crate) fn new(
        workspace: &'a Workspace,
        toolchain: &'a Toolchain,
        krate: &'a Crate,
        source_dir: &'a Path,
        patches: Vec<CratePatch>,
    ) -> Self {
        Self {
            workspace,
            toolchain,
            krate,
            source_dir,
            patches,
        }
    }

    pub(crate) fn prepare(&mut self) -> anyhow::Result<()> {
        self.krate.copy_source_to(self.workspace, self.source_dir)?;
        self.remove_override_files()?;
        self.tweak_toml()?;
        self.validate_manifest()?;
        self.capture_lockfile()?;
        self.fetch_deps()?;

        Ok(())
    }

    fn validate_manifest(&self) -> anyhow::Result<()> {
        info!(
            "validating manifest of {} on toolchain {}",
            self.krate, self.toolchain
        );

        // Skip crates missing a Cargo.toml
        if !self.source_dir.join("Cargo.toml").is_file() {
            return Err(PrepareError::MissingCargoToml.into());
        }

        let res = Command::new(self.workspace, self.toolchain.cargo())
            .args(&["metadata", "--manifest-path", "Cargo.toml", "--no-deps"])
            .cd(self.source_dir)
            .log_output(false)
            .run();
        if res.is_err() {
            return Err(PrepareError::InvalidCargoTomlSyntax.into());
        }

        Ok(())
    }

    fn remove_override_files(&self) -> anyhow::Result<()> {
        let paths = [
            &Path::new(".cargo").join("config"),
            &Path::new(".cargo").join("config.toml"),
            Path::new("rust-toolchain"),
            Path::new("rust-toolchain.toml"),
        ];
        for path in &paths {
            let path = self.source_dir.join(path);
            if path.exists() {
                crate::utils::remove_file(&path)?;
                info!("removed {}", path.display());
            }
        }
        Ok(())
    }

    fn tweak_toml(&self) -> anyhow::Result<()> {
        let path = self.source_dir.join("Cargo.toml");
        let mut tweaker = TomlTweaker::new(self.krate, &path, &self.patches)?;
        tweaker.tweak();
        tweaker.save(&path)?;
        Ok(())
    }

    fn capture_lockfile(&mut self) -> anyhow::Result<()> {
        if self.source_dir.join("Cargo.lock").exists() {
            info!(
                "crate {} already has a lockfile, it will not be regenerated",
                self.krate
            );
            return Ok(());
        }

        let mut cmd = Command::new(self.workspace, self.toolchain.cargo()).args(&[
            "generate-lockfile",
            "--manifest-path",
            "Cargo.toml",
        ]);
        if !self.workspace.fetch_registry_index_during_builds() {
            cmd = cmd
                .args(&["-Zno-index-update"])
                .env("__CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS", "nightly");
        }

        run_command(cmd.cd(self.source_dir))
    }

    fn fetch_deps(&mut self) -> anyhow::Result<()> {
        fetch_deps(self.workspace, self.toolchain, self.source_dir, &[])
    }
}

pub(crate) fn fetch_deps(
    workspace: &Workspace,
    toolchain: &Toolchain,
    source_dir: &Path,
    fetch_build_std_targets: &[&str],
) -> anyhow::Result<()> {
    let mut cmd = Command::new(workspace, toolchain.cargo())
        .args(&["fetch", "--manifest-path", "Cargo.toml"])
        .cd(source_dir);
    // Pass `-Zbuild-std` in case a build in the sandbox wants to use it;
    // build-std has to have the source for libstd's dependencies available.
    if !fetch_build_std_targets.is_empty() {
        toolchain.add_component(workspace, "rust-src")?;
        cmd = cmd.args(&["-Zbuild-std"]).env("RUSTC_BOOTSTRAP", "1");
    }
    for target in fetch_build_std_targets {
        cmd = cmd.args(&["--target", target]);
    }

    run_command(cmd)
}

fn run_command(cmd: Command) -> anyhow::Result<()> {
    let mut yanked_deps = false;
    let mut missing_deps = false;
    let mut broken_deps = false;
    let mut broken_lockfile = false;

    let mut process = |line: &str, _: &mut ProcessLinesActions| {
        if line.contains("failed to select a version for the requirement") {
            yanked_deps = true;
        } else if line.contains("failed to load source for dependency")
            || line.contains("no matching package named")
        {
            missing_deps = true;
        } else if line.contains("failed to parse manifest at")
            || line.contains("error: invalid table header")
        {
            broken_deps = true;
        } else if line.contains("error: failed to parse lock file at") {
            broken_lockfile = true;
        }
    };

    match cmd.process_lines(&mut process).run_capture() {
        Ok(_) => Ok(()),
        Err(CommandError::ExecutionFailed { status: _, stderr }) if yanked_deps => {
            Err(PrepareError::YankedDependencies(stderr).into())
        }
        Err(CommandError::ExecutionFailed { status: _, stderr }) if missing_deps => {
            Err(PrepareError::MissingDependencies(stderr).into())
        }
        Err(CommandError::ExecutionFailed { status: _, stderr }) if broken_deps => {
            Err(PrepareError::BrokenDependencies(stderr).into())
        }
        Err(CommandError::ExecutionFailed { status: _, stderr }) if broken_lockfile => {
            Err(PrepareError::InvalidCargoLock(stderr).into())
        }
        Err(err) => Err(err.into()),
    }
}

struct TomlTweaker<'a> {
    krate: &'a Crate,
    table: Table,
    dir: Option<&'a Path>,
    patches: Vec<CratePatch>,
}

impl<'a> TomlTweaker<'a> {
    pub fn new(
        krate: &'a Crate,
        cargo_toml: &'a Path,
        patches: &[CratePatch],
    ) -> anyhow::Result<Self> {
        let toml_content =
            ::std::fs::read_to_string(cargo_toml).context(PrepareError::MissingCargoToml)?;
        let table: Table =
            toml::from_str(&toml_content).context(PrepareError::InvalidCargoTomlSyntax)?;

        let dir = cargo_toml.parent();

        Ok(TomlTweaker {
            krate,
            table,
            dir,
            patches: patches.to_vec(),
        })
    }

    #[cfg(test)]
    fn new_with_table(krate: &'a Crate, table: Table, patches: &[CratePatch]) -> Self {
        TomlTweaker {
            krate,
            table,
            dir: None,
            patches: patches.to_vec(),
        }
    }

    pub fn tweak(&mut self) {
        info!("started tweaking {}", self.krate);

        self.remove_missing_items("example");
        self.remove_missing_items("test");
        self.remove_parent_workspaces();
        self.remove_unwanted_cargo_features();
        self.apply_patches();

        info!("finished tweaking {}", self.krate);
    }

    #[allow(clippy::ptr_arg)]
    fn test_existance(dir: &Path, value: &Array, folder: &str) -> Array {
        value
            .iter()
            .filter_map(|t| t.as_table())
            .filter_map(|t| {
                t.get("name")
                    .and_then(Value::as_str)
                    .map(|n| (t, n.to_owned()))
            })
            .map(|(table, name)| {
                let path = table.get("path").map_or_else(
                    || dir.join(folder).join(name + ".rs"),
                    |path| dir.join(path.as_str().unwrap()),
                );
                (table, path)
            })
            .filter(|(_table, path)| path.exists())
            .filter_map(|(table, _path)| Value::try_from(table).ok())
            .collect()
    }

    fn remove_missing_items(&mut self, category: &str) {
        let folder = &(String::from(category) + "s");
        if let Some(dir) = self.dir {
            if let Some(&mut Value::Array(ref mut array)) = self.table.get_mut(category) {
                let dim = array.len();
                *(array) = Self::test_existance(dir, array, folder);
                info!("removed {} missing {}", dim - array.len(), folder);
            }
        }
    }

    fn remove_parent_workspaces(&mut self) {
        let krate = self.krate.to_string();

        // Eliminate parent workspaces
        if let Some(&mut Value::Table(ref mut package)) = self.table.get_mut("package") {
            if package.remove("workspace").is_some() {
                info!("removed parent workspace from {krate}");
            }
        }
    }

    fn remove_unwanted_cargo_features(&mut self) {
        let krate = self.krate.to_string();

        // Remove the unwanted features from the main list
        let mut has_publish_lockfile = false;
        let mut has_default_run = false;
        if let Some(&mut Value::Array(ref mut vec)) = self.table.get_mut("cargo-features") {
            vec.retain(|key| {
                if let Value::String(key) = key {
                    match key.as_str() {
                        "publish-lockfile" => has_publish_lockfile = true,
                        "default-run" => has_default_run = true,
                        _ => return true,
                    }
                }

                false
            });
        }

        // Strip the 'publish-lockfile' key from [package]
        if has_publish_lockfile {
            info!("disabled cargo feature 'publish-lockfile' from {krate}");
            if let Some(&mut Value::Table(ref mut package)) = self.table.get_mut("package") {
                package.remove("publish-lockfile");
            }
        }

        // Strip the 'default-run' key from [package]
        if has_default_run {
            info!("disabled cargo feature 'default-run' from {krate}");
            if let Some(&mut Value::Table(ref mut package)) = self.table.get_mut("package") {
                package.remove("default-run");
            }
        }
    }

    fn apply_patches(&mut self) {
        if !self.patches.is_empty() {
            let mut patch_table = self.table.get_mut("patch");
            let patch_table = match patch_table {
                Some(ref mut pt) => pt,
                None => {
                    self.table
                        .insert("patch".to_string(), Value::Table(Table::new()));
                    self.table.get_mut("patch").unwrap()
                }
            };

            let mut cratesio_table = patch_table.get_mut("crates-io");
            let cratesio_table = match cratesio_table {
                Some(ref mut cio) => cio,
                None => {
                    patch_table
                        .as_table_mut()
                        .unwrap()
                        .insert("crates-io".to_string(), Value::Table(Table::new()));
                    patch_table.get_mut("crates-io").unwrap()
                }
            };

            for patch in self.patches.iter().cloned() {
                let (name, table) = match patch {
                    CratePatch::Git(patch) => {
                        let mut table = Table::new();
                        table.insert("git".into(), Value::String(patch.uri));
                        table.insert("branch".into(), Value::String(patch.branch));
                        (patch.name, table)
                    }
                    CratePatch::Path(patch) => {
                        let mut table = Table::new();
                        table.insert("path".into(), Value::String(patch.path.clone()));
                        (patch.name, table)
                    }
                };

                cratesio_table
                    .as_table_mut()
                    .unwrap()
                    .insert(name, Value::Table(table));
            }
        }
    }

    pub fn save(self, output_file: &Path) -> anyhow::Result<()> {
        let crate_name = self.krate.to_string();
        ::std::fs::write(output_file, toml::to_string(&self.table)?.as_bytes())?;
        info!(
            "tweaked toml for {} written to {}",
            crate_name,
            output_file.to_string_lossy()
        );

        Ok(())
    }
}

/// Error happened while preparing a crate for a build.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PrepareError {
    /// The git repository isn't publicly available.
    #[error("can't fetch private git repositories")]
    PrivateGitRepository,
    /// The crate doesn't have a `Cargo.toml` in its source code.
    #[error("missing Cargo.toml")]
    MissingCargoToml,
    /// The crate's Cargo.toml is invalid, either due to a TOML syntax error in it or cargo
    /// rejecting it.
    #[error("invalid Cargo.toml syntax")]
    InvalidCargoTomlSyntax,
    /// Something about the crates dependencies is invalid
    #[error("broken dependencies: \n\n{0}")]
    BrokenDependencies(String),
    /// Some of this crate's dependencies were yanked, preventing Crater from fetching them.
    #[error("the crate depends on yanked dependencies: \n\n{0}")]
    YankedDependencies(String),
    /// Some of the dependencies do not exist anymore.
    #[error("the crate depends on missing dependencies: \n\n{0}")]
    MissingDependencies(String),
    /// cargo rejected (generating) the lockfile
    #[error("the crate has a broken lockfile: \n\n{0}")]
    InvalidCargoLock(String),
    /// Uncategorized error
    #[doc(hidden)]
    #[error("uncategorized prepare error")]
    Uncategorized,
}

#[cfg(test)]
mod tests {
    use super::TomlTweaker;
    use crate::build::{CratePatch, GitCratePatch, PathCratePatch};
    use crate::crates::Crate;
    use toml::toml;

    #[test]
    fn test_tweak_table_noop() {
        let toml = toml! {
            cargo-features = ["foobar"]

            [package]
            name = "foo"
            version = "1.0"
        };

        let result = toml.clone();

        let krate = Crate::local("/dev/null".as_ref());
        let patches: Vec<CratePatch> = Vec::new();
        let mut tweaker = TomlTweaker::new_with_table(&krate, toml, &patches);
        tweaker.tweak();

        assert_eq!(tweaker.table, result);
    }

    #[test]
    fn test_tweak_table_changes() {
        let toml = toml! {
            cargo-features = ["foobar", "publish-lockfile", "default-run"]

            [package]
            name = "foo"
            version = "1.0"
            workspace = ".."
            publish-lockfile = true
            default-run = "foo"

            [workspace]
            members = []
        };

        let result = toml! {
            cargo-features = ["foobar"]

            [package]
            name = "foo"
            version = "1.0"

            [workspace]
            members = []
        };

        let krate = Crate::local("/dev/null".as_ref());
        let patches: Vec<CratePatch> = Vec::new();
        let mut tweaker = TomlTweaker::new_with_table(&krate, toml, &patches);
        tweaker.tweak();

        assert_eq!(tweaker.table, result);
    }

    #[test]
    fn test_tweak_table_patches() {
        let toml = toml! {
            cargo-features = ["foobar"]

            [package]
            name = "foo"
            version = "1.0"

            [dependencies]
            bar = "1.0"

            [dev-dependencies]
            baz = "1.0"

            [target."cfg(unix)".dependencies]
            quux = "1.0"
        };

        let result = toml! {
            cargo-features = ["foobar"]

            [package]
            name = "foo"
            version = "1.0"

            [dependencies]
            bar = "1.0"

            [dev-dependencies]
            baz = "1.0"

            [target."cfg(unix)".dependencies]
            quux = "1.0"

            [patch.crates-io]
            quux = { git = "https://git.example.com/quux", branch = "dev" }
            baz = { path = "/path/to/baz" }
        };

        let krate = Crate::local("/dev/null".as_ref());
        let patches = vec![
            CratePatch::Git(GitCratePatch {
                name: "quux".into(),
                uri: "https://git.example.com/quux".into(),
                branch: "dev".into(),
            }),
            CratePatch::Path(PathCratePatch {
                name: "baz".into(),
                path: "/path/to/baz".into(),
            }),
        ];
        let mut tweaker = TomlTweaker::new_with_table(&krate, toml, &patches);
        tweaker.tweak();

        assert_eq!(tweaker.table, result);
    }
}
