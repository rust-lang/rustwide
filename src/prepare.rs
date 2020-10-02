use crate::cmd::Command;
use crate::{build::CratePatch, Crate, Toolchain, Workspace};
use failure::{Error, Fail, ResultExt};
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
    lockfile_captured: bool,
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
            lockfile_captured: false,
            patches,
        }
    }

    pub(crate) fn prepare(&mut self) -> Result<(), Error> {
        self.krate.copy_source_to(self.workspace, self.source_dir)?;
        self.validate_manifest()?;
        self.remove_cargo_config()?;
        self.tweak_toml()?;
        self.capture_lockfile(false)?;
        self.fetch_deps()?;

        Ok(())
    }

    fn validate_manifest(&self) -> Result<(), Error> {
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

    fn remove_cargo_config(&self) -> Result<(), Error> {
        let path = self.source_dir.join(".cargo").join("config");
        if path.exists() {
            crate::utils::remove_file(&path)?;
            info!("removed {}", path.display());
        }
        Ok(())
    }

    fn tweak_toml(&self) -> Result<(), Error> {
        let path = self.source_dir.join("Cargo.toml");
        let mut tweaker = TomlTweaker::new(&self.krate, &path, &self.patches)?;
        tweaker.tweak();
        tweaker.save(&path)?;
        Ok(())
    }

    fn capture_lockfile(&mut self, force: bool) -> Result<(), Error> {
        if !force && self.source_dir.join("Cargo.lock").exists() {
            info!(
                "crate {} already has a lockfile, it will not be regenerated",
                self.krate
            );
            return Ok(());
        }

        let mut yanked_deps = false;
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
        let res = cmd
            .cd(self.source_dir)
            .process_lines(&mut |line, _| {
                if line.contains("failed to select a version for the requirement") {
                    yanked_deps = true;
                }
            })
            .run();
        match res {
            Err(_) if yanked_deps => {
                return Err(PrepareError::YankedDependencies.into());
            }
            other => other?,
        }
        self.lockfile_captured = true;
        Ok(())
    }

    fn fetch_deps(&mut self) -> Result<(), Error> {
        let mut outdated_lockfile = false;
        let res = Command::new(self.workspace, self.toolchain.cargo())
            .args(&["fetch", "--locked", "--manifest-path", "Cargo.toml"])
            .cd(&self.source_dir)
            .process_lines(&mut |line, _| {
                if line.ends_with(
                    "Cargo.lock needs to be updated but --locked was passed to prevent this",
                ) {
                    outdated_lockfile = true;
                }
            })
            .run();
        match res {
            Ok(_) => {}
            Err(_) if outdated_lockfile && !self.lockfile_captured => {
                info!("the lockfile is outdated, regenerating it");
                // Force-update the lockfile and recursively call this function to fetch
                // dependencies again.
                self.capture_lockfile(true)?;
                return self.fetch_deps();
            }
            err => return err.map_err(|e| e.into()),
        }
        Ok(())
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
    ) -> Result<Self, Error> {
        let toml_content = ::std::fs::read_to_string(cargo_toml)
            .with_context(|_| PrepareError::MissingCargoToml)?;
        let table: Table =
            toml::from_str(&toml_content).with_context(|_| PrepareError::InvalidCargoTomlSyntax)?;

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
        self.remove_dependencies();
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
                info!("removed parent workspace from {}", krate);
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
            info!("disabled cargo feature 'publish-lockfile' from {}", krate);
            if let Some(&mut Value::Table(ref mut package)) = self.table.get_mut("package") {
                package.remove("publish-lockfile");
            }
        }

        // Strip the 'default-run' key from [package]
        if has_default_run {
            info!("disabled cargo feature 'default-run' from {}", krate);
            if let Some(&mut Value::Table(ref mut package)) = self.table.get_mut("package") {
                package.remove("default-run");
            }
        }
    }

    fn remove_dependencies(&mut self) {
        let krate = self.krate.to_string();

        Self::remove_dependencies_from_table(&mut self.table, &krate);

        // Tweak target-specific dependencies
        if let Some(&mut Value::Table(ref mut targets)) = self.table.get_mut("target") {
            for (_, target) in targets.iter_mut() {
                if let Value::Table(ref mut target_table) = *target {
                    Self::remove_dependencies_from_table(target_table, &krate);
                }
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
                let mut table = Table::new();
                table.insert("git".into(), Value::String(patch.uri));
                table.insert("branch".into(), Value::String(patch.branch));
                cratesio_table
                    .as_table_mut()
                    .unwrap()
                    .insert(patch.name, Value::Table(table));
            }
        }
    }

    // This is not a method to avoid borrow checker problems
    fn remove_dependencies_from_table(table: &mut Table, krate: &str) {
        // Convert path dependencies to registry dependencies
        for section in &["dependencies", "dev-dependencies", "build-dependencies"] {
            if let Some(&mut Value::Table(ref mut deps)) = table.get_mut(*section) {
                // Iterate through the "name = { ... }", removing any "path"
                // keys in the dependency definition
                for (dep_name, v) in deps.iter_mut() {
                    if let Value::Table(ref mut dep_props) = *v {
                        if dep_props.remove("path").is_some() {
                            info!("removed path dependency {} from {}", dep_name, krate);
                        }
                    }
                }
            }
        }
    }

    pub fn save(self, output_file: &Path) -> Result<(), Error> {
        let crate_name = self.krate.to_string();
        ::std::fs::write(output_file, Value::Table(self.table).to_string().as_bytes())?;
        info!(
            "tweaked toml for {} written to {}",
            crate_name,
            output_file.to_string_lossy()
        );

        Ok(())
    }
}

