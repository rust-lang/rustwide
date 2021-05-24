mod git;
mod local;
mod registry;

use crate::Workspace;
use failure::Error;
use log::info;
use std::path::Path;

trait CrateTrait: std::fmt::Display {
    fn fetch(&self, workspace: &Workspace) -> Result<(), Error>;
    fn purge_from_cache(&self, workspace: &Workspace) -> Result<(), Error>;
    fn copy_source_to(&self, workspace: &Workspace, dest: &Path) -> Result<(), Error>;
}

enum CrateType {
    Registry(registry::RegistryCrate),
    Git(git::GitRepo),
    Local(local::Local),
}

/// A Rust crate that can be used with rustwide.
pub struct Crate(CrateType);

impl Crate {
    /// Load a crate from specified registry.
    pub fn registry(registry_index: &str, name: &str, version: &str) -> Self {
        Crate(CrateType::Registry(registry::RegistryCrate::new(
            registry::Registry::Alternative(registry::AlternativeRegistry::new(registry_index)),
            name,
            version,
        )))
    }

    /// Load a crate from the [crates.io registry](https://crates.io).
    pub fn crates_io(name: &str, version: &str) -> Self {
        Crate(CrateType::Registry(registry::RegistryCrate::new(
            registry::Registry::CratesIo,
            name,
            version,
        )))
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
    pub fn fetch(&self, workspace: &Workspace) -> Result<(), Error> {
        self.as_trait().fetch(workspace)
    }

    /// Remove the cached copy of this crate. The method will do nothing if the crate isn't cached.
    pub fn purge_from_cache(&self, workspace: &Workspace) -> Result<(), Error> {
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

    /// Copy this crate's source to `dest`. If `dest` already exists, it will be replaced.
    pub fn copy_source_to(&self, workspace: &Workspace, dest: &Path) -> Result<(), Error> {
        if dest.exists() {
            info!(
                "crate source directory {} already exists, cleaning it up",
                dest.display()
            );
            crate::utils::remove_dir_all(dest)?;
        }
        self.as_trait().copy_source_to(workspace, dest)
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
