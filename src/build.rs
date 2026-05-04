use crate::{
    Crate, PrepareError, Toolchain, Workspace,
    cmd::{Command, Runnable, Sandbox, SandboxBuilder, container_dirs},
    prepare::Prepare,
};
use std::mem;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::vec::Vec;
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
pub(crate) enum CratePatch {
    Git(GitCratePatch),
    Path(PathCratePatch),
}

#[derive(Clone)]
pub(crate) struct GitCratePatch {
    pub(crate) name: String,
    pub(crate) uri: String,
    pub(crate) branch: String,
}

#[derive(Clone)]
pub(crate) struct PathCratePatch {
    pub(crate) name: String,
    pub(crate) path: String,
}

/// Directory in the [`Workspace`](struct.Workspace.html) where builds can be executed.
///
/// The build directory contains the source code of the crate being built and the target directory
/// used by cargo to store build artifacts. If multiple builds are executed in the same build
/// directory they will share the target directory.
pub struct BuildDirectory {
    workspace: Workspace,
    name: String,
}

/// Builder for configuring builds in a [`BuildDirectory`](struct.BuildDirectory.html).
pub struct BuildBuilder<'a> {
    build_dir: &'a mut BuildDirectory,
    toolchain: &'a Toolchain,
    krate: &'a Crate,
    sandbox: SandboxBuilder,
    patches: Vec<CratePatch>,
}

/// Statistics collected for a sandboxed build.
///
/// These metrics describe the sandbox as a whole across the build, not an
/// individual command invocation.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SandboxStatistics {
    memory_peak: Option<u64>,
}

impl SandboxStatistics {
    /// Return the peak memory usage in bytes observed across the whole build, if available.
    pub fn memory_peak_bytes(&self) -> Option<u64> {
        self.memory_peak
    }

    /// Merge two `SandboxStatistics` into one, keeping the highest observed peak memory.
    pub fn merge(self, other: Self) -> Self {
        Self {
            memory_peak: match (self.memory_peak, other.memory_peak) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            },
        }
    }

    /// Merge another `SandboxStatistics` into `self` in place.
    pub fn merge_mut(&mut self, other: Self) {
        *self = mem::take(self).merge(other);
    }
}

/// Output of a completed build together with build-level statistics.
pub struct BuildResult<T> {
    output: T,
    statistics: SandboxStatistics,
}

impl<T> BuildResult<T> {
    /// Return the wrapped build output.
    pub fn into_inner(self) -> T {
        self.output
    }

    /// Borrow the build-level statistics.
    pub fn statistics(&self) -> &SandboxStatistics {
        &self.statistics
    }

    /// Return the peak memory usage in bytes observed across the whole build, if available.
    pub fn memory_peak_bytes(&self) -> Option<u64> {
        self.statistics.memory_peak_bytes()
    }
}

impl<T> Deref for BuildResult<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.output
    }
}

impl<T> DerefMut for BuildResult<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.output
    }
}

#[cfg(test)]
mod tests {
    use super::SandboxStatistics;
    use test_case::test_case;

    const fn stats(peak: Option<u64>) -> SandboxStatistics {
        SandboxStatistics { memory_peak: peak }
    }

    #[test_case(stats(None), stats(None), stats(None))]
    #[test_case(stats(Some(100)), stats(None), stats(Some(100)))]
    #[test_case(stats(None), stats(Some(100)), stats(Some(100)))]
    #[test_case(stats(Some(300)), stats(Some(100)), stats(Some(300)))]
    #[test_case(stats(Some(100)), stats(Some(300)), stats(Some(300)))]
    #[test_case(stats(Some(42)), stats(Some(42)), stats(Some(42)))]
    fn test_merge(lhs: SandboxStatistics, rhs: SandboxStatistics, expected: SandboxStatistics) {
        {
            let lhs = lhs.clone();
            let rhs = rhs.clone();
            assert_eq!(lhs.merge(rhs), expected);
        }

        {
            let mut lhs = lhs.clone();
            lhs.merge_mut(rhs);
            assert_eq!(lhs, expected);
        }
    }

    #[test]
    fn merge_mut_accumulate_over_multiple() {
        let mut s = stats(None);
        s.merge_mut(stats(Some(50)));
        s.merge_mut(stats(Some(200)));
        s.merge_mut(stats(None));
        s.merge_mut(stats(Some(150)));
        assert_eq!(s.memory_peak, Some(200));
    }
}

