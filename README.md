# Rustwide

Rustwide is a library to execute your code on the Rust ecosystem, powering
projects like [Crater] and [docs.rs]. It features:

* Linux and Windows support.
* Sandboxing by default using Docker containers, with the option to restrict
  network access during builds while still supporting most of the crates.
* [Curated build environment][build-env] to build a large part of the
  ecosystem, built from the experience gathered running [Crater] and [docs.rs].

Rustwide was originally part of the [Crater] project, and it was extracted to
let the whole community benefit from it.

Rustwide is licensed under both the MIT and Apache 2.0 licenses, allowing you
to choose which one to adhere to.

[Crater]: https://github.com/rust-lang/crater
[docs.rs]: https://github.com/rust-lang/docs.rs
[build-env]: https://github.com/rust-lang/crates-build-env
