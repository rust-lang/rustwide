use failure::Error;
use rustwide::cmd::Command;

mod utils;

#[test]
fn run_binary_with_same_name_as_file() -> Result<(), Error> {
    use std::fs;

    let tmpdir = tempfile::tempdir()?;
    std::env::set_current_dir(&tmpdir)?;
    fs::write("true", b"foobar")?;
    let workspace = crate::utils::init_workspace()?;
    Command::new(&workspace, "true").run()?;

    Ok(())
}
