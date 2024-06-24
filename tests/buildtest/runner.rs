use rand::{distributions::Alphanumeric, Rng};
use rustwide::{cmd::SandboxBuilder, Build, BuildBuilder, Crate, Toolchain, Workspace};
use std::path::Path;

pub(crate) fn run(crate_name: &str, f: impl FnOnce(&mut Runner) -> anyhow::Result<()>) {
    let mut runner = Runner::new(crate_name).unwrap();
    f(&mut runner).unwrap();
}

pub(crate) struct Runner {
    crate_name: String,
    workspace: Workspace,
    toolchain: Toolchain,
    krate: Crate,
}

impl Runner {
    fn new(crate_name: &str) -> anyhow::Result<Self> {
        let workspace = crate::utils::init_workspace()?;
        let krate = Crate::local(
            &Path::new("tests")
                .join("buildtest")
                .join("crates")
                .join(crate_name),
        );
        Ok(Runner {
            crate_name: if std::env::var("RUSTWIDE_TEST_INSIDE_DOCKER").is_ok() {
                format!("{}-inside-docker", crate_name)
            } else {
                crate_name.to_string()
            },
            workspace,
            toolchain: Toolchain::dist("stable"),
            krate,
        })
    }

    pub(crate) fn build<T>(
        &self,
        sandbox: SandboxBuilder,
        f: impl FnOnce(BuildBuilder) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        // Use a random string at the end to avoid conflicts if multiple tests use the same source crate.
        let suffix: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(10)
            .map(char::from)
            .collect();

        let mut dir = self
            .workspace
            .build_dir(&format!("{}-{suffix}", &self.crate_name));
        dir.purge()?;
        f(dir.build(&self.toolchain, &self.krate, sandbox))
    }

    pub(crate) fn run<T>(
        &self,
        sandbox: SandboxBuilder,
        f: impl FnOnce(&Build) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        self.build(sandbox, |builder| builder.run(f))
    }
}

macro_rules! test_prepare_error {
    ($name:ident, $krate:expr, $expected:ident) => {
        #[test]
        fn $name() {
            runner::run($krate, |run| {
                let res = run.run(
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
