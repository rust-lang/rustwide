use super::CrateTrait;
use crate::cmd::Command;
use crate::prepare::PrepareError;
use crate::Workspace;
use failure::{Error, ResultExt};
use log::{info, warn};
use percent_encoding::{percent_encode, AsciiSet, CONTROLS};
use std::path::{Path, PathBuf};

const ENCODE_SET: AsciiSet = CONTROLS
    .add(b'/')
    .add(b'\\')
    .add(b'<')
    .add(b'>')
    .add(b':')
    .add(b'"')
    .add(b'|')
    .add(b'?')
    .add(b'*')
    .add(b' ');

pub(super) struct GitRepo {
    url: String,
}

impl GitRepo {
    pub(super) fn new(url: &str) -> Self {
        Self { url: url.into() }
    }

    pub(super) fn git_commit(&self, workspace: &Workspace) -> Option<String> {
        let res = Command::new(workspace, "git")
            .args(&["rev-parse", "HEAD"])
            .cd(&self.cached_path(workspace))
            .run_capture();

        match res {
            Ok(out) => {
                if let Some(shaline) = out.stdout_lines().get(0) {
                    if !shaline.is_empty() {
                        return Some(shaline.to_string());
                    }
                }
                warn!("bad output from `git rev-parse HEAD`");
            }
            Err(e) => {
                warn!("unable to capture sha for {}: {}", self.url, e);
            }
        }
        None
    }

    fn cached_path(&self, workspace: &Workspace) -> PathBuf {
        workspace
            .cache_dir()
            .join("git-repos")
            .join(percent_encode(self.url.as_bytes(), &ENCODE_SET).to_string())
    }

    fn suppress_password_prompt_args(&self, workspace: &Workspace) -> Vec<String> {
        // The first `-c credential.helper=` clears the list of existing helpers
        vec![
            "-c".into(),
            "credential.helper=".into(),
            "-c".into(),
            format!(
                "credential.helper={}",
                crate::tools::GIT_CREDENTIAL_NULL
                    .binary_path(workspace)
                    .to_str()
                    .unwrap()
                    .replace('\\', "/")
            ),
        ]
    }
}

impl CrateTrait for GitRepo {
    fn fetch(&self, workspace: &Workspace) -> Result<(), Error> {
        // The credential helper that suppresses the password prompt shows this message when a
        // repository requires authentication:
        //
        //    fata: credential helper '{path}' told us to quit
        //
        let mut private_repository = false;
        let mut detect_private_repositories = |line: &str| {
            if line.starts_with("fatal: credential helper") && line.ends_with("told us to quit") {
                private_repository = true;
            }
        };

        let path = self.cached_path(workspace);
        let res = if path.join("HEAD").is_file() {
            info!("updating cached repository {}", self.url);
            Command::new(workspace, "git")
                .args(&self.suppress_password_prompt_args(workspace))
                .args(&["-c", "remote.origin.fetch=refs/heads/*:refs/heads/*"])
                .args(&["fetch", "origin", "--force", "--prune"])
                .cd(&path)
                .process_lines(&mut detect_private_repositories)
                .run()
                .with_context(|_| format!("failed to update {}", self.url))
        } else {
            info!("cloning repository {}", self.url);
            Command::new(workspace, "git")
                .args(&self.suppress_password_prompt_args(workspace))
                .args(&["clone", "--bare", &self.url])
                .args(&[&path])
                .process_lines(&mut detect_private_repositories)
                .run()
                .with_context(|_| format!("failed to clone {}", self.url))
        };

        if private_repository && res.is_err() {
            Err(PrepareError::PrivateGitRepository.into())
        } else {
            Ok(res?)
        }
    }

    fn copy_source_to(&self, workspace: &Workspace, dest: &Path) -> Result<(), Error> {
        Command::new(workspace, "git")
            .args(&["clone"])
            .args(&[self.cached_path(workspace).as_path(), dest])
            .run()
            .with_context(|_| format!("failed to checkout {}", self.url))?;
        Ok(())
    }
}

impl std::fmt::Display for GitRepo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "git repo {}", self.url)
    }
}
