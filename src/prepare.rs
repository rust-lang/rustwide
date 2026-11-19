use crate::cmd::{Command, CommandError, ProcessLinesActions};
use crate::{Crate, Toolchain, Workspace, build::CratePatch};
use anyhow::{Context as _, anyhow};
use log::info;
use std::path::{Path, PathBuf};
use toml::{Value, value::Table};

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

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all, level = "debug"))]
    pub(crate) fn prepare(&mut self) -> anyhow::Result<()> {
        self.krate.copy_source_to(self.workspace, self.source_dir)?;
        self.remove_override_files()?;
        self.tweak_toml()?;
        self.validate_manifest()?;
        self.capture_lockfile()?;
        self.fetch_deps()?;

        Ok(())
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all, level = "debug"))]
    fn validate_manifest(&self) -> anyhow::Result<()> {
        info!(
            "validating manifest of {} on toolchain {}",
            self.krate, self.toolchain
        );

        // Skip crates missing a Cargo.toml
        if !self.source_dir.join("Cargo.toml").is_file() {
            return Err(PrepareError::MissingCargoToml.into());
        }

        Command::new(self.workspace, self.toolchain.cargo())
            .args(["metadata", "--manifest-path", "Cargo.toml", "--no-deps"])
            .current_directory(self.source_dir)
            .log_output(false)
            .run_capture()
            .context(PrepareError::InvalidCargoTomlSyntax)?;

        Ok(())
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all, level = "debug"))]
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

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all, level = "debug"))]
    fn tweak_toml(&self) -> anyhow::Result<()> {
        let path = self.source_dir.join("Cargo.toml");
        let mut tweaker = TomlTweaker::new(self.krate, &path, &self.patches)?;
        tweaker.tweak()?;
        tweaker.save(&path)?;
        Ok(())
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all, level = "debug"))]
    fn capture_lockfile(&mut self) -> anyhow::Result<()> {
        if self.source_dir.join("Cargo.lock").exists() {
            info!(
                "crate {} already has a lockfile, it will not be regenerated",
                self.krate
            );
            return Ok(());
        }

        let mut cmd = Command::new(self.workspace, self.toolchain.cargo()).args([
            "generate-lockfile",
            "--manifest-path",
            "Cargo.toml",
        ]);
        if !self.workspace.fetch_registry_index_during_builds() {
            cmd = cmd
                .args(["-Zno-index-update"])
                .env("__CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS", "nightly");
        }

        run_command(cmd.current_directory(self.source_dir))
    }

    #[cfg_attr(feature = "tracing", tracing::instrument(skip_all, level = "debug"))]
    fn fetch_deps(&mut self) -> anyhow::Result<()> {
        fetch_deps(self.workspace, self.toolchain, self.source_dir, &[])
    }
}

