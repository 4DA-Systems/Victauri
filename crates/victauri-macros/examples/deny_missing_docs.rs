//! A consumer that denies `missing_docs`. It only has to compile: `cargo test` and
//! `clippy --all-targets` build it, while `missing_docs` does not run inside a `--test` crate.
#![deny(missing_docs)]

use victauri_macros::inspectable;

/// A public, documented command: `#[inspectable]` must not add an undocumented public item to it.
#[inspectable(description = "Greet someone")]
#[must_use]
pub fn greet(name: String) -> String {
    format!("Hello, {name}")
}

fn main() {
    println!("{}: {}", greet__schema().name, greet("World".to_string()));
}
