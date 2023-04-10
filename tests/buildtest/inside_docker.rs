#![cfg_attr(windows, allow(unused))]

use failure::{Error, ResultExt};
use std::io::Write;
use std::path::Path;
use std::process::Command;

static DOCKER_IMAGE_TAG: &str = "ghcr.io/rust-lang/crates-build-env/linux-micro";
static DOCKER_SOCKET: &str = "/var/run/docker.sock";
static CONTAINER_PREFIX: &str = "/outside";

#[test]
#[cfg(unix)]
fn test_hello_world() {
    execute("buildtest::test_hello_world").unwrap();
}

#[test]
#[cfg(unix)]
fn test_path_based_patch() {
    execute("buildtest::path_based_patch").unwrap();
}

fn execute(test: &str) -> Result<(), Error> {
    // The current working directory is mounted in the container to /outside.
    // The binary to execute is remapped to be prefixed by /outside instead of the current
    // directory.
    let current_dir = std::fs::canonicalize(".")?;
    let current_exe = std::env::current_exe().unwrap();
    let container_prefix = Path::new(CONTAINER_PREFIX);
    let container_exe = container_prefix.join(
        current_exe
            .strip_prefix(&current_dir)
            .with_context(|_| "the working directory is not a parent of the test binary")?,
    );
    let mount = os_string!(&current_dir, ":", &container_prefix);
    let docker_sock = os_string!(DOCKER_SOCKET, ":", DOCKER_SOCKET);

    Command::new("docker")
        .arg("run")
        .arg("-v")
        .arg(mount)
        .arg("-v")
        .arg(docker_sock)
        .arg("-w")
        .arg(container_prefix)
        .arg("-e")
        .arg("RUST_BACKTRACE=1")
        .arg("-e")
        .arg("RUSTWIDE_TEST_INSIDE_DOCKER=1")
        .map_user_group()?
        .arg("--rm")
        .arg("-i")
        .arg(DOCKER_IMAGE_TAG)
        .arg(&container_exe)
        .arg(test)
        .assert()?;

    Ok(())
}

trait CommandExt {
    fn map_user_group(&mut self) -> Result<&mut Self, Error>;
    fn assert(&mut self) -> Result<(), Error>;
}

impl CommandExt for Command {
    #[cfg(unix)]
    fn map_user_group(&mut self) -> Result<&mut Self, Error> {
        use std::os::unix::fs::MetadataExt;
        let gid = std::fs::metadata(DOCKER_SOCKET)?.gid();
        let uid = nix::unistd::Uid::effective();

        self.arg("--user").arg(format!("{}:{}", uid, gid));
        Ok(self)
    }

    #[cfg(windows)]
    fn map_user_group(&mut self) -> Result<&mut Self, Error> {
        Ok(self)
    }

    fn assert(&mut self) -> Result<(), Error> {
        let out = self.output()?;
        if !out.status.success() {
            eprintln!("failed to execute command {:?}", self);
            eprintln!("stdout:");
            std::io::stderr().lock().write_all(&out.stdout)?;
            eprintln!("stderr:");
            std::io::stderr().lock().write_all(&out.stderr)?;
            failure::bail!("failed to execute command {:?}", self);
        }
        Ok(())
    }
}
