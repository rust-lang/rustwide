use failure::Error;
use log::LevelFilter;
use rustwide::{Workspace, WorkspaceBuilder};
use std::path::Path;

static USER_AGENT: &str = "rustwide-tests (https://github.com/rust-lang/rustwide)";

pub(crate) fn init_workspace() -> Result<Workspace, Error> {
    init_logs();
    let workspace_path = Path::new(".workspaces").join("integration");
    Ok(WorkspaceBuilder::new(&workspace_path, USER_AGENT)
        .fast_init(true)
        .init()?)
}

fn init_logs() {
    let env = env_logger::Builder::new()
        .filter_module("rustwide", LevelFilter::Info)
        .default_format_timestamp(false)
        .is_test(true)
        .build();
    rustwide::logging::init_with(env);
}