#[cfg_attr(feature = "tracing", tracing::instrument(skip_all, level = "debug"))]
pub(crate) fn fetch_deps(
    workspace: &Workspace,
    toolchain: &Toolchain,
    source_dir: &Path,
    fetch_build_std_targets: &[&str],
) -> anyhow::Result<()> {
    let mut cmd = Command::new(workspace, toolchain.cargo())
        .args(["fetch", "--manifest-path", "Cargo.toml"])
        .current_directory(source_dir);
    // Pass `-Zbuild-std` in case a build in the sandbox wants to use it;
    // build-std has to have the source for libstd's dependencies available.
    if !fetch_build_std_targets.is_empty() {
        toolchain.add_component(workspace, "rust-src")?;
        cmd = cmd.args(["-Zbuild-std"]).env("RUSTC_BOOTSTRAP", "1");
    }
    for target in fetch_build_std_targets {
        cmd = cmd.args(["--target", target]);
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
            || line.contains("no matching package found")
            || line.contains("no matching package for override ")
            || (line.contains("The patch location ")
                && line.contains(" does not appear to contain any packages matching the name "))
            || (line.contains("patch location ")
                && line.contains(" does not contain packages matching "))
        {
            missing_deps = true;
        } else if line.contains("failed to parse manifest at")
            || line.contains("error: invalid table header")
            || (line.contains("error: unexpected ") && line.contains(", expected "))
            || line.contains("error: invalid type: ")
            || line.contains("error: cyclic feature dependency: feature ")
            || line.contains("error: cyclic package dependency: package ")
            || (line.contains("error: package collision in the lockfile: packages ")
                && line.contains(
                    " are different, but only one can be written to lockfile unambiguously",
                ))
            || (line.contains("failed to select a version for ")
                && line.contains(" which could resolve this conflict"))
        {
            broken_deps = true;
        } else if line.contains("error: failed to parse lock file at")
            || line.contains(
                "error: Attempting to resolve a dependency with more than one crate with links=",
            )
            || (line.contains("error: checksum for ")
                && line.contains(" changed between lock files"))
        {
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

    pub fn tweak(&mut self) -> anyhow::Result<()> {
        info!("started tweaking {}", self.krate);

        self.remove_missing_items("example");
        self.remove_missing_items("test");
        self.remove_parent_workspaces();
        self.remove_unwanted_cargo_features();
        self.apply_patches()?;

        info!("finished tweaking {}", self.krate);
        Ok(())
    }

    /// Remove the `[[example]]` / `[[test]]` entries whose target file is
    /// missing, or that aren't a table with a string `name`.
    fn remove_missing_items(&mut self, category: &str) {
        let folder = &(String::from(category) + "s");
        if let Some(dir) = self.dir
            && let Some(&mut Value::Array(ref mut array)) = self.table.get_mut(category)
        {
            let dim = array.len();
            array.retain(|item| {
                let Some(name) = item.get("name").and_then(Value::as_str) else {
                    return false;
                };
                match item.get("path") {
                    None => dir.join(folder).join(format!("{name}.rs")).exists(),
                    // A non-string `path` is kept for cargo to reject.
                    Some(path) => path.as_str().is_none_or(|path| dir.join(path).exists()),
                }
            });
            info!("removed {} missing {}", dim - array.len(), folder);
        }
    }

    fn remove_parent_workspaces(&mut self) {
        let krate = self.krate.to_string();

        // Eliminate parent workspaces
        if let Some(&mut Value::Table(ref mut package)) = self.table.get_mut("package")
            && package.remove("workspace").is_some()
        {
            info!("removed parent workspace from {krate}");
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

    fn apply_patches(&mut self) -> anyhow::Result<()> {
        if self.patches.is_empty() {
            return Ok(());
        }

        // The caller asked for patches, so if the crate's `patch` or
        // `patch.crates-io` isn't a table (which makes its manifest invalid)
        // we can't apply them and fail instead.
        let cratesio_table = self
            .table
            .entry("patch")
            .or_insert_with(|| Value::Table(Table::new()))
            .as_table_mut()
            .ok_or_else(|| anyhow!("can't apply patches: `patch` is not a table"))
            .context(PrepareError::InvalidCargoTomlSyntax)?
            .entry("crates-io")
            .or_insert_with(|| Value::Table(Table::new()))
            .as_table_mut()
            .ok_or_else(|| anyhow!("can't apply patches: `patch.crates-io` is not a table"))
            .context(PrepareError::InvalidCargoTomlSyntax)?;

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
                    table.insert("path".into(), Value::String(patch.path));
                    (patch.name, table)
                }
            };

            cratesio_table.insert(name, Value::Table(table));
        }

        Ok(())
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
    /// The crate source contains a symlink that points outside of the source
    /// directory, or can't be resolved. Following it on the host could read,
    /// write or delete host files.
    ///
    /// Contains the path of the symlink, relative to the source directory. It
    /// is escaped in the error message, but not in this field.
    #[error(
        "the crate contains a symlink that escapes its source directory or can't be resolved: {0:?}"
    )]
    PathEscapesSource(PathBuf),
    /// The crate tarball contains an entry that can't be unpacked safely: an
    /// entry outside of the `{name}-{version}/` directory, a path with `..`,
    /// or an entry that is neither a regular file nor a directory (e.g. a
    /// symlink or hard link).
    #[error("the crate archive contains an invalid entry {path:?}: {reason}")]
    InvalidArchiveEntry {
        /// Path of the entry inside the archive.
        path: PathBuf,
        /// Why the entry was rejected.
        reason: String,
    },
    /// Uncategorized error
    #[doc(hidden)]
    #[error("uncategorized prepare error")]
    Uncategorized,
}

#[cfg(test)]
mod tests {
    use super::{PrepareError, TomlTweaker};
    use crate::build::{CratePatch, GitCratePatch, PathCratePatch};
    use crate::crates::Crate;
    use std::fs;
    use test_case::test_case;
    use toml::toml;

    #[test]
    fn test_tweak_keeps_items_with_non_string_path() {
        let dir = tempfile::tempdir().unwrap();
        let cargo_toml = dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            toml! {
                example = [
                    { name = "invalid-path", path = 1 },
                    { name = 1, path = "examples/a.rs" },
                    { path = "examples/a.rs" },
                    1,
                    { name = "missing" },
                ]
            }
            .to_string(),
        )
        .unwrap();

        let krate = Crate::local("/dev/null".as_ref());
        let mut tweaker = TomlTweaker::new(&krate, &cargo_toml, &[]).unwrap();
        tweaker.tweak().unwrap();

        // the non-string path is kept for cargo to reject, the other items
        // are removed as before
        assert_eq!(
            tweaker.table,
            toml! {
                example = [{ name = "invalid-path", path = 1 }]
            }
        );
    }

    #[test_case(toml! { patch = 1 }, "`patch` is not a table"; "patch")]
    #[test_case(
        toml! {
            [patch]
            crates-io = 1
        },
        "`patch.crates-io` is not a table";
        "patch crates-io"
    )]
    fn test_tweak_patches_fails_on_non_table_patch_sections(manifest: toml::Table, reason: &str) {
        let patches = vec![CratePatch::Path(PathCratePatch {
            name: "bar".into(),
            path: "/path/to/bar".into(),
        })];
        let krate = Crate::local("/dev/null".as_ref());
        let mut tweaker = TomlTweaker::new_with_table(&krate, manifest, &patches);

        let err = tweaker.tweak().unwrap_err();
        assert!(matches!(
            err.downcast_ref::<PrepareError>(),
            Some(PrepareError::InvalidCargoTomlSyntax)
        ));
        assert!(format!("{err:#}").contains(reason), "error: {err:#}");
    }

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
        tweaker.tweak().unwrap();

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
        tweaker.tweak().unwrap();

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
        tweaker.tweak().unwrap();

        assert_eq!(tweaker.table, result);
    }
}
