use failure::Error;
use rustwide::Crate;

const INDEX_URL: &str = "https://github.com/rust-lang/staging.crates.io-index";

#[test]
fn test_fetch() -> Result<(), Error> {
    let workspace = crate::utils::init_workspace()?;

    let krate = Crate::registry(INDEX_URL, "foo", "0.4.0");
    krate.fetch(&workspace)?;

    Ok(())
}
