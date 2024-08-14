use crate::cmd::{Binary, Command, Runnable};
use crate::tools::Tool;
use crate::{Toolchain, Workspace};
use std::path::PathBuf;

pub(crate) struct BinaryCrate {
    pub(super) crate_name: &'static str,
    pub(super) binary: &'static str,
    pub(super) cargo_subcommand: Option<&'static str>,
}

impl BinaryCrate {
    pub(crate) fn binary_path(&self, workspace: &Workspace) -> PathBuf {
        Tool::binary_path(self, workspace)
    }
}

impl Runnable for BinaryCrate {
    fn name(&self) -> Binary {
        Binary::ManagedByRustwide(if self.cargo_subcommand.is_some() {
            "cargo".into()
        } else {
            self.binary.into()
        })
    }

    fn prepare_command<'w, 'pl>(&self, mut cmd: Command<'w, 'pl>) -> Command<'w, 'pl> {
        if let Some(subcommand) = self.cargo_subcommand {
            cmd = cmd.args(&[subcommand]);
        }
        cmd
    }
}

impl Tool for BinaryCrate {
    fn name(&self) -> &'static str {
        self.binary
    }

    fn is_installed(&self, workspace: &Workspace) -> anyhow::Result<bool> {
        let path = self.binary_path(workspace);
        if !path.is_file() {
            return Ok(false);
        }

        crate::native::is_executable(path)
    }

    fn install(&self, workspace: &Workspace, fast_install: bool) -> anyhow::Result<()> {
        let mut cmd = Command::new(workspace, Toolchain::MAIN.cargo())
            .args(&["install", self.crate_name])
            .timeout(None);
        if fast_install {
            cmd = cmd.args(&["--debug"]);
        }
        cmd.run()?;
        Ok(())
    }

    fn update(&self, workspace: &Workspace, fast_install: bool) -> anyhow::Result<()> {
        self.install(workspace, fast_install)
    }
}
