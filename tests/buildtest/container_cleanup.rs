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
