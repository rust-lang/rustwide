use rustwide::{AlternativeRegistry, Crate};

const INDEX_URL: &str = "https://github.com/rust-lang/staging.crates.io-index";

#[test]
fn test_fetch() -> anyhow::Result<()> {
    let workspace = crate::utils::init_workspace()?;

    let alt = AlternativeRegistry::new(INDEX_URL);
    let krate = Crate::registry(alt, "foo", "0.4.0");
    krate.fetch(&workspace)?;

    Ok(())
}
