//! Tools to manage and use Rust toolchains.

use crate::cmd::{Binary, Command, Runnable};
use crate::tools::RUSTUP;
#[cfg(feature = "unstable-toolchain-ci")]
use crate::tools::RUSTUP_TOOLCHAIN_INSTALL_MASTER;
use crate::Workspace;
use failure::{Error, ResultExt};
use log::info;
use std::borrow::Cow;
use std::path::Path;

pub(crate) const MAIN_TOOLCHAIN_NAME: &str = "stable";

/// Error caused by methods in the `toolchain` moodule.
#[derive(Debug, failure::Fail)]
#[non_exhaustive]
pub enum ToolchainError {
    /// The toolchain is not installed in the workspace, but the called method requires it to be
    /// present.  Use the [`Toolchain::Install`](struct.Toolchain.html#method.install) method to
    /// install it inside the workspace.
    #[fail(display = "the toolchain is not installed")]
    NotInstalled,
    /// Not every method can be called with every kind of toolchain. If you receive this error
    /// please check the documentation of the method you're calling to see which toolchains can you
    /// use with it.
    #[fail(display = "unsupported operation on this toolchain")]
    UnsupportedOperation,
}

/// Metadata of a dist toolchain. See [`Toolchain`](struct.Toolchain.html) to create and get it.
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Hash, Debug, Clone)]
pub struct DistToolchain {
    name: Cow<'static, str>,
}

impl DistToolchain {
    /// Get the name of this toolchain.
    pub fn name(&self) -> &str {
        self.name.as_ref()
    }

    fn init(&self, workspace: &Workspace) -> Result<(), Error> {
        info!("installing toolchain {}", self.name());
        Command::new(workspace, &RUSTUP)
            .args(&[
                "toolchain",
                "install",
                self.name(),
                "--profile",
                workspace.rustup_profile(),
            ])
            .run()
            .with_context(|_| format!("unable to install toolchain {} via rustup", self.name()))?;

        Ok(())
    }
}

#[derive(Copy, Clone)]
enum RustupAction {
    Add,
    Remove,
}

impl std::fmt::Display for RustupAction {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Add => "add",
                Self::Remove => "remove",
            }
        )
    }
}

#[derive(Copy, Clone)]
enum RustupThing {
    Target,
    Component,
}

impl std::fmt::Display for RustupThing {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Target => "target",
                Self::Component => "component",
            }
        )
    }
}

/// Metadata of a CI toolchain. See [`Toolchain`](struct.Toolchain.html) to create and get it.
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Hash, Debug, Clone)]
#[cfg(any(feature = "unstable-toolchain-ci", doc))]
#[cfg_attr(docs_rs, doc(cfg(feature = "unstable-toolchain-ci")))]
pub struct CiToolchain {
    /// Hash of the merge commit of the PR you want to download.
    sha: String,
    /// Whether you want to download a standard or "alt" build. "alt" builds have extra
    /// compiler assertions enabled.
    alt: bool,
}

#[cfg(any(feature = "unstable-toolchain-ci", doc))]
impl CiToolchain {
    /// Get the SHA of the git commit that produced this toolchain.
    pub fn sha(&self) -> &str {
        &self.sha
    }

    /// Check whether this is a normal CI artifact or an alternate CI artifact.
    ///
    /// Alternate CI artifacts are artifacts with extra assertions or features, produced by the Rust
    /// team mostly for internal usage. The difference between them and normal CI artifacts can
    /// change over time.
    pub fn is_alt(&self) -> bool {
        self.alt
    }

    fn init(&self, workspace: &Workspace) -> Result<(), Error> {
        if self.alt {
            info!("installing toolchain {}-alt", self.sha);
        } else {
            info!("installing toolchain {}", self.sha);
        }

        let mut args = vec![self.sha(), "-c", "cargo"];
        if self.alt {
            args.push("--alt");
        }

        Command::new(workspace, &RUSTUP_TOOLCHAIN_INSTALL_MASTER)
            .args(&args)
            .run()
            .with_context(|_| {
                format!(
                    "unable to install toolchain {} via rustup-toolchain-install-master",
                    self.sha
                )
            })?;

        Ok(())
    }
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Hash, Debug, Clone)]
#[serde(rename_all = "kebab-case", tag = "type")]
enum ToolchainInner {
    Dist(DistToolchain),
    #[serde(rename = "ci")]
    #[cfg(feature = "unstable-toolchain-ci")]
    CI(CiToolchain),
}

