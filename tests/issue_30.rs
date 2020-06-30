use failure::Error;
use rustwide::cmd::Command;
use rustwide::WorkspaceBuilder;

const USER_AGENT: &str = "rustwide-tests (https://github.com/rust-lang/rustwide)";

#[test]
fn run_binary_with_same_name_as_file() -> Result<(), Error> {
	use std::fs;

    let env = env_logger::Builder::new()
        .filter_module("rustwide", log::LevelFilter::Info)
        .default_format_timestamp(false)
        .is_test(true)
        .build();
    rustwide::logging::init_with(env);
	let tmpdir = tempfile::tempdir()?;
	std::env::set_current_dir(&tmpdir)?;
	fs::write("true", b"foobar")?;
	let workspace = WorkspaceBuilder::new(tempfile::tempdir()?.path(), USER_AGENT).fast_init(true).init()?;
	Command::new(&workspace, "true").run()?;

	Ok(())
}