/// Error happened while preparing a crate for a build.
#[derive(Debug, Fail)]
#[non_exhaustive]
pub enum PrepareError {
    /// The git repository isn't publicly available.
    #[fail(display = "can't fetch private git repositories")]
    PrivateGitRepository,
    /// The crate doesn't have a `Cargo.toml` in its source code.
    #[fail(display = "missing Cargo.toml")]
    MissingCargoToml,
    /// The crate's Cargo.toml is invalid, either due to a TOML syntax error in it or cargo
    /// rejecting it.
    #[fail(display = "invalid Cargo.toml syntax")]
    InvalidCargoTomlSyntax,
    /// Some of this crate's dependencies were yanked, preventing Crater from fetching them.
    #[fail(display = "the crate depends on yanked dependencies")]
    YankedDependencies,
}

#[cfg(test)]
mod tests {
    use super::TomlTweaker;
    use crate::build::CratePatch;
    use crate::crates::Crate;
    use toml::{self, Value};

    #[test]
    fn test_tweak_table_noop() {
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

        let result = toml.clone();

        let krate = Crate::local("/dev/null".as_ref());
        let patches: Vec<CratePatch> = Vec::new();
        let mut tweaker =
            TomlTweaker::new_with_table(&krate, toml.as_table().unwrap().clone(), &patches);
        tweaker.tweak();

        assert_eq!(Value::Table(tweaker.table), result);
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

            [dependencies]
            bar = { version = "1.0", path = "../bar" }

            [dev-dependencies]
            baz = { version = "1.0", path = "../baz" }

            [target."cfg(unix)".dependencies]
            quux = { version = "1.0", path = "../quux" }

            [workspace]
            members = []
        };

        let result = toml! {
            cargo-features = ["foobar"]

            [package]
            name = "foo"
            version = "1.0"

            [dependencies]
            bar = { version = "1.0" }

            [dev-dependencies]
            baz = { version = "1.0" }

            [target."cfg(unix)".dependencies]
            quux = { version = "1.0" }

            [workspace]
            members = []
        };

        let krate = Crate::local("/dev/null".as_ref());
        let patches: Vec<CratePatch> = Vec::new();
        let mut tweaker =
            TomlTweaker::new_with_table(&krate, toml.as_table().unwrap().clone(), &patches);
        tweaker.tweak();

        assert_eq!(Value::Table(tweaker.table), result);
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
        };

        let krate = Crate::local("/dev/null".as_ref());
        let patches = vec![CratePatch {
            name: "quux".into(),
            uri: "https://git.example.com/quux".into(),
            branch: "dev".into(),
        }];
        let mut tweaker =
            TomlTweaker::new_with_table(&krate, toml.as_table().unwrap().clone(), &patches);
        tweaker.tweak();

        assert_eq!(Value::Table(tweaker.table), result);
    }
}
