use failure::Error;
use rustwide::{cmd::SandboxBuilder, Build, Crate, Toolchain, Workspace};
use std::borrow::Cow;
use std::path::Path;

static TOOLCHAIN: Toolchain = Toolchain::Dist {
    name: Cow::Borrowed("stable"),
};

pub(crate) fn run(crate_name: &str, f: impl FnOnce(&mut Runner) -> Result<(), Error>) {
    let mut runner = Runner::new(crate_name).unwrap();
    f(&mut runner).unwrap();
}

pub(crate) struct Runner {
    crate_name: String,
    workspace: Workspace,
    toolchain: &'static Toolchain,
    krate: Crate,
}

impl Runner {
    fn new(crate_name: &str) -> Result<Self, Error> {
        let workspace = crate::utils::init_workspace()?;
        let krate = Crate::local(
            &Path::new("tests")
                .join("buildtest")
                .join("crates")
                .join(crate_name),
        );
        Ok(Runner {
            crate_name: crate_name.to_string(),
            workspace,
            toolchain: &TOOLCHAIN,
            krate,
        })
    }

    pub(crate) fn build<T>(
        &self,
        sandbox: SandboxBuilder,
        f: impl FnOnce(&Build) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut dir = self.workspace.build_dir(&self.crate_name);
        dir.purge()?;
        dir.build(self.toolchain, &self.krate, sandbox).run(f)
    }
}

macro_rules! test_prepare_error {
    ($name:ident, $krate:expr, $expected:ident) => {
        #[test]
        fn $name() {
            runner::run($krate, |run| {
                let res = run.build(
                    rustwide::cmd::SandboxBuilder::new().enable_networking(false),
                    |_| Ok(()),
                );
                if let Some(rustwide::PrepareError::$expected) =
                    res.err().and_then(|err| err.downcast().ok())
                {
                    // Everything is OK!
                } else {
                    panic!("didn't get the error {}", stringify!($expected));
                }
                Ok(())
            });
        }
    };
}
