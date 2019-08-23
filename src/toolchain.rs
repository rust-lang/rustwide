use crate::cmd::{Binary, Command, Runnable};
use crate::tools::{RUSTUP, RUSTUP_TOOLCHAIN_INSTALL_MASTER};
use crate::Workspace;
use failure::{bail, Error, ResultExt};
use log::info;
use std::borrow::Cow;
use std::path::Path;

pub(crate) const MAIN_TOOLCHAIN_NAME: &str = "stable";

/// Representation of a Rust compiler toolchain.
///
/// The `Toolchain` enum represents a compiler toolchain, either downloaded from rustup or from the
/// [rust-lang/rust][rustc] repo's CI artifacts storage. and it provides the tool to install and use it.
///
/// [rustc]: https://github.com/rust-lang/rust
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Hash, Debug, Clone)]
#[serde(rename_all = "kebab-case", tag = "type")]
pub enum Toolchain {
    /// Toolchain available through rustup and distributed from
    /// [static.rust-lang.org](https://static.rust-lang.org).
    Dist {
        /// The name of the toolchain, which is the same you'd use with `rustup toolchain install
        /// <name>`.
        name: Cow<'static, str>,
    },
    /// CI artifact from the [rust-lang/rust] repo. Each merged PR has its own full build
    /// available for a while after it's been merged, identified by the merge commit sha. **There
    /// is no retention or stability guarantee for these builds**.
    ///
    /// [rust-lang/rust]: https://github.com/rust-lang/rust
    #[serde(rename = "ci")]
    CI {
        /// Hash of the merge commit of the PR you want to download.
        sha: Cow<'static, str>,
        /// Whether you want to download a standard or "alt" build. "alt" builds have extra
        /// compiler assertions enabled.
        alt: bool,
    },
    #[doc(hidden)]
    __NonExaustive,
}

impl Toolchain {
    pub(crate) const MAIN: Toolchain = Toolchain::Dist {
        name: Cow::Borrowed(MAIN_TOOLCHAIN_NAME),
    };

    /// Download and install the toolchain.
    pub fn install(&self, workspace: &Workspace) -> Result<(), Error> {
        match self {
            Self::Dist { name } => init_toolchain_from_dist(workspace, name)?,
            Self::CI { sha, alt } => init_toolchain_from_ci(workspace, *alt, sha)?,
            Self::__NonExaustive => panic!("do not create __NonExaustive variants manually"),
        }

        Ok(())
    }

    /// Download and install a component for the toolchain.
    pub fn add_component(&self, workspace: &Workspace, name: &str) -> Result<(), Error> {
        self.add_rustup_thing(workspace, "component", name)
    }

    /// Download and install a target for the toolchain.
    pub fn add_target(&self, workspace: &Workspace, name: &str) -> Result<(), Error> {
        self.add_rustup_thing(workspace, "target", name)
    }

    fn add_rustup_thing(
        &self,
        workspace: &Workspace,
        thing: &str,
        name: &str,
    ) -> Result<(), Error> {
        if let Self::CI { .. } = self {
            bail!("installing {} on CI toolchains is not supported yet", thing);
        }
        let toolchain_name = self.rustup_name();
        info!(
            "installing {} {} for toolchain {}",
            thing, name, toolchain_name
        );

        Command::new(workspace, &RUSTUP)
            .args(&[thing, "add", "--toolchain", &toolchain_name, name])
            .run()
            .with_context(|_| {
                format!(
                    "unable to install {} {} for toolchain {} via rustup",
                    thing, name, toolchain_name,
                )
            })?;
        Ok(())
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
    /// let toolchain = Toolchain::Dist { name: "beta".into() };
    /// Command::new(&workspace, toolchain.cargo())
    ///     .args(&["check"])
    ///     .run()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn cargo<'a>(&'a self) -> impl Runnable + 'a {
        struct CargoBin<'a>(&'a Toolchain);

        impl Runnable for CargoBin<'_> {
            fn name(&self) -> Binary {
                Binary::ManagedByRustwide("cargo".into())
            }

            fn prepare_command<'w, 'pl>(&self, cmd: Command<'w, 'pl>) -> Command<'w, 'pl> {
                cmd.args(&[format!("+{}", self.0.rustup_name())])
            }
        }

        CargoBin(self)
    }

    fn rustup_name(&self) -> String {
        match self {
            Self::Dist { name } => name.to_string(),
            Self::CI { sha, alt: false } => sha.to_string(),
            Self::CI { sha, alt: true } => format!("{}-alt", sha),
            Self::__NonExaustive => panic!("do not create __NonExaustive variants manually"),
        }
    }
}

impl std::fmt::Display for Toolchain {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.rustup_name())
    }
}

fn init_toolchain_from_dist(workspace: &Workspace, toolchain: &str) -> Result<(), Error> {
    info!("installing toolchain {}", toolchain);
    Command::new(workspace, &RUSTUP)
        .args(&["toolchain", "install", toolchain])
        .run()
        .with_context(|_| format!("unable to install toolchain {} via rustup", toolchain))?;

    Ok(())
}

fn init_toolchain_from_ci(workspace: &Workspace, alt: bool, sha: &str) -> Result<(), Error> {
    if alt {
        info!("installing toolchain {}-alt", sha);
    } else {
        info!("installing toolchain {}", sha);
    }

    let mut args = vec![sha, "-c", "cargo"];
    if alt {
        args.push("--alt");
    }

    Command::new(workspace, &RUSTUP_TOOLCHAIN_INSTALL_MASTER)
        .args(&args)
        .run()
        .with_context(|_| {
            format!(
                "unable to install toolchain {} via rustup-toolchain-install-master",
                sha
            )
        })?;

    Ok(())
}

pub(crate) fn list_installed(rustup_home: &Path) -> Result<Vec<Toolchain>, Error> {
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
            result.push(Toolchain::Dist {
                name: Cow::Owned(name),
            });
        } else {
            let (sha, alt) = if name.ends_with("-alt") {
                ((&name[..name.len() - 4]).to_string(), true)
            } else {
                (name, false)
            };
            result.push(Toolchain::CI {
                sha: Cow::Owned(sha),
                alt,
            });
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::Toolchain;
    use failure::Error;

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

        let res = super::list_installed(rustup_home.path())?;
        assert_eq!(4, res.len());
        assert!(res.contains(&Toolchain::Dist {
            name: DIST_NAME.into()
        }));
        assert!(res.contains(&Toolchain::Dist {
            name: LINK_NAME.into()
        }));
        assert!(res.contains(&Toolchain::CI {
            sha: CI_SHA.into(),
            alt: false
        }));
        assert!(res.contains(&Toolchain::CI {
            sha: CI_SHA.into(),
            alt: true
        }));

        Ok(())
    }
}
