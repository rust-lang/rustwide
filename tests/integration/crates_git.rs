use failure::Error;
use rustwide::cmd::{Command, CommandError};
use rustwide::{Crate, PrepareError, Workspace};

#[test]
fn test_fetch() -> Result<(), Error> {
    let workspace = crate::utils::init_workspace()?;

    let repo = Repo::new(&workspace)?;
    let krate = Crate::git(&repo.serve()?);
    krate.fetch(&workspace)?;

    assert!(repo.last_commit_sha.is_some());
    assert_eq!(repo.last_commit_sha, krate.git_commit(&workspace));

    Ok(())
}

#[test]
fn test_fetch_with_authentication() -> Result<(), Error> {
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
    fn new(workspace: &Workspace) -> Result<Self, Error> {
        let source = tempfile::tempdir()?;

        // Initialize a cargo project with a git repo in it.
        Command::new(workspace, "cargo")
            .args(&["init", "--name", "foo", "--bin"])
            .args(&[source.path()])
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

    fn commit(&mut self, workspace: &Workspace) -> Result<(), Error> {
        Command::new(workspace, "git")
            .args(&["add", "."])
            .cd(self.source.path())
            .run()?;
        Command::new(workspace, "git")
            .args(&["-c", "commit.gpgsign=false"])
            .args(&["-c", "user.name=test"])
            .args(&["-c", "user.email=test@example.com"])
            .args(&["commit", "-m", "auto commit"])
            .cd(self.source.path())
            .run()?;
        Command::new(workspace, "git")
            .args(&["update-server-info"])
            .cd(self.source.path())
            .run()?;

        self.last_commit_sha = Some(
            Command::new(workspace, "git")
                .args(&["rev-parse", "HEAD"])
                .cd(self.source.path())
                .run_capture()?
                .stdout_lines()[0]
                .to_string(),
        );
        Ok(())
    }

    fn serve(&self) -> Result<String, Error> {
        let server =
            tiny_http::Server::http("localhost:0").map_err(|e| failure::err_msg(e.to_string()))?;
        let port = server.server_addr().port();

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
