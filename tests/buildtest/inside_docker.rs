#![cfg_attr(windows, allow(unused))]

use anyhow::Context;
use std::io::Write;
use std::path::Path;
use std::process::Command;

static DOCKER_IMAGE_TAG: &str = "ghcr.io/rust-lang/crates-build-env/linux-micro";
static DOCKER_SOCKET: &str = "/var/run/docker.sock";
static CONTAINER_PREFIX: &str = "/outside";
static TARGET_PREFIX: &str = "/target";

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

fn execute(test: &str) -> anyhow::Result<()> {
    // The current working directory is mounted in the container to /outside.
    // The binary to execute is remapped to be prefixed by /outside instead of the current
    // directory.
    let current_dir = std::fs::canonicalize(".")?;
    let target_parent_dir = match option_env!("CARGO_TARGET_DIR") {
        Some(t) => Path::new(t).parent().unwrap(),
        None => &current_dir,
    };
    let current_exe = std::env::current_exe().unwrap();
    let container_prefix = Path::new(CONTAINER_PREFIX);
    let target_prefix = Path::new(TARGET_PREFIX);
    let container_exe = target_prefix.join(
        current_exe
            .strip_prefix(target_parent_dir)
            .context("could not determine cargo target dir")?,
    );
    let src_mount = os_string!(&current_dir, ":", &container_prefix);
    let target_mount = os_string!(&target_parent_dir, ":", &target_prefix);
    let docker_sock = os_string!(DOCKER_SOCKET, ":", DOCKER_SOCKET);

    Command::new("docker")
        .arg("run")
        .arg("-v")
        .arg(src_mount)
        .arg("-v")
        .arg(target_mount)
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
    fn map_user_group(&mut self) -> anyhow::Result<&mut Self>;
    fn assert(&mut self) -> anyhow::Result<()>;
}

impl CommandExt for Command {
    #[cfg(unix)]
    fn map_user_group(&mut self) -> anyhow::Result<&mut Self> {
        use std::os::unix::fs::MetadataExt;
        let gid = std::fs::metadata(DOCKER_SOCKET)?.gid();
        let uid = nix::unistd::Uid::effective();

        self.arg("--user").arg(format!("{}:{}", uid, gid));
        Ok(self)
    }

    #[cfg(windows)]
    fn map_user_group(&mut self) -> anyhow::Result<&mut Self> {
        Ok(self)
    }

    fn assert(&mut self) -> anyhow::Result<()> {
        let out = self.output()?;
        if !out.status.success() {
            eprintln!("failed to execute command {:?}", self);
            eprintln!("stdout:");
            std::io::stderr().lock().write_all(&out.stdout)?;
            eprintln!("stderr:");
            std::io::stderr().lock().write_all(&out.stderr)?;
            anyhow::bail!("failed to execute command {:?}", self);
        }
        Ok(())
    }
}
