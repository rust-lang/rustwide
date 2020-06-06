use crate::cmd::{Command, MountKind, Runnable, SandboxBuilder};
use crate::prepare::Prepare;
use crate::{Crate, Toolchain, Workspace};
use failure::Error;
use remove_dir_all::remove_dir_all;
use std::path::PathBuf;
use std::vec::Vec;

#[derive(Clone)]
pub(crate) struct CratePatch {
    pub(crate) name: String,
    pub(crate) uri: String,
    pub(crate) branch: String,
}

/// Directory in the [`Workspace`](struct.Workspace.html) where builds can be executed.
///
/// The build directory contains the source code of the crate being built and the target directory
/// used by cargo to store build artifacts. If multiple builds are executed in the same build
/// directory they will share the target directory.
pub struct BuildDirectory {
    workspace: Workspace,
    name: String,
    purge_on_drop: bool,
}

/// Builder for configuring builds in a [`BuildDirectory`](struct.BuildDirectory.html).
pub struct BuildBuilder<'a> {
    build_dir: &'a mut BuildDirectory,
    toolchain: &'a Toolchain,
    krate: &'a Crate,
    sandbox: SandboxBuilder,
    patches: Vec<CratePatch>,
}

impl<'a> BuildBuilder<'a> {
    /// Add a patch to this build.
    /// Patches get added to the crate's Cargo.toml in the `patch.crates-io` table.
    /// # Example
    ///
    /// ```no_run
    /// # use rustwide::{WorkspaceBuilder, Toolchain, Crate, cmd::SandboxBuilder};
    /// # use std::error::Error;
    /// # fn main() -> Result<(), Box<dyn Error>> {
    /// # let workspace = WorkspaceBuilder::new("".as_ref(), "").init()?;
    /// # let toolchain = Toolchain::dist("");
    /// # let krate = Crate::local("".as_ref());
    /// # let sandbox = SandboxBuilder::new();
    /// let mut build_dir = workspace.build_dir("foo");
    /// build_dir.build(&toolchain, &krate, sandbox)
    ///     .patch_with_git("bar", "https://github.com/foo/bar", "baz")
    ///     .run(|build| {
    ///         build.cargo().args(&["test", "--all"]).run()?;
    ///         Ok(())
    ///     })?;
    /// # Ok(())
    /// # }
    pub fn patch_with_git(mut self, name: &str, uri: &str, branch: &str) -> Self {
        self.patches.push(CratePatch {
            name: name.into(),
            uri: uri.into(),
            branch: branch.into(),
        });
        self
    }

    /// Run a sandboxed build of the provided crate with the provided toolchain. The closure will
    /// be provided an instance of [`Build`](struct.Build.html) that allows spawning new processes
    /// inside the sandbox.
    ///
    /// All the state will be kept on disk as long as the closure doesn't exit: after that things
    /// might be removed.
    /// # Example
    ///
    /// ```no_run
    /// # use rustwide::{WorkspaceBuilder, Toolchain, Crate, cmd::SandboxBuilder};
    /// # use std::error::Error;
    /// # fn main() -> Result<(), Box<dyn Error>> {
    /// # let workspace = WorkspaceBuilder::new("".as_ref(), "").init()?;
    /// # let toolchain = Toolchain::dist("");
    /// # let krate = Crate::local("".as_ref());
    /// # let sandbox = SandboxBuilder::new();
    /// let mut build_dir = workspace.build_dir("foo");
    /// build_dir.build(&toolchain, &krate, sandbox).run(|build| {
    ///     build.cargo().args(&["test", "--all"]).run()?;
    ///     Ok(())
    /// })?;
    /// # Ok(())
    /// # }
    pub fn run<R, F: FnOnce(&Build) -> Result<R, Error>>(self, f: F) -> Result<R, Error> {
        self.build_dir
            .run(self.toolchain, self.krate, self.sandbox, self.patches, f)
    }
}

impl BuildDirectory {
    pub(crate) fn new(workspace: Workspace, name: &str) -> Self {
        Self {
            workspace,
            name: name.into(),
            purge_on_drop: false,
        }
    }

