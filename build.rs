//! Embeds xaft's documentation and Rust source for the self-introspection
//! tools (`list_xaft_docs`, `read_xaft_doc`, `search_xaft_docs`,
//! `inspect_xaft_source`, `search_xaft_source` — parity rows 46-50).
//!
//! The archive is written to `$OUT_DIR/xaft-corpus.bin` and embedded by
//! `src/main.rs`. It is built here, in the binary crate, rather than in
//! `xaft-tools`: the corpus changes whenever any source file does, and
//! embedding it in a library would make every edit anywhere rebuild that
//! library and everything that depends on it. Here an edit costs a re-run of
//! this script and a re-link of the binary, which happen anyway.
//!
//! The format code is `xaft-tools`'s own, included by path, so the writer
//! and the reader are one implementation.

#[allow(dead_code)]
#[path = "crates/xaft-tools/src/introspect/archive.rs"]
mod archive;

use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("xaft-corpus.bin");
    for watched in [
        "docs",
        "crates",
        "src",
        "llms.txt",
        "llms-full.txt",
        "README.md",
    ] {
        println!("cargo:rerun-if-changed={watched}");
    }
    let writer = archive::pack_checkout(&root, 6);
    let entries = writer.len();
    let bytes = writer.finish();
    std::fs::write(&out, &bytes).expect("write the introspection corpus");
    println!("cargo:rustc-env=XAFT_CORPUS_ENTRIES={entries}");
}
