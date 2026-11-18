use anyhow::{anyhow, bail};
use rustwide::cmd::{Command, CommandError};
use rustwide::{Crate, PrepareError, Toolchain, Workspace};
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[test]
fn test_fetch() -> anyhow::Result<()> {
    let workspace = crate::utils::init_workspace()?;
    let toolchain = Toolchain::dist("stable");
    toolchain.install(&workspace)?;

    let mut repo = Repo::new(&workspace)?;
    let krate = Crate::git(&repo.serve()?);
    krate.fetch(&workspace)?;

    // Return the commit that was used during a build.
    let cloned_commit = || -> anyhow::Result<String> {
        let mut dir = workspace.build_dir("integration-crates_git-test_fetch");
        dir.purge()?;
        dir.build(&toolchain, &krate, crate::utils::sandbox_builder())
            .run(|build| {
                Ok(Command::new(&workspace, "git")
                    .args(["rev-parse", "HEAD"])
                    .current_directory(build.host_source_dir())
                    .run_capture()?
                    .stdout_lines()[0]
                    .to_string())
            })
            .map(|result| result.into_inner())
    };

    // Check if the initial commit was fetched
    let initial_commit = repo.last_commit_sha.clone().unwrap();
    assert_eq!(initial_commit, krate.git_commit(&workspace).unwrap());
    assert_eq!(initial_commit, cloned_commit()?);

    // Make a new commit
    repo.commit(&workspace)?;
    let new_commit = repo.last_commit_sha.unwrap();
    assert_ne!(initial_commit, new_commit);
    assert_eq!(initial_commit, krate.git_commit(&workspace).unwrap());
    assert_eq!(initial_commit, cloned_commit()?);

    // Then ensure the new commit was fetched
    krate.fetch(&workspace)?;
    assert_eq!(new_commit, krate.git_commit(&workspace).unwrap());
    assert_eq!(new_commit, cloned_commit()?);

    Ok(())
}

/// Build the crate from `repo`, expecting `PrepareError::PathEscapesSource`,
/// and return its path.
#[cfg(unix)]
fn escaping_symlink_reported_by_build(
    workspace: &Workspace,
    repo: &Repo,
    build_dir: &str,
) -> anyhow::Result<PathBuf> {
    let toolchain = Toolchain::dist("stable");
    let krate = Crate::git(&repo.serve()?);
    krate.fetch(workspace)?;

    let mut dir = workspace.build_dir(build_dir);
    dir.purge()?;
    let err = dir
        .build(&toolchain, &krate, crate::utils::sandbox_builder())
        .run(|_| Ok(()))
        .err()
        .expect("a symlink escaping the source dir should fail the build");

    // the rejected source isn't left behind
    let source_dir = crate::utils::workspace_path("integration")
        .join("builds")
        .join(build_dir)
        .join("source");
    assert!(!source_dir.exists(), "{source_dir:?} still exists");

    match err.downcast_ref::<PrepareError>() {
        Some(PrepareError::PathEscapesSource(path)) => Ok(path.clone()),
        other => panic!("expected PrepareError::PathEscapesSource, got {other:?}: {err:?}"),
    }
}

#[test]
#[cfg(unix)]
fn test_symlinked_cargo_dir_escaping_source_dir_fails_build() -> anyhow::Result<()> {
    use std::{fs, os::unix};

    let workspace = crate::utils::init_workspace()?;

    // A host directory outside of the crate source.
    let host = tempfile::tempdir()?;
    let host_config = host.path().join("config");
    fs::write(&host_config, "host config")?;

    // Without the fix, preparing the crate deletes `host/config`.
    let mut repo = Repo::new(&workspace)?;
    unix::fs::symlink(host.path(), repo.source.path().join(".cargo"))?;
    repo.commit(&workspace)?;

    let path = escaping_symlink_reported_by_build(
        &workspace,
        &repo,
        "integration-crates_git-test_symlinked_cargo_dir",
    )?;
    assert_eq!(path, Path::new(".cargo"));
    assert_eq!(fs::read_to_string(&host_config)?, "host config");

    Ok(())
}