/// Representation of a Rust compiler toolchain.
///
/// The `Toolchain` struct represents a compiler toolchain, either downloaded from rustup or from
/// the [rust-lang/rust][rustc] repo's CI artifacts storage, and it provides the methods to install
/// and use it.
///
/// [rustc]: https://github.com/rust-lang/rust
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Hash, Debug, Clone)]
pub struct Toolchain {
    #[serde(flatten)]
    inner: ToolchainInner,
}

impl Toolchain {
    pub(crate) const MAIN: Toolchain = Toolchain {
        inner: ToolchainInner::Dist(DistToolchain {
            name: Cow::Borrowed(MAIN_TOOLCHAIN_NAME),
        }),
    };

    /// Returns whether or not this toolchain is needed by rustwide itself.
    ///
    /// This toolchain is used for doing things like installing tools.
    ///
    /// ```rust
    /// # use rustwide::Toolchain;
    /// let tc = Toolchain::dist("stable-x86_64-unknown-linux-gnu");
    /// assert!(tc.is_needed_by_rustwide());
    /// let tc = Toolchain::dist("nightly-x86_64-unknown-linux-gnu");
    /// assert!(!tc.is_needed_by_rustwide());
    /// ```
    pub fn is_needed_by_rustwide(&self) -> bool {
        match &self.inner {
            ToolchainInner::Dist(dist) => dist.name.starts_with(MAIN_TOOLCHAIN_NAME),
            #[cfg(feature = "unstable-toolchain-ci")]
            _ => false,
        }
    }

    /// Create a new dist toolchain.
    ///
    /// Dist toolchains are all the toolchains available through rustup and distributed from
    /// [static.rust-lang.org][static-rlo]. You need to provide the toolchain name (the same you'd
    /// use to install that toolchain with rustup).
    ///
    /// [static-rlo]: https://static.rust-lang.org
    pub fn dist(name: &str) -> Self {
        Toolchain {
            inner: ToolchainInner::Dist(DistToolchain {
                name: Cow::Owned(name.into()),
            }),
        }
    }

    /// Create a new CI toolchain.
    ///
    /// CI toolchains are artifacts built for every merged PR in the [rust-lang/rust][repo]
    /// repository, identified by the SHA of the merge commit. These builds are purged after a
    /// couple of months, and are available both in normal mode and "alternate" mode (experimental
    /// builds with extra debugging and testing features enabled).
    ///
    /// **There is no availability or stability guarantee for these builds!**
    ///
    /// [repo]: https://github.com/rust-lang/rust
    #[cfg(any(feature = "unstable-toolchain-ci", doc))]
    #[cfg_attr(docs_rs, doc(cfg(feature = "unstable-toolchain-ci")))]
    pub fn ci(sha: &str, alt: bool) -> Self {
        Toolchain {
            inner: ToolchainInner::CI(CiToolchain {
                sha: sha.to_string(),
                alt,
            }),
        }
    }

    /// If this toolchain is a dist toolchain, return its metadata.
    #[allow(irrefutable_let_patterns)]
    pub fn as_dist(&self) -> Option<&DistToolchain> {
        if let ToolchainInner::Dist(dist) = &self.inner {
            Some(dist)
        } else {
            None
        }
    }

    /// If this toolchain is a CI toolchain, return its metadata.
    #[cfg(any(feature = "unstable-toolchain-ci", doc))]
    #[cfg_attr(docs_rs, doc(cfg(feature = "unstable-toolchain-ci")))]
    pub fn as_ci(&self) -> Option<&CiToolchain> {
        if let ToolchainInner::CI(ci) = &self.inner {
            Some(ci)
        } else {
            None
        }
    }

