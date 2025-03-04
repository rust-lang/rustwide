use crate::cmd::{Binary, Command, Runnable};
use crate::toolchain::MAIN_TOOLCHAIN_NAME;
use crate::tools::{Tool, RUSTUP};
use crate::workspace::Workspace;
use anyhow::Context as _;
use std::env::consts::EXE_SUFFIX;
use std::fs::{self, File};
use std::io;
use tempfile::tempdir;

// we're using an old version of rustup, since rustup 1.28 is broken for rustwide for now.
// We'll try to either fix rustup, or adapt rustwide to fix this, until then, we'll use this version.
// see https://github.com/rust-lang/rustup/issues/4224
static RUSTUP_VERSION: &str = "1.27.1";

pub(crate) struct Rustup;

impl Runnable for Rustup {
    fn name(&self) -> Binary {
        Binary::ManagedByRustwide("rustup".into())
    }
}

impl Tool for Rustup {
    fn name(&self) -> &'static str {
        "rustup"
    }

    fn is_installed(&self, workspace: &Workspace) -> anyhow::Result<bool> {
        let path = self.binary_path(workspace);
        if !path.is_file() {
            return Ok(false);
        }

        crate::native::is_executable(path)
    }

    fn install(&self, workspace: &Workspace, _fast_install: bool) -> anyhow::Result<()> {
        fs::create_dir_all(workspace.cargo_home())?;
        fs::create_dir_all(workspace.rustup_home())?;

        let url = format!(
            "https://static.rust-lang.org/rustup/archive/{version}/{target}/rustup-init{exe_suffix}",
            version = RUSTUP_VERSION,
            target = crate::HOST_TARGET,
            exe_suffix = EXE_SUFFIX
        );
        let mut resp = workspace
            .http_client()
            .get(url)
            .send()?
            .error_for_status()?;

        let tempdir = tempdir()?;
        let installer = &tempdir.path().join(format!("rustup-init{}", EXE_SUFFIX));
        {
            let mut file = File::create(installer)?;
            io::copy(&mut resp, &mut file)?;
            crate::native::make_executable(installer)?;
        }

        Command::new(workspace, installer.to_string_lossy().as_ref())
            .args(&[
                "-y",
                "--no-modify-path",
                "--default-toolchain",
                MAIN_TOOLCHAIN_NAME,
                "--profile",
                workspace.rustup_profile(),
            ])
            .env("RUSTUP_HOME", workspace.rustup_home())
            .env("CARGO_HOME", workspace.cargo_home())
            .run()
            .context("unable to install rustup")?;

        Ok(())
    }

    fn update(&self, workspace: &Workspace, _fast_install: bool) -> anyhow::Result<()> {
        Command::new(workspace, &RUSTUP)
            .args(&["update", MAIN_TOOLCHAIN_NAME, "--no-self-update"])
            .run()
            .with_context(|| format!("failed to update main toolchain {}", MAIN_TOOLCHAIN_NAME))?;
        Ok(())
    }
}
