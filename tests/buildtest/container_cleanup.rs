use std::time::Duration;

#[test]
fn test_container_cleanup_on_success() {
    super::runner::run("hello-world", |run| {
        let container_id = run.run(crate::utils::sandbox_builder(), |build| {
            // Verify we are running inside a Docker container
            let dockerenv = build.cmd("test").args(["-f", "/.dockerenv"]).run_capture();
            assert!(
                dockerenv.is_ok(),
                "expected to run inside a Docker container"
            );

            let output = build.cmd("cat").args(["/etc/hostname"]).run_capture()?;
            Ok(output.stdout_lines()[0].trim().to_string())
        })?;
        let container_id = container_id.into_inner();

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
        let container_ids = run.run(crate::utils::sandbox_builder(), |build| {
            let first = build.cmd("cat").args(["/etc/hostname"]).run_capture()?;
            let second = build.cmd("cat").args(["/etc/hostname"]).run_capture()?;

            Ok(vec![
                first.stdout_lines()[0].trim().to_string(),
                second.stdout_lines()[0].trim().to_string(),
            ])
        })?;
        let container_ids = container_ids.into_inner();

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
fn test_container_recreated_when_previous_dies() {
    super::runner::run("hello-world", |run| {
        let container_ids = run.run(crate::utils::sandbox_builder(), |build| {
            // Capture the original container's short ID via /etc/hostname.
            let first = build.cmd("cat").args(["/etc/hostname"]).run_capture()?;
            let first_id = first.stdout_lines()[0].trim().to_string();
            assert!(!first_id.is_empty(), "should capture a container ID");

            // Simulate a whole-container OOM (PID 1 killed) by stopping the
            // container externally. The next `docker exec` will fail, and
            // the inspect that follows refreshes the sandbox's cached
            // running flag so the *next* command recreates the container.
            let killed = std::process::Command::new("docker")
                .args(["kill", &first_id])
                .output()
                .expect("failed to spawn docker kill");
            assert!(killed.status.success(), "docker kill failed: {killed:?}");

            // First command after the kill detects the dead state; its
            // result is not load-bearing here.
            let _ = build.cmd("cat").args(["/etc/hostname"]).run_capture();

            // The next command should transparently run in a fresh container.
            let second = build.cmd("cat").args(["/etc/hostname"]).run_capture()?;
            let second_id = second.stdout_lines()[0].trim().to_string();
            assert_ne!(
                first_id, second_id,
                "expected a new container after the previous one died"
            );

            Ok(vec![first_id, second_id])
        })?;
        let container_ids = container_ids.into_inner();

        // Both the killed-and-replaced container and the fresh one should be
        // gone after the build finishes.
        for id in container_ids.iter() {
            assert_container_stopped_and_removed(id);
        }
        Ok(())
    });
}

#[test]
#[cfg(not(windows))]
fn test_reused_container_oom_does_not_poison_later_commands() {
    use rustwide::cmd::CommandError;

    super::runner::run("allocate", |run| {
        run.run(
            crate::utils::sandbox_builder().memory_limit(Some(512 * 1024 * 1024)),
            |build| {
                let first = build.cargo().args(["run", "--", "1024"]).run();
                assert!(
                    matches!(first, Err(CommandError::SandboxOOM)),
                    "expected first command to OOM, got {first:?}"
                );

                dbg!(build.cmd("true").run())?;
                Ok(())
            },
        )?;
        Ok(())
    });
}

#[test]
#[cfg(not(windows))]
fn test_reused_container_timeout_recreates_container() {
    use rustwide::cmd::CommandError;

    super::runner::run("hello-world", |run| {
        let container_ids = run.run(crate::utils::sandbox_builder(), |build| {
            let first = build.cmd("cat").args(["/etc/hostname"]).run_capture()?;
            let first_id = first.stdout_lines()[0].trim().to_string();

            let timed_out = build
                .cmd("sh")
                .args([
                    "-c",
                    "nohup sh -c 'sleep 2; touch /tmp/rustwide-timeout-leak' >/dev/null 2>&1 & sleep 30",
                ])
                .timeout(Some(Duration::from_secs(1)))
                .run();
            assert!(
                matches!(
                    timed_out,
                    Err(CommandError::Timeout(1) | CommandError::NoOutputFor(1))
                ),
                "expected timeout-style error, got {timed_out:?}"
            );

            std::thread::sleep(Duration::from_secs(3));

            let second = build.cmd("cat").args(["/etc/hostname"]).run_capture()?;
            let second_id = second.stdout_lines()[0].trim().to_string();
            assert_ne!(
                first_id, second_id,
                "expected a timed-out command to force a fresh container"
            );

            build.cmd("test")
                .args(["!", "-e", "/tmp/rustwide-timeout-leak"])
                .run()?;

            Ok(vec![first_id, second_id])
        })?;
        let container_ids = container_ids.into_inner();

        for id in container_ids.iter() {
            assert_container_stopped_and_removed(id);
        }
        Ok(())
    });
}

#[test]
#[cfg(not(windows))]
fn test_reused_container_no_output_timeout_recreates_container() {
    use rustwide::cmd::CommandError;

    super::runner::run("hello-world", |run| {
        let container_ids = run.run(crate::utils::sandbox_builder(), |build| {
            let first = build.cmd("cat").args(["/etc/hostname"]).run_capture()?;
            let first_id = first.stdout_lines()[0].trim().to_string();

            let timed_out = build
                .cmd("sh")
                .args([
                    "-c",
                    "nohup sh -c 'sleep 2; touch /tmp/rustwide-no-output-timeout-leak' >/dev/null 2>&1 & sleep 30 >/dev/null 2>&1",
                ])
                .timeout(Some(Duration::from_secs(30)))
                .no_output_timeout(Some(Duration::from_secs(1)))
                .run();
            assert!(
                matches!(timed_out, Err(CommandError::NoOutputFor(1))),
                "expected no-output timeout error, got {timed_out:?}"
            );

            std::thread::sleep(Duration::from_secs(3));

            let second = build.cmd("cat").args(["/etc/hostname"]).run_capture()?;
            let second_id = second.stdout_lines()[0].trim().to_string();
            assert_ne!(
                first_id, second_id,
                "expected a no-output timed-out command to force a fresh container"
            );

            build.cmd("test")
                .args(["!", "-e", "/tmp/rustwide-no-output-timeout-leak"])
                .run()?;

            Ok(vec![first_id, second_id])
        })?;
        let container_ids = container_ids.into_inner();

        for id in container_ids.iter() {
            assert_container_stopped_and_removed(id);
        }
        Ok(())
    });
}

#[test]
fn test_container_cleanup_on_command_failure() {
    super::runner::run("hello-world", |run| {
        let container_id = run.run(crate::utils::sandbox_builder(), |build| {
            // Verify we are running inside a Docker container
            let dockerenv = build.cmd("test").args(["-f", "/.dockerenv"]).run_capture();
            assert!(
                dockerenv.is_ok(),
                "expected to run inside a Docker container"
            );

            let mut container_id = String::new();
            let _err = build
                .cmd("sh")
                .args(["-c", "cat /etc/hostname; exit 1"])
                .process_lines(&mut |line, _| {
                    if container_id.is_empty() {
                        container_id = line.trim().to_string();
                    }
                })
                .run();
            Ok(container_id)
        })?;
        let container_id = container_id.into_inner();

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
