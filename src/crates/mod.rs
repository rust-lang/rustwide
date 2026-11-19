mod git;
mod local;
mod registry;

use crate::{Workspace, crates::registry::CRATES_IO_SPARSE_INDEX};
use anyhow::{Context as _, Result};
use log::info;
use std::path::Path;
use url::Url;

#[cfg(feature = "git-registries")]
pub use registry::GitRegistry;
#[cfg(feature = "git-registries")]
pub use registry::GitRegistry as AlternativeRegistry;

trait CrateTrait: std::fmt::Display {
    fn fetch(&self, workspace: &Workspace) -> anyhow::Result<()>;
    fn purge_from_cache(&self, workspace: &Workspace) -> anyhow::Result<()>;
    fn copy_source_to(&self, workspace: &Workspace, dest: &Path) -> anyhow::Result<()>;
}

enum CrateType {
    Registry(registry::RegistryCrate),
    Git(git::GitRepo),
    Local(local::Local),
}

/// A Rust crate that can be used with rustwide.
pub struct Crate(CrateType);

impl Crate {
    /// Load a crate from a sparse registry index.
    ///
    /// `index` is the URL of the registry index, with or without Cargo's `sparse+` prefix.
    /// Its `config.json` is cached in the workspace after the first fetch.
    pub fn sparse_registry<U>(index: U, name: &str, version: &str) -> Result<Self>
    where
        U: TryInto<Url>,
        <U as TryInto<Url>>::Error: std::error::Error + Send + Sync + 'static,
    {
        let index = index.try_into().context("invalid index url")?;

        Ok(Crate(CrateType::Registry(registry::RegistryCrate::new(
            registry::Registry::Sparse(registry::normalize_sparse_index(index)?),
            name,
            version,
        ))))
    }

    /// Load a crate from a Git-indexed registry.
    ///
    /// For an HTTP sparse index, use [`Crate::sparse_registry`].
    #[cfg(feature = "git-registries")]
    pub fn git_registry(registry: GitRegistry, name: &str, version: &str) -> Self {
        Crate(CrateType::Registry(registry::RegistryCrate::new(
            registry::Registry::Git(registry),
            name,
            version,
        )))
    }

    /// Compatibility alias for [`Crate::git_registry`].
    #[cfg(feature = "git-registries")]
    pub fn registry(registry: AlternativeRegistry, name: &str, version: &str) -> Self {
        Self::git_registry(registry, name, version)
    }

    /// Load a crate from the [crates.io registry](https://crates.io).
    pub fn crates_io(name: &str, version: &str) -> Self {
        Self::sparse_registry(CRATES_IO_SPARSE_INDEX.clone(), name, version)
            .expect("we know crates.io index url is valid")
    }

    /// Load a crate from a git repository. The full URL needed to clone the repo has to be
    /// provided.
    pub fn git(url: &str) -> Self {
        Crate(CrateType::Git(git::GitRepo::new(url)))
    }

    /// Load a crate from a directory in the local filesystem.
    pub fn local(path: &Path) -> Self {
        Crate(CrateType::Local(local::Local::new(path)))
    }

    /// Fetch the crate's source code and cache it in the workspace. This method will reach out to
    /// the network for some crate types.
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(skip_all, fields(krate = %self))
    )]
    pub fn fetch(&self, workspace: &Workspace) -> anyhow::Result<()> {
        self.as_trait().fetch(workspace)
    }

    /// Remove the cached copy of this crate. The method will do nothing if the crate isn't cached.
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(skip_all, fields(krate = %self))
    )]
    pub fn purge_from_cache(&self, workspace: &Workspace) -> anyhow::Result<()> {
        self.as_trait().purge_from_cache(workspace)
    }

    /// Get this crate's git commit. This method is best-effort, and currently works just for git
    /// crates. If the commit can't be retrieved `None` will be returned.
    pub fn git_commit(&self, workspace: &Workspace) -> Option<String> {
        if let CrateType::Git(repo) = &self.0 {
            repo.git_commit(workspace)
        } else {
            None
        }
    }

    /// copy the source of this crate to the specified destination path.
    ///
    /// Will delete the target directory if it already exists.
    ///
    /// Fails with [`PrepareError::InvalidArchiveEntry`] when a registry crate
    /// tarball contains an entry cargo wouldn't extract, like a symlink.
    ///
    /// Fails with [`PrepareError::PathEscapesSource`] when the source contains
    /// a symlink that points outside of it or can't be resolved; the
    /// destination is removed again. Dangling symlinks are removed. Only git
    /// crates can contain symlinks.
    ///
    /// [`PrepareError::InvalidArchiveEntry`]: crate::PrepareError::InvalidArchiveEntry
    /// [`PrepareError::PathEscapesSource`]: crate::PrepareError::PathEscapesSource
    #[cfg_attr(
        feature = "tracing",
        tracing::instrument(skip_all, fields(krate = %self, dest = %dest.display()))
    )]
    pub fn copy_source_to(&self, workspace: &Workspace, dest: &Path) -> anyhow::Result<()> {
        if dest.exists() {
            info!(
                "crate source directory {} already exists, cleaning it up",
                dest.display()
            );
            crate::utils::remove_dir_all(dest)?;
        }
        self.as_trait().copy_source_to(workspace, dest)?;

        // Has to run before anything else touches the copied source.
        if let Err(err) = crate::utils::sanitize_symlinks(dest) {
            // Don't leave a source with escaping symlinks behind.
            let _ = crate::utils::remove_dir_all(dest);
            return Err(err);
        }
        Ok(())
    }

    fn as_trait(&self) -> &dyn CrateTrait {
        match &self.0 {
            CrateType::Registry(krate) => krate,
            CrateType::Git(repo) => repo,
            CrateType::Local(local) => local,
        }
    }
}

impl std::fmt::Display for Crate {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.as_trait())
    }
}
