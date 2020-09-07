use failure::Error;
use log::LevelFilter;
use rustwide::cmd::{ProcessLinesActions, SandboxBuilder};

#[macro_use]
mod runner;
mod inside_docker;

#[test]
fn test_hello_world() {
    runner::run("hello-world", |run| {
        run.build(SandboxBuilder::new().enable_networking(false), |build| {
            let storage = rustwide::logging::LogStorage::new(LevelFilter::Info);
            rustwide::logging::capture(&storage, || -> Result<_, Error> {
                build.cargo().args(&["run"]).run()?;
                Ok(())
            })?;

            assert!(storage.to_string().contains("[stdout] Hello, world!\n"));
            assert!(storage
                .to_string()
                .contains("[stdout] Hello, world again!\n"));
            Ok(())
        })?;
        Ok(())
    });
}

#[test]
fn test_process_lines() {
    runner::run("process-lines", |run| {
        run.build(SandboxBuilder::new().enable_networking(false), |build| {
            let storage = rustwide::logging::LogStorage::new(LevelFilter::Info);
            let mut ex = false;
            rustwide::logging::capture(&storage, || -> Result<_, Error> {
                build
                    .cargo()
                    .process_lines(&mut |line: &str, actions: &mut ProcessLinesActions| {
                        if line.contains("Hello, world again!") {
                            ex = true;
                            actions.replace_with_lines(line.split(","));
                        } else if line.contains("Hello, world!") {
                            actions.remove_line();
                        }
                    })
                    .args(&["run"])
                    .run()?;
                Ok(())
            })?;

            assert!(ex);
            assert!(!storage.to_string().contains("[stdout] Hello, world!\n"));
            assert!(storage.to_string().contains("[stdout]  world again!\n"));
            assert!(storage.to_string().contains("[stdout] Hello\n"));
            Ok(())
        })?;
        Ok(())
    });
}

#[test]
#[cfg(not(windows))]
fn test_sandbox_oom() {
    use rustwide::cmd::CommandError;

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

#[test]
fn test_cargo_config() {
    runner::run("cargo-config", |run| {
        run.build(SandboxBuilder::new().enable_networking(false), |build| {
            let storage = rustwide::logging::LogStorage::new(LevelFilter::Info);
            rustwide::logging::capture(&storage, || -> Result<_, Error> {
                build.cargo().args(&["run"]).run()?;
                Ok(())
            })?;
            Ok(())
        })?;
        Ok(())
    });
}

#[test]
fn workspace() {
    runner::run("workspace", |run| {
        run.build(SandboxBuilder::new().enable_networking(false), |build| {
            let storage = rustwide::logging::LogStorage::new(LevelFilter::Info);
            rustwide::logging::capture(&storage, || -> Result<_, Error> {
                build.cargo().args(&["run"]).run()?;
                Ok(())
            })?;
            Ok(())
        })?;
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
