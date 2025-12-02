use std::{env, error::Error, fs};

fn main() -> Result<(), Box<dyn Error>> {
    // This prevents Cargo from rebuilding everything each time a non source code file changes.
    println!("cargo:rerun-if-changed=build.rs");

    let target = env::var("TARGET")?;
    let output = env::var("OUT_DIR")?;
    fs::write(format!("{output}/target"), target.as_bytes())?;

    Ok(())
}
