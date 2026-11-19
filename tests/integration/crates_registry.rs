use flate2::{Compression, write::GzEncoder};
use rustwide::{Crate, PrepareError};

#[test]
fn test_tarball_escaping_source_dir_fails() -> anyhow::Result<()> {
    let workspace = crate::utils::init_workspace()?;
    let host = tempfile::tempdir()?;

    // A symlink to a host directory, followed by a file written through it.
    let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    tar.append_link(&mut header, "evil-1.0.0/link", host.path())?;
    let mut header = tar::Header::new_gnu();
    header.set_size(0);
    tar.append_data(&mut header, "evil-1.0.0/link/file", &[][..])?;
    let tarball = tar.into_inner()?.finish()?;

    let mut server = mockito::Server::new();
    let _config = server
        .mock("GET", "/index/config.json")
        .with_body(format!(r#"{{"dl":"{}/dl"}}"#, server.url()))
        .create();
    let _crate = server
        .mock("GET", "/dl/evil/1.0.0/download")
        .with_body(tarball)
        .create();

    let index = format!("{}/index/", server.url());
    let krate = Crate::sparse_registry(index.as_str(), "evil", "1.0.0")?;
    krate.fetch(&workspace)?;
    let err = krate
        .copy_source_to(&workspace, &host.path().join("source"))
        .expect_err("a tarball with a symlink should not be unpacked");
    krate.purge_from_cache(&workspace)?;

    assert!(
        matches!(
            err.downcast_ref::<PrepareError>(),
            Some(PrepareError::InvalidArchiveEntry { .. })
        ),
        "unexpected error: {err:?}"
    );
    assert!(!host.path().join("file").exists());
    assert!(!host.path().join("source").exists());

    Ok(())
}
