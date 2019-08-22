use failure::Error;
use log::LevelFilter;
use rustwide::cmd::{CommandError, SandboxBuilder};

#[macro_use]
mod runner;

#[test]
fn test_hello_world() {
    runner::run("hello-world", |run| {
        run.build(SandboxBuilder::new().enable_networking(false), |build| {
            let mut storage = rustwide::logging::LogStorage::new(LevelFilter::Info);
            rustwide::logging::capture(&mut storage, || -> Result<_, Error> {
                build.cargo().args(&["run"]).run()?;
                Ok(())
            })?;

            assert!(storage.to_string().contains("[stdout] Hello, world!\n"));
            Ok(())
        })?;
        Ok(())
    });
}

#[test]
#[cfg(not(windows))]
fn test_sandbox_oom() {
    runner::run("out-of-memory", |run| {
        let res = run.build(
            SandboxBuilder::new()
                .enable_networking(false)
                .memory_limit(Some(512 * 1024 * 1024)),
            |build| {
                build.cargo().args(&["run"]).run()?;
                Ok(())
            },
        );
        if let Some(CommandError::SandboxOOM) = res.err().and_then(|err| err.downcast().ok()) {
            // Everything is OK!
        } else {
            panic!("didn't get the error CommandError::SandboxOOM");
        }
        Ok(())
    });
}

test_prepare_error!(
    test_missing_cargotoml,
    "missing-cargotoml",
    MissingCargoToml
);

test_prepare_error!(
    test_invalid_cargotoml_syntax,
    "invalid-cargotoml-syntax",
    InvalidCargoTomlSyntax
);

test_prepare_error!(
    test_invalid_cargotoml_content,
    "invalid-cargotoml-content",
    InvalidCargoTomlSyntax
);

test_prepare_error!(test_yanked_deps, "yanked-deps", YankedDependencies);