impl BuildBuilder<'_> {
    /// Add a git-based patch to this build.
    /// Patches get added to the crate's Cargo.toml in the `patch.crates-io` table.
    /// # Example
    ///
    /// ```no_run
    /// # use rustwide::{WorkspaceBuilder, Toolchain, Crate, cmd::SandboxBuilder};
    /// # use std::error::Error;
    /// # fn main() -> anyhow::Result<(), Box<dyn Error>> {
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
        self.patches.push(CratePatch::Git(GitCratePatch {
            name: name.into(),
            uri: uri.into(),
            branch: branch.into(),
        }));
        self
    }

    /// Add a path-based patch to this build.
    /// Patches get added to the crate's Cargo.toml in the `patch.crates-io` table.
    /// # Example
    ///
    /// ```no_run
    /// # use rustwide::{WorkspaceBuilder, Toolchain, Crate, cmd::{MountKind, SandboxBuilder}};
    /// # use std::{error::Error, path::{Path, PathBuf}};
    /// # fn main() -> anyhow::Result<(), Box<dyn Error>> {
    /// # let workspace = WorkspaceBuilder::new("".as_ref(), "").init()?;
    /// # let toolchain = Toolchain::dist("");
    /// # let krate = Crate::local("".as_ref());
    /// # let manifest_dir = "/path/to/bar";
    /// let sandbox = SandboxBuilder::new().mount(
    ///     Path::new(manifest_dir),
    ///     Path::new("/patch/bar"),
    ///     MountKind::ReadOnly,
    /// );
    /// let mut build_dir = workspace.build_dir("foo");
    /// build_dir.build(&toolchain, &krate, sandbox)
    ///     .patch_with_path("bar", "/patch/bar")
    ///     .run(|build| {
    ///         build.cargo().args(&["test", "--all"]).run()?;
    ///         Ok(())
    ///     })?;
    /// # Ok(())
    /// # }
    pub fn patch_with_path(mut self, name: &str, path: &str) -> Self {
        self.patches.push(CratePatch::Path(PathCratePatch {
            name: name.into(),
            path: path.into(),
        }));
        self
    }

    /// Run a sandboxed build of the provided crate with the provided toolchain. The closure will
    /// be provided an instance of [`Build`](struct.Build.html) that allows spawning new processes
    /// inside the sandbox.
    ///
    /// Returns a [`BuildResult`] containing both the closure's return value and build-level
    /// statistics gathered across the sandbox lifetime.
    ///
    /// All the state will be kept on disk as long as the closure doesn't exit: after that things
    /// might be removed.
    /// # Example
    ///
    /// ```no_run
    /// # use rustwide::{WorkspaceBuilder, Toolchain, Crate, cmd::SandboxBuilder};
    /// # use std::error::Error;
    /// # fn main() -> anyhow::Result<(), Box<dyn Error>> {
    /// # let workspace = WorkspaceBuilder::new("".as_ref(), "").init()?;
    /// # let toolchain = Toolchain::dist("");
    /// # let krate = Crate::local("".as_ref());
    /// # let sandbox = SandboxBuilder::new();
    /// let mut build_dir = workspace.build_dir("foo");
    /// let result = build_dir.build(&toolchain, &krate, sandbox).run(|build| {
    ///     build.cargo().args(&["test", "--all"]).run()?;
    ///     Ok(())
    /// })?;
    /// let _peak = result.memory_peak_bytes();
    /// # Ok(())
    /// # }
    pub fn run<R, F: FnOnce(&Build) -> anyhow::Result<R>>(
        self,
        f: F,
    ) -> anyhow::Result<BuildResult<R>> {
        self.build_dir
            .run(self.toolchain, self.krate, self.sandbox, self.patches, f)
    }
}

impl BuildDirectory {
    pub(crate) fn new(workspace: Workspace, name: &str) -> Self {
        Self {
            workspace,
            name: name.into(),
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
    /// # fn main() -> anyhow::Result<(), Box<dyn Error>> {
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
    ) -> BuildBuilder<'a> {
        BuildBuilder {
            build_dir: self,
            toolchain,
            krate,
            sandbox,
            patches: Vec::new(),
        }
    }

