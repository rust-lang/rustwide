use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    // This prevents Cargo from rebuilding everything each time a non source code file changes.
    println!("cargo:rerun-if-changed=build.rs");

    let target = std::env::var("TARGET")?;

    let output = std::env::var("OUT_DIR")?;
    ::std::fs::write(format!("{}/target", output), target.as_bytes())?;

    println!("cargo:rerun-if-env-changed=DOCS_RS");
    if std::env::var_os("DOCS_RS").as_deref() == Some(std::ffi::OsStr::new("1")) {
        println!("cargo:rustc-cfg=docs_rs");
    }

    Ok(())
}
