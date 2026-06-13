use log::LevelFilter;
use rustwide::{
    Workspace, WorkspaceBuilder,
    cmd::{SandboxBuilder, SandboxImage},
};
use std::path::{Path, PathBuf};

static USER_AGENT: &str = "rustwide-tests (https://github.com/rust-lang/rustwide)";

pub(crate) fn workspace_path(name: &str) -> PathBuf {
    Path::new(".workspaces").join(name)
}

pub(crate) fn init_workspace() -> anyhow::Result<Workspace> {
    init_named_workspace("integration")
}

pub(crate) fn init_named_workspace(name: &str) -> anyhow::Result<Workspace> {
    init_logs();
    let workspace_path = workspace_path(name);
    let mut builder = WorkspaceBuilder::new(&workspace_path, USER_AGENT).fast_init(true);

    if std::env::var("RUSTWIDE_TEST_INSIDE_DOCKER").is_ok() {
        builder = builder.running_inside_docker(true);
    }

    // Use the micro image when running tests on Linux, speeding them up.
    if cfg!(target_os = "linux") {
        builder = builder.sandbox_image(SandboxImage::remote(
            "ghcr.io/rust-lang/crates-build-env/linux-micro",
        )?);
    }

    builder.init()
}

#[allow(dead_code)]
pub(crate) fn sandbox_builder() -> SandboxBuilder {
    configure_sandbox_builder(SandboxBuilder::new().enable_networking(false))
}

pub(crate) fn configure_sandbox_builder(builder: SandboxBuilder) -> SandboxBuilder {
    let Ok(runtime) = std::env::var("RUSTWIDE_DOCKER_RUNTIME") else {
        return builder;
    };
    builder.docker_runtime(runtime.parse().expect("invalid RUSTWIDE_DOCKER_RUNTIME"))
}

fn init_logs() {
    let env = env_logger::Builder::new()
        .filter_module("rustwide", LevelFilter::Info)
        .format_timestamp(None)
        .is_test(true)
        .build();
    rustwide::logging::init_with(env);
}

#[macro_export]
macro_rules! os_string {
    ($val:expr $(, $push:expr)*) => {{
        let mut string = std::ffi::OsString::from($val);
        $(string.push($push);)*
        string
    }}
}