    /// Download and install the toolchain.
    pub fn install(&self, workspace: &Workspace) -> Result<(), Error> {
        match &self.inner {
            ToolchainInner::Dist(dist) => dist.init(workspace)?,
            #[cfg(feature = "unstable-toolchain-ci")]
            ToolchainInner::CI(ci) => ci.init(workspace)?,
        }

        Ok(())
    }

    /// Download and install a component for the toolchain.
    pub fn add_component(&self, workspace: &Workspace, name: &str) -> Result<(), Error> {
        self.change_rustup_thing(workspace, RustupAction::Add, RustupThing::Component, name)
    }

    /// Remove a component already installed for the toolchain.
    pub fn remove_component(&self, workspace: &Workspace, name: &str) -> Result<(), Error> {
        self.change_rustup_thing(
            workspace,
            RustupAction::Remove,
            RustupThing::Component,
            name,
        )
    }

    /// Download and install a target for the toolchain.
    ///
    /// If the toolchain is not installed in the workspace an error will be returned. This is only
    /// supported for dist toolchains.
    pub fn add_target(&self, workspace: &Workspace, name: &str) -> Result<(), Error> {
        self.change_rustup_thing(workspace, RustupAction::Add, RustupThing::Target, name)
    }

    /// Remove a target already installed for the toolchain.
    ///
    /// If the toolchain is not installed in the workspace or the target is missing an error will
    /// be returned. This is only supported for dist toolchains.
    pub fn remove_target(&self, workspace: &Workspace, name: &str) -> Result<(), Error> {
        self.change_rustup_thing(workspace, RustupAction::Remove, RustupThing::Target, name)
    }

    /// Return a list of installed targets for this toolchain.
    ///
    /// If the toolchain is not installed an empty list is returned.
    pub fn installed_targets(&self, workspace: &Workspace) -> Result<Vec<String>, Error> {
        self.list_rustup_things(workspace, RustupThing::Target)
    }

    fn change_rustup_thing(
        &self,
        workspace: &Workspace,
        action: RustupAction,
        thing: RustupThing,
        name: &str,
    ) -> Result<(), Error> {
        let (log_action, log_action_ing) = match action {
            RustupAction::Add => ("add", "adding"),
            RustupAction::Remove => ("remove", "removing"),
        };

        let thing = thing.to_string();
        let action = action.to_string();

        #[cfg(feature = "unstable-toolchain-ci")]
        if let ToolchainInner::CI { .. } = self.inner {
            failure::bail!(
                "{} {} on CI toolchains is not supported yet",
                log_action_ing,
                thing
            );
        }

        let toolchain_name = self.rustup_name();
        info!(
            "{} {} {} for toolchain {}",
            log_action_ing, thing, name, toolchain_name
        );

        Command::new(workspace, &RUSTUP)
            .args(&[
                thing.as_str(),
                action.as_str(),
                "--toolchain",
                &toolchain_name,
                name,
            ])
            .run()
            .with_context(|_| {
                format!(
                    "unable to {} {} {} for toolchain {} via rustup",
                    log_action, thing, name, toolchain_name,
                )
            })?;
        Ok(())
    }

    fn list_rustup_things(
        &self,
        workspace: &Workspace,
        thing: RustupThing,
    ) -> Result<Vec<String>, Error> {
        let thing = thing.to_string();
        let name = if let Some(dist) = self.as_dist() {
            dist.name()
        } else {
            return Err(ToolchainError::UnsupportedOperation.into());
        };

        let mut not_installed = false;
        let result = Command::new(workspace, &RUSTUP)
            .args(&[thing.as_str(), "list", "--installed", "--toolchain", name])
            .log_output(false)
            .process_lines(&mut |line, _| {
                if line.starts_with("error: toolchain ") && line.ends_with(" is not installed") {
                    not_installed = true;
                }
            })
            .run_capture();

        match result {
            Ok(out) => Ok(out
                .stdout_lines()
                .iter()
                .filter(|line| !line.is_empty())
                .cloned()
                .collect()),
            Err(_) if not_installed => Err(ToolchainError::NotInstalled.into()),
            Err(err) => Err(Error::from(err)
                .context(format!(
                    "failed to read the list of installed {}s for {} with rustup",
                    thing, name
                ))
                .into()),
        }
    }

