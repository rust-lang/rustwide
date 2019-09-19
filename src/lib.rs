#![warn(missing_docs)]
#![allow(clippy::new_without_default)]

//! Rustwide is a library to execute your code on the Rust ecosystem, powering projects like
//! [Crater][crater] and [docs.rs][docsrs].
//!
//! Rustwide uses feature flags to enable or disable parts of its code at compile time. The
//! available flags are:
//!
//! * **unstable**: allow Rustwide to use unstable Rust and Cargo features. While this feature also
//!   works on Rust stable it might cause Rustwide to break, and **no stability guarantee is
//!   present when using it!**
//!
//! [crater]: https://github.com/rust-lang/crater
//! [docsrs]: https://github.com/rust-lang/docs.rs

#[cfg(test)]
#[macro_use]
extern crate toml;

mod build;
pub mod cmd;
mod crates;
mod inside_docker;
pub mod logging;
mod native;
mod prepare;
mod toolchain;
mod tools;
mod utils;
mod workspace;

pub use crate::build::{Build, BuildBuilder, BuildDirectory};
pub use crate::crates::Crate;
pub use crate::prepare::PrepareError;
pub use crate::toolchain::Toolchain;
pub use crate::workspace::{Workspace, WorkspaceBuilder};

pub(crate) static HOST_TARGET: &str = include_str!(concat!(env!("OUT_DIR"), "/target"));
