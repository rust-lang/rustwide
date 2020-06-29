use failure::Error;

const WORKSPACE_NAME: &str = "test-drop";

#[test]
fn test_build_dir_drop() -> Result<(), Error> {
    let workspace = crate::utils::init_named_workspace(WORKSPACE_NAME)?;
	let mut build_dir = workspace.build_dir("drop-dir");
	build_dir.purge_on_drop(true);
	let path = build_dir.build_dir();
	drop(build_dir);
	assert!(!path.exists());
	Ok(())
}
