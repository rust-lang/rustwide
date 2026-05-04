use rustwide::cmd::SandboxBuilder;

#[test]
fn test_container_cleanup_on_success() {
    super::runner::run("hello-world", |run| {
        let container_id = run.run(SandboxBuilder::new().enable_networking(false), |build| {
            // Verify we are running inside a Docker container
            let dockerenv = build.cmd("test").args(&["-f", "/.dockerenv"]).run_capture();
            assert!(
                dockerenv.is_ok(),
                "expected to run inside a Docker container"
            );

            let output = build.cmd("cat").args(&["/etc/hostname"]).run_capture()?;
            Ok(output.stdout_lines()[0].trim().to_string())
        })?;

        assert!(
            !container_id.is_empty(),
            "should have captured container ID"
        );
        assert_container_stopped_and_removed(&container_id);
        Ok(())
    });
}

#[test]
fn test_container_reused_across_commands() {
    super::runner::run("hello-world", |run| {
        let container_ids = run.run(SandboxBuilder::new().enable_networking(false), |build| {
            let first = build.cmd("cat").args(&["/etc/hostname"]).run_capture()?;
            let second = build.cmd("cat").args(&["/etc/hostname"]).run_capture()?;

            Ok(vec![
                first.stdout_lines()[0].trim().to_string(),
                second.stdout_lines()[0].trim().to_string(),
            ])
        })?;

        assert_eq!(container_ids.len(), 2);
        assert_eq!(container_ids[0], container_ids[1]);
        assert!(
            !container_ids[0].is_empty(),
            "should capture a container ID"
        );
        assert_container_stopped_and_removed(&container_ids[0]);
        Ok(())
    });
}

#[test]
#[cfg(not(windows))]
fn test_reused_container_oom_does_not_poison_later_commands() {
    use rustwide::cmd::CommandError;

    super::runner::run("allocate", |run| {
        run.run(
            SandboxBuilder::new()
                .enable_networking(false)
                .memory_limit(Some(512 * 1024 * 1024)),
            |build| {
                let first = build.cargo().args(&["run", "--", "1024"]).run();
                assert!(
                    matches!(first, Err(CommandError::SandboxOOM)),
                    "expected first command to OOM, got {first:?}"
                );

                build.cmd("true").run()?;
                Ok(())
            },
        )?;
        Ok(())
    });
}

#[test]
fn test_container_cleanup_on_command_failure() {
    super::runner::run("hello-world", |run| {
        let container_id = run.run(SandboxBuilder::new().enable_networking(false), |build| {
            // Verify we are running inside a Docker container
            let dockerenv = build.cmd("test").args(&["-f", "/.dockerenv"]).run_capture();
            assert!(
                dockerenv.is_ok(),
                "expected to run inside a Docker container"
            );

            let mut container_id = String::new();
            let _err = build
                .cmd("sh")
                .args(&["-c", "cat /etc/hostname; exit 1"])
                .process_lines(&mut |line, _| {
                    if container_id.is_empty() {
                        container_id = line.trim().to_string();
                    }
                })
                .run();
            Ok(container_id)
        })?;

        assert!(
            !container_id.is_empty(),
            "should have captured container ID"
        );
        assert_container_stopped_and_removed(&container_id);
        Ok(())
    });
}

fn assert_container_stopped_and_removed(container_id: &str) {
    // Verify the container is not running
    let output = std::process::Command::new("docker")
        .args(["ps", "-q", "--filter", &format!("id={}", container_id)])
        .output()
        .expect("failed to run docker ps");
    let remaining = String::from_utf8_lossy(&output.stdout);
    assert!(
        remaining.trim().is_empty(),
        "container {} should not be running",
        container_id
    );

    // Verify the container has been removed entirely
    let output = std::process::Command::new("docker")
        .args([
            "ps",
            "-a",
            "-q",
            "--filter",
            &format!("id={}", container_id),
        ])
        .output()
        .expect("failed to run docker ps -a");
    let remaining = String::from_utf8_lossy(&output.stdout);
    assert!(
        remaining.trim().is_empty(),
        "container {} should have been removed",
        container_id
    );
}