#[test]
#[cfg(unix)]
fn test_symlinked_manifest_escaping_source_dir_fails_build() -> anyhow::Result<()> {
    use std::{fs, os::unix};

    const HOST_MANIFEST: &str =
        "[package]\nname = \"victim\"\nversion = \"1.0.0\"\nworkspace = \"/sensitive\"\n";

    let workspace = crate::utils::init_workspace()?;

    // A host file outside of the crate source.
    let host = tempfile::tempdir()?;
    let host_manifest = host.path().join("host.toml");
    fs::write(&host_manifest, HOST_MANIFEST)?;

    // Without the fix, preparing the crate rewrites `host.toml`.
    let mut repo = Repo::new(&workspace)?;
    let manifest = repo.source.path().join("Cargo.toml");
    fs::remove_file(&manifest)?;
    unix::fs::symlink(&host_manifest, &manifest)?;
    repo.commit(&workspace)?;

    let path = escaping_symlink_reported_by_build(
        &workspace,
        &repo,
        "integration-crates_git-test_symlinked_manifest",
    )?;
    assert_eq!(path, Path::new("Cargo.toml"));
    assert_eq!(fs::read_to_string(&host_manifest)?, HOST_MANIFEST);

    Ok(())
}

#[test]
fn test_fetch_with_authentication() -> anyhow::Result<()> {
    let workspace = crate::utils::init_workspace()?;

    let repo = Repo::new(&workspace)?.authenticated();
    let krate = Crate::git(&repo.serve()?);

    let err = krate.fetch(&workspace).unwrap_err();
    if let Some(&CommandError::Timeout(_)) = err.downcast_ref() {
        panic!("an authentication prompt was shown during the fetch");
    } else if let Some(&PrepareError::PrivateGitRepository) = err.downcast_ref() {
        // Expected error
    } else {
        panic!("unexpected error: {}", err);
    }

    Ok(())
}

struct Repo {
    source: tempfile::TempDir,
    last_commit_sha: Option<String>,
    require_auth: bool,
}

impl Repo {
    fn new(workspace: &Workspace) -> anyhow::Result<Self> {
        let source = tempfile::tempdir()?;

        // Initialize a cargo project with a git repo in it.
        Command::new(workspace, "cargo")
            .args(["init", "--name", "foo", "--bin"])
            .args([source.path()])
            .run()?;

        let mut repo = Repo {
            source,
            last_commit_sha: None,
            require_auth: false,
        };
        repo.commit(workspace)?;
        Ok(repo)
    }

    fn authenticated(mut self) -> Self {
        self.require_auth = true;
        self
    }

    fn commit(&mut self, workspace: &Workspace) -> anyhow::Result<()> {
        Command::new(workspace, "git")
            .args(["add", "."])
            .current_directory(self.source.path())
            .run()?;
        Command::new(workspace, "git")
            .args(["-c", "commit.gpgsign=false"])
            .args(["-c", "user.name=test"])
            .args(["-c", "user.email=test@example.com"])
            .args(["commit", "-m", "auto commit"])
            .args(["--allow-empty"])
            .current_directory(self.source.path())
            .run()?;
        Command::new(workspace, "git")
            .args(["update-server-info"])
            .current_directory(self.source.path())
            .run()?;

        self.last_commit_sha = Some(
            Command::new(workspace, "git")
                .args(["rev-parse", "HEAD"])
                .current_directory(self.source.path())
                .run_capture()?
                .stdout_lines()[0]
                .to_string(),
        );
        Ok(())
    }

    fn serve(&self) -> anyhow::Result<String> {
        let server = tiny_http::Server::http("localhost:0").map_err(|e| anyhow!(e.to_string()))?;
        #[allow(irrefutable_let_patterns)]
        let port = if let tiny_http::ListenAddr::IP(socket_addr) = server.server_addr() {
            socket_addr.port()
        } else {
            bail!("found a non-IP server address");
        };

        let base = self.source.path().join(".git");
        let require_auth = self.require_auth;
        std::thread::spawn(move || {
            while let Ok(req) = server.recv() {
                // Remove the first char from the URL as it's the initial `/`.
                let url = req.url().split('?').next().unwrap()[1..].to_string();
                let file = std::fs::File::open(base.join(url));

                if require_auth {
                    let resp = tiny_http::Response::new_empty(tiny_http::StatusCode(401));
                    let _ = req.respond(resp.with_header(tiny_http::Header {
                        field: "WWW-Authenticate".parse().unwrap(),
                        value: "Basic realm=\"Dummy\"".parse().unwrap(),
                    }));
                } else if file.is_ok() {
                    let resp = tiny_http::Response::from_file(file.unwrap());
                    let _ = req.respond(resp);
                } else {
                    let resp = tiny_http::Response::new_empty(tiny_http::StatusCode(404));
                    let _ = req.respond(resp);
                }
            }
        });

        Ok(format!("http://localhost:{}", port))
    }
}