    /// Remove the toolchain from the rustwide workspace, freeing up disk space.
    pub fn uninstall(&self, workspace: &Workspace) -> Result<(), Error> {
        let name = self.rustup_name();
        Command::new(workspace, &RUSTUP)
            .args(&["toolchain", "uninstall", &name])
            .run()
            .with_context(|_| format!("unable to uninstall toolchain {} via rustup", name))?;
        Ok(())
    }

    /// Return a runnable object configured to run `cargo` with this toolchain. This method is
    /// intended to be used with [`rustwide::cmd::Command`](cmd/struct.Command.html).
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use rustwide::{WorkspaceBuilder, Toolchain, cmd::Command};
    /// # use std::error::Error;
    /// # fn main() -> Result<(), Box<dyn Error>> {
    /// # let workspace = WorkspaceBuilder::new("".as_ref(), "").init()?;
    /// let toolchain = Toolchain::dist("beta");
    /// Command::new(&workspace, toolchain.cargo())
    ///     .args(&["check"])
    ///     .run()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn cargo(&self) -> impl Runnable + '_ {
        self.rustup_binary("cargo")
    }

    /// Return a runnable object configured to run `rustc` with this toolchain. This method is
    /// intended to be used with [`rustwide::cmd::Command`](cmd/struct.Command.html).
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use rustwide::{WorkspaceBuilder, Toolchain, cmd::Command};
    /// # use std::error::Error;
    /// # fn main() -> Result<(), Box<dyn Error>> {
    /// # let workspace = WorkspaceBuilder::new("".as_ref(), "").init()?;
    /// let toolchain = Toolchain::dist("beta");
    /// Command::new(&workspace, toolchain.rustc())
    ///     .args(&["hello.rs"])
    ///     .run()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn rustc(&self) -> impl Runnable + '_ {
        self.rustup_binary("rustc")
    }

    /// Return a runnable object configured to run `name` with this toolchain. This method is
    /// intended to be used with [`rustwide::cmd::Command`](cmd/struct.Command.html).
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use rustwide::{WorkspaceBuilder, Toolchain, cmd::Command};
    /// # use std::error::Error;
    /// # fn main() -> Result<(), Box<dyn Error>> {
    /// # let workspace = WorkspaceBuilder::new("".as_ref(), "").init()?;
    /// let toolchain = Toolchain::dist("beta");
    /// Command::new(&workspace, toolchain.rustup_binary("rustdoc"))
    ///     .args(&["hello.rs"])
    ///     .run()?;
    /// # Ok(())
    /// # }
    pub fn rustup_binary(&self, name: &'static str) -> impl Runnable + '_ {
        RustupProxy {
            toolchain: self,
            name,
        }
    }

    fn rustup_name(&self) -> String {
        match &self.inner {
            ToolchainInner::Dist(dist) => dist.name.to_string(),
            #[cfg(feature = "unstable-toolchain-ci")]
            ToolchainInner::CI(ci) if ci.alt => format!("{}-alt", ci.sha),
            #[cfg(feature = "unstable-toolchain-ci")]
            ToolchainInner::CI(ci) => ci.sha.to_string(),
        }
    }
}

impl std::fmt::Display for Toolchain {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.rustup_name())
    }
}

struct RustupProxy<'a> {
    toolchain: &'a Toolchain,
    name: &'static str,
}

impl Runnable for RustupProxy<'_> {
    fn name(&self) -> Binary {
        Binary::ManagedByRustwide(self.name.into())
    }

    fn prepare_command<'w, 'pl>(&self, cmd: Command<'w, 'pl>) -> Command<'w, 'pl> {
        cmd.args(&[format!("+{}", self.toolchain.rustup_name())])
    }
}

