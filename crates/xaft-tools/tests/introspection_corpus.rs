//! The self-introspection tools (parity rows 46-50) over xaft's own checkout.
//!
//! The binary embeds an archive `build.rs` packs with
//! `introspect::archive::pack_checkout`; this test packs the same checkout the
//! same way and drives all five tools through a built `ToolRegistry`, so the
//! answers asserted here are the answers an agent gets about this code.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use agtrs_runtime::tool::ToolContext;
use serde_json::{Value, json};
use tempfile::TempDir;
use xaft_tools::introspect::{EntryKind, archive::pack_checkout, rust_items};
use xaft_tools::{Corpus, ToolRegistryBuilder};

fn checkout() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn corpus() -> Arc<Corpus> {
    static CORPUS: OnceLock<Arc<Corpus>> = OnceLock::new();
    Arc::clone(CORPUS.get_or_init(|| {
        Arc::new(Corpus::from_vec(pack_checkout(checkout(), 1).finish()).expect("valid archive"))
    }))
}

async fn call(name: &str, input: Value) -> Value {
    let tmp = TempDir::new().unwrap();
    let registry = ToolRegistryBuilder::new(tmp.path())
        .without_git()
        .with_introspection_corpus(corpus())
        .build_reader()
        .unwrap();
    let tool = registry
        .get(name)
        .unwrap_or_else(|| panic!("{name} is not registered"));
    let result = tool.call(input, &ToolContext::new("t")).await.unwrap();
    let body: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(
        result.is_error,
        body["ok"] == json!(false),
        "is_error mirrors ok: {body}"
    );
    body
}

#[test]
fn the_checkout_packs_its_docs_and_every_rust_file() {
    let c = corpus();
    for doc in [
        "llms.txt",
        "llms-full.txt",
        "README.md",
        "guides/tools.md",
        "parity-matrix.md",
    ] {
        assert!(c.find(EntryKind::Doc, doc).is_some(), "{doc}");
    }
    let sources: Vec<&str> = c.sources().map(|e| e.path.as_str()).collect();
    assert!(sources.contains(&"src/main.rs"));
    assert!(sources.contains(&"crates/xaft-tools/src/introspect/rust_items.rs"));
    assert!(sources.contains(&"crates/xaft-tools/tests/introspection_corpus.rs"));
    assert!(sources.iter().all(|p| p.ends_with(".rs")));
    assert!(!sources.iter().any(|p| p.contains("/target/")));
}

/// The outliner never panics on real code and finds the items a reader would.
#[test]
fn every_source_file_outlines() {
    let c = corpus();
    let mut items = 0usize;
    for entry in c.sources() {
        let text = c.text(entry).unwrap();
        let outline = rust_items::outline(&text);
        items += outline.items.len();
        for item in &outline.items {
            assert!(
                item.start_line <= item.end_line && item.end_line <= entry.lines.max(1) as usize,
                "{}: {} spans {}..{} of {} lines",
                entry.path,
                item.name,
                item.start_line,
                item.end_line,
                entry.lines
            );
        }
    }
    assert!(items > 2_000, "only {items} items across the workspace");
}

#[tokio::test]
async fn inspect_reads_real_symbols_in_every_spelling() {
    let method = call(
        "inspect_xaft_source",
        json!({"target": "xaft_tools::registry:ToolRegistryBuilder::build_coder"}),
    )
    .await;
    assert_eq!(method["path"], json!("crates/xaft-tools/src/registry.rs"));
    assert_eq!(method["kind"], json!("function"));
    assert!(
        method["signature"]
            .as_str()
            .unwrap()
            .starts_with("pub fn build_coder(self)")
    );
    assert!(
        method["source"]
            .as_str()
            .unwrap()
            .contains("introspection_tools(corpus)")
    );

    let ty = call(
        "inspect_xaft_source",
        json!({"target": "xaft_tools.shell.run_tests.RunTestsTool", "include_source": false}),
    )
    .await;
    assert_eq!(ty["kind"], json!("struct"));
    let members: Vec<&str> = ty["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    for expected in ["executor", "new", "call"] {
        assert!(members.contains(&expected), "{expected} in {members:?}");
    }

    let module = call(
        "inspect_xaft_source",
        json!({"target": "xaft-runtime::auth", "include_source": false}),
    )
    .await;
    assert_eq!(module["path"], json!("crates/xaft-runtime/src/auth/mod.rs"));
    assert!(module["docstring"].as_str().unwrap().contains("PKCE"));
    assert!(module.get("source").is_none());

    let binary = call(
        "inspect_xaft_source",
        json!({"target": "xaft:bootstrap_runtime"}),
    )
    .await;
    assert_eq!(binary["path"], json!("src/main.rs"));
    assert_eq!(binary["kind"], json!("async function"));
}

#[tokio::test]
async fn docs_tools_read_the_real_documentation() {
    let list = call("list_xaft_docs", json!({"section": "guides"})).await;
    let paths: Vec<&str> = list["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"guides/tools.md"));
    assert!(paths.iter().all(|p| p.starts_with("guides/")));

    let hits = call(
        "search_xaft_docs",
        json!({"query": "browser_backend", "max_results": 5}),
    )
    .await;
    assert!(hits["match_count"].as_u64().unwrap() >= 1);

    let page = call("read_xaft_doc", json!({"path": "llms.txt", "max_lines": 3})).await;
    assert_eq!(page["start_line"], json!(1));
    assert_eq!(page["end_line"], json!(3));
}

#[tokio::test]
async fn source_search_finds_the_parity_tools() {
    let hits = call(
        "search_xaft_source",
        json!({"query": r#""inspect_xaft_source""#, "module": "xaft_tools"}),
    )
    .await;
    let paths: Vec<&str> = hits["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["path"].as_str().unwrap())
        .collect();
    assert!(
        paths.iter().all(|p| p.starts_with("crates/xaft-tools/")),
        "{paths:?}"
    );
    assert!(paths.contains(&"crates/xaft-tools/src/introspect/tools.rs"));
}
