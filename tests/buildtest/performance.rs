//! Manual performance comparisons for sandbox runtimes.
//!
//! Run with `just run-engine-performance-tests`.

use rustwide::cmd::{DockerRuntime, SandboxBuilder};
use std::time::{Duration, Instant};

const SAMPLES: usize = 3;

fn measure(runtime: DockerRuntime) -> Duration {
    let started = Instant::now();

    crate::buildtest::runner::run("with-dependencies", |run| {
        run.run(
            SandboxBuilder::new()
                .enable_networking(false)
                .docker_runtime(runtime),
            |build| {
                build.cargo().args(["build", "--release"]).run()?;
                Ok(())
            },
        )?;
        Ok(())
    });

    started.elapsed()
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

#[test]
#[ignore = "manual performance comparison; requires Docker and gVisor's runsc runtime"]
fn compare_default_and_runsc() {
    // Prime the image, toolchain, and Cargo caches. The samples below then focus
    // on the build and sandbox runtime rather than one-time setup work.
    for runtime in [DockerRuntime::Default, DockerRuntime::Runsc] {
        let _ = measure(runtime);
    }

    let mut result = Vec::new();

    for runtime in [DockerRuntime::Default, DockerRuntime::Runsc] {
        let samples = (0..SAMPLES).map(|_| measure(runtime)).collect();
        result.push((runtime, median(samples)));
    }

    for (runtime, median) in result {
        eprintln!("{runtime:>7} median of {SAMPLES} runs: {:?}", median);
    }
}