    /// Create a build in this build directory.  Returns a builder that can be used
    /// to configure the build and run it.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use rustwide::{WorkspaceBuilder, Toolchain, Crate, cmd::SandboxBuilder};
    /// # use std::error::Error;
    /// # fn main() -> Result<(), Box<dyn Error>> {
    /// # let workspace = WorkspaceBuilder::new("".as_ref(), "").init()?;
    /// # let toolchain = Toolchain::dist("");
    /// # let krate = Crate::local("".as_ref());
    /// # let sandbox = SandboxBuilder::new();
    /// let mut build_dir = workspace.build_dir("foo");
    /// build_dir.build(&toolchain, &krate, sandbox).run(|build| {
    ///     build.cargo().args(&["test", "--all"]).run()?;
    ///     Ok(())
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn build<'a>(
        &'a mut self,
        toolchain: &'a Toolchain,
        krate: &'a Crate,
        sandbox: SandboxBuilder,
    ) -> BuildBuilder {
        BuildBuilder {
            build_dir: self,
            toolchain,
            krate,
            sandbox,
            patches: Vec::new(),
        }
    }

    pub(crate) fn run<R, F: FnOnce(&Build) -> Result<R, Error>>(
        &mut self,
        toolchain: &Toolchain,
        krate: &Crate,
        sandbox: SandboxBuilder,
        patches: Vec<CratePatch>,
        f: F,
    ) -> Result<R, Error> {
        let source_dir = self.source_dir();
        if source_dir.exists() {
            remove_dir_all(&source_dir)?;
        }

        let mut prepare = Prepare::new(&self.workspace, toolchain, krate, &source_dir, patches);
        prepare.prepare()?;

        std::fs::create_dir_all(self.target_dir())?;
        let res = f(&Build {
            dir: self,
            toolchain,
            sandbox,
        })?;

        remove_dir_all(&source_dir)?;
        Ok(res)
    }

    /// Remove all the contents of the build directory, freeing disk space.
    pub fn purge(&mut self) -> Result<(), Error> {
        let build_dir = self.build_dir();
        if build_dir.exists() {
            remove_dir_all(build_dir)?;
        }
        Ok(())
    }

    /// Enable or disable purging the build directory when dropped.
    ///
    /// By default, the directory is not purged.
    pub fn purge_on_drop(&mut self, yes: bool) {
        self.purge_on_drop = yes;
    }

    fn build_dir(&self) -> PathBuf {
        self.workspace.builds_dir().join(&self.name)
    }

    fn source_dir(&self) -> PathBuf {
        self.build_dir().join("source")
    }

    fn target_dir(&self) -> PathBuf {
        self.build_dir().join("target")
    }
}

impl Drop for BuildDirectory {
    fn drop(&mut self) {
        if self.purge_on_drop {
            if let Err(err) = self.purge() {
                log::error!("failed to delete build directory: {}", err);
            }
        }
    }
}

/// API to interact with a running build.
///
/// This is created from [`BuildDirectory::build`](struct.BuildDirectory.html#method.build)
pub struct Build<'b> {
    dir: &'b BuildDirectory,
    toolchain: &'b Toolchain,
    sandbox: SandboxBuilder,
}

impl Build<'_> {
    /// Run a command inside the sandbox.
    ///
    /// Any `cargo` invocation will automatically be configured to use a target directory mounted
    /// outside the sandbox. The crate's source directory will be the working directory for the
    /// command.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use rustwide::{WorkspaceBuilder, Toolchain, Crate, cmd::SandboxBuilder};
    /// # use std::error::Error;
    /// # fn main() -> Result<(), Box<dyn Error>> {
    /// # let workspace = WorkspaceBuilder::new("".as_ref(), "").init()?;
    /// # let toolchain = Toolchain::dist("");
    /// # let krate = Crate::local("".as_ref());
    /// # let sandbox = SandboxBuilder::new();
    /// let mut build_dir = workspace.build_dir("foo");
    /// build_dir.build(&toolchain, &krate, sandbox).run(|build| {
    ///     build.cmd("rustfmt").args(&["--check", "src/main.rs"]).run()?;
    ///     Ok(())
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn cmd<R: Runnable>(&self, bin: R) -> Command {
        let container_dir = &*crate::cmd::container_dirs::TARGET_DIR;

        Command::new_sandboxed(
            &self.dir.workspace,
            self.sandbox
                .clone()
                .mount(&self.dir.target_dir(), container_dir, MountKind::ReadWrite),
            bin,
        )
        .cd(self.dir.source_dir())
        .env("CARGO_TARGET_DIR", container_dir)
    }

    /// Run `cargo` inside the sandbox, using the toolchain chosen for the build.
    ///
    /// `cargo` will automatically be configured to use a target directory mounted outside the
    /// sandbox. The crate's source directory will be the working directory for the command.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use rustwide::{WorkspaceBuilder, Toolchain, Crate, cmd::SandboxBuilder};
    /// # use std::error::Error;
    /// # fn main() -> Result<(), Box<dyn Error>> {
    /// # let workspace = WorkspaceBuilder::new("".as_ref(), "").init()?;
    /// # let toolchain = Toolchain::dist("");
    /// # let krate = Crate::local("".as_ref());
    /// # let sandbox = SandboxBuilder::new();
    /// let mut build_dir = workspace.build_dir("foo");
    /// build_dir.build(&toolchain, &krate, sandbox).run(|build| {
    ///     build.cargo().args(&["test", "--all"]).run()?;
    ///     Ok(())
    /// })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn cargo(&self) -> Command {
        self.cmd(self.toolchain.cargo())
    }

    /// Get the path to the source code on the host machine (outside the sandbox).
    pub fn host_source_dir(&self) -> PathBuf {
        self.dir.source_dir()
    }

    /// Get the path to the target directory on the host machine (outside the sandbox).
    pub fn host_target_dir(&self) -> PathBuf {
        self.dir.target_dir()
    }
}