pub(crate) fn list_installed_toolchains(rustup_home: &Path) -> Result<Vec<Toolchain>, Error> {
    let update_hashes = rustup_home.join("update-hashes");

    let mut result = Vec::new();
    for entry in std::fs::read_dir(&rustup_home.join("toolchains"))? {
        let entry = entry?;
        let name = entry
            .file_name()
            .to_str()
            .ok_or_else(|| failure::err_msg("non-utf8 toolchain name"))?
            .to_string();
        // A toolchain installed by rustup has a corresponding file in $RUSTUP_HOME/update-hashes
        // A toolchain linked by rustup is just a symlink
        if entry.file_type()?.is_symlink() || update_hashes.join(&name).exists() {
            result.push(Toolchain::dist(&name));
        } else {
            #[cfg(feature = "unstable-toolchain-ci")]
            {
                let (sha, alt) = if name.ends_with("-alt") {
                    ((&name[..name.len() - 4]).to_string(), true)
                } else {
                    (name, false)
                };
                result.push(Toolchain::ci(&sha, alt));
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::Toolchain;
    use failure::Error;

    #[test]
    fn test_dist_serde_repr() -> Result<(), Error> {
        const DIST: &str = r#"{"type": "dist", "name": "stable"}"#;

        assert_eq!(Toolchain::dist("stable"), serde_json::from_str(DIST)?);

        Ok(())
    }

    #[test]
    #[cfg(feature = "unstable-toolchain-ci")]
    fn test_ci_serde_repr() -> Result<(), Error> {
        const CI_NORMAL: &str = r#"{"type": "ci", "sha": "0000000", "alt": false}"#;
        const CI_ALT: &str = r#"{"type": "ci", "sha": "0000000", "alt": true}"#;

        assert_eq!(
            Toolchain::ci("0000000", false),
            serde_json::from_str(CI_NORMAL)?
        );
        assert_eq!(
            Toolchain::ci("0000000", true),
            serde_json::from_str(CI_ALT)?
        );

        Ok(())
    }

    #[test]
    fn test_list_installed() -> Result<(), Error> {
        const DIST_NAME: &str = "stable-x86_64-unknown-linux-gnu";
        const LINK_NAME: &str = "stage1";
        const CI_SHA: &str = "0000000000000000000000000000000000000000";

        let rustup_home = tempfile::tempdir()?;

        // Create a fake rustup-installed toolchain
        std::fs::create_dir_all(rustup_home.path().join("toolchains").join(DIST_NAME))?;
        std::fs::create_dir_all(rustup_home.path().join("update-hashes"))?;
        std::fs::write(
            rustup_home.path().join("update-hashes").join(DIST_NAME),
            &[],
        )?;

        // Create a fake symlinked toolchain
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            "/dev/null",
            rustup_home.path().join("toolchains").join(LINK_NAME),
        )?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(
            "NUL",
            rustup_home.path().join("toolchains").join(LINK_NAME),
        )?;

        // Create a standard CI toolchain
        std::fs::create_dir_all(rustup_home.path().join("toolchains").join(CI_SHA))?;

        // Create an alt CI toolchain
        std::fs::create_dir_all(
            rustup_home
                .path()
                .join("toolchains")
                .join(format!("{}-alt", CI_SHA)),
        )?;

        let res = super::list_installed_toolchains(rustup_home.path())?;

        let mut expected_count = 0;

        assert!(res.contains(&Toolchain::dist(DIST_NAME)));
        assert!(res.contains(&Toolchain::dist(LINK_NAME)));
        expected_count += 2;

        #[cfg(feature = "unstable-toolchain-ci")]
        {
            assert!(res.contains(&Toolchain::ci(CI_SHA, false)));
            assert!(res.contains(&Toolchain::ci(CI_SHA, true)));
            expected_count += 2;
        }

        assert_eq!(res.len(), expected_count);

        Ok(())
    }
}