    pub(crate) fn run<R, F: FnOnce(&Build) -> anyhow::Result<R>>(
        &mut self,
        toolchain: &Toolchain,
        krate: &Crate,
        sandbox: SandboxBuilder,
        patches: Vec<CratePatch>,
        f: F,
    ) -> anyhow::Result<BuildResult<R>> {
        let source_dir = self.source_dir();
        if source_dir.exists() {
            crate::utils::remove_dir_all(&source_dir)?;
        }

        let mut prepare = Prepare::new(&self.workspace, toolchain, krate, &source_dir, patches);
        prepare.prepare().map_err(|err| {
            if err.downcast_ref::<PrepareError>().is_none() {
                err.context(PrepareError::Uncategorized)
            } else {
                err
            }
        })?;

        std::fs::create_dir_all(self.target_dir())?;
        let sandbox = Rc::new(RefCell::new(sandbox.clone().start(
            &self.workspace,
            source_dir.clone(),
            self.target_dir(),
        )));
        let sandbox_cleanup = sandbox.clone();
        scopeguard::defer! {{
            if let Err(err) = sandbox_cleanup.borrow_mut().cleanup() {
                log::error!("failed to clean up reused sandbox containers");
                log::error!("caused by: {err}");
            }
        }}
        let res = f(&Build {
            dir: self,
            toolchain,
            sandbox: sandbox.clone(),
        })?;
        let statistics = SandboxStatistics {
            memory_peak: sandbox.borrow().memory_peak_bytes(),
        };

        crate::utils::remove_dir_all(&source_dir)?;
        Ok(BuildResult {
            output: res,
            statistics,
        })
    }

    /// Remove all the contents of the build directory, freeing disk space.
    pub fn purge(&mut self) -> anyhow::Result<()> {
        let build_dir = self.build_dir();
        if build_dir.exists() {
            crate::utils::remove_dir_all(&build_dir)?;
        }
        Ok(())
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

/// API to interact with a running build.
///
/// This is created from [`BuildDirectory::build`](struct.BuildDirectory.html#method.build)
pub struct Build<'ws> {
    dir: &'ws BuildDirectory,
    toolchain: &'ws Toolchain,
    sandbox: Rc<RefCell<Sandbox<'ws>>>,
}

impl<'ws> Build<'ws> {
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
    /// # fn main() -> anyhow::Result<(), Box<dyn Error>> {
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
    pub fn cmd<'pl, R: Runnable>(&self, bin: R) -> Command<'ws, 'pl> {
        let container_dir = &*container_dirs::TARGET_DIR;

        Command::new_in_sandbox(&self.dir.workspace, self.sandbox.clone(), bin)
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
    /// # fn main() -> anyhow::Result<(), Box<dyn Error>> {
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
    pub fn cargo<'pl>(&self) -> Command<'ws, 'pl> {
        self.cmd(self.toolchain.cargo())
    }

    /// Return the peak memory usage in bytes observed across the sandbox so far, if available.
    ///
    /// Unlike [`BuildResult::memory_peak_bytes`], this can be queried while the build closure is
    /// still running.
    pub fn memory_peak_bytes(&self) -> Option<u64> {
        self.sandbox.borrow().memory_peak_bytes()
    }

    /// Get the path to the source code on the host machine (outside the sandbox).
    pub fn host_source_dir(&self) -> PathBuf {
        self.dir.source_dir()
    }

    /// Get the path to the target directory on the host machine (outside the sandbox).
    pub fn host_target_dir(&self) -> PathBuf {
        self.dir.target_dir()
    }

    /// Pre-fetching the dependencies for `-Z build-std` outside the sandbox.
    ///
    /// When this function is called, it is possible to use `-Zbuild-std` inside
    /// the sandbox to build the standard library from source even when
    /// networking is disabled.
    #[cfg(any(feature = "unstable", doc))]
    #[cfg_attr(docs_rs, doc(cfg(feature = "unstable")))]
    pub fn fetch_build_std_dependencies(&self, targets: &[&str]) -> anyhow::Result<()> {
        crate::prepare::fetch_deps(
            &self.dir.workspace,
            self.toolchain,
            &self.host_source_dir(),
            targets,
        )
    }
}
