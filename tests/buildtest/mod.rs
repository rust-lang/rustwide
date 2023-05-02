use failure::Error;
use log::LevelFilter;
use rustwide::cmd::{ProcessLinesActions, SandboxBuilder};

#[macro_use]
mod runner;
mod inside_docker;

#[test]
fn test_hello_world() {
    runner::run("hello-world", |run| {
        run.run(SandboxBuilder::new().enable_networking(false), |build| {
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
#[cfg(feature = "unstable")]
fn test_fetch_build_std() {
    use std::path::Path;

    let target_file = Path::new(env!("OUT_DIR")).join("target");
    let target = std::fs::read_to_string(target_file).unwrap();

    runner::run("hello-world", |run| {
        run.run(SandboxBuilder::new().enable_networking(false), |build| {
            build.fetch_build_std_dependencies(&vec![target.as_str()])?;
            let storage = rustwide::logging::LogStorage::new(LevelFilter::Info);
            rustwide::logging::capture(&storage, || -> Result<_, Error> {
                build
                    .cargo()
                    .env("RUSTC_BOOTSTRAP", "1")
                    .args(&["run", "-Zbuild-std", "--target", &target])
                    .run()?;
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
fn path_based_patch() {
    runner::run("path-based-patch", |run| {
        run.build(SandboxBuilder::new().enable_networking(false), |builder| {
            builder
                .patch_with_path("empty-library", "./patch")
                .run(move |build| {
                    let storage = rustwide::logging::LogStorage::new(LevelFilter::Info);
                    rustwide::logging::capture(&storage, || -> Result<_, Error> {
                        build.cargo().args(&["run"]).run()?;
                        Ok(())
                    })?;

                    assert!(storage.to_string().contains("[stdout] Hello, world!\n"));
                    assert!(storage
                        .to_string()
                        .contains("[stdout] This is coming from the patch!\n"));
                    Ok(())
                })
        })?;
        Ok(())
    });
}

#[test]
fn test_process_lines() {
    runner::run("process-lines", |run| {
        run.run(SandboxBuilder::new().enable_networking(false), |build| {
            let storage = rustwide::logging::LogStorage::new(LevelFilter::Info);
            let mut ex = false;
            rustwide::logging::capture(&storage, || -> Result<_, Error> {
                build
                    .cargo()
                    .process_lines(&mut |line: &str, actions: &mut ProcessLinesActions| {
                        if line.contains("Hello, world again!") {
                            ex = true;
                            actions.replace_with_lines(line.split(','));
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
        let res = run.run(
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
fn test_override_files() {
    runner::run("cargo-config", |run| {
        run.run(SandboxBuilder::new().enable_networking(false), |build| {
            let storage = rustwide::logging::LogStorage::new(LevelFilter::Info);
            rustwide::logging::capture(&storage, || -> Result<_, Error> {
                build.cargo().args(&["--version"]).run()?;
                Ok(())
            })?;
            let output = storage.to_string();
            assert!(output.contains("cargo 1."));
            assert!(!output.contains("1.0.0"));
            build.cargo().args(&["run"]).run()?;
            Ok(())
        })?;
        Ok(())
    });
}

#[test]
fn test_cargo_workspace() {
    runner::run("cargo-workspace", |run| {
        run.run(SandboxBuilder::new().enable_networking(false), |build| {
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

test_prepare_error!(
    test_missing_deps_git,
    "missing-deps-git",
    MissingDependencies
);

test_prepare_error!(
    test_missing_deps_git_locked,
    "missing-deps-git-locked",
    MissingDependencies
);

test_prepare_error!(
    test_missing_deps_registry,
    "missing-deps-registry",
    MissingDependencies
);
