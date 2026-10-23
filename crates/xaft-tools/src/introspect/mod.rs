//! Self-introspection: xaft's own documentation and source, embedded in the
//! binary (parity rows 46-50).
//!
//! agenthicc's `tools/introspect` lets the agent read the *current* agenthicc
//! — its `docs/` tree, `llms.txt`, `llms-full.txt`, `README.md`, and any
//! module or symbol of the package — instead of trusting prose in a system
//! prompt that goes stale the moment the code changes. xaft ships the same
//! five tools over its own corpus, renamed for it:
//!
//! | agenthicc | xaft |
//! |---|---|
//! | `list_agenthicc_docs` | `list_xaft_docs` |
//! | `read_agenthicc_doc` | `read_xaft_doc` |
//! | `search_agenthicc_docs` | `search_xaft_docs` |
//! | `inspect_agenthicc_source` | `inspect_xaft_source` |
//! | `search_agenthicc_source` | `search_xaft_source` |
//!
//! # The corpus is compiled in
//!
//! agenthicc reads files from its installed package (or a source checkout,
//! or `AGENTHICC_DOCS_DIR`) and therefore has to resolve every path and refuse
//! anything outside its roots. An xaft binary has no installed source tree, so
//! the root crate's build script packs `docs/**` (`.md`/`.txt`/`.json`),
//! `llms.txt`, `llms-full.txt`, `README.md`, and every `crates/**/*.rs` and
//! `src/**/*.rs` file into a per-file-DEFLATE archive ([`archive`]) and
//! `main.rs` installs it with [`install_embedded_corpus`]. The tools read only
//! that archive: there is no filesystem path to traverse, so containment holds
//! by construction rather than by checking. The cost is the archive's size in
//! the binary (a few MiB; see `docs/guides/tools.md`), and that the answers
//! describe the source the binary was built from — which is the point.
//!
//! A registry built without the binary (library tests, embedders) still
//! registers all five tools; they report that no corpus is embedded, the way
//! agenthicc reports a missing documentation tree.

pub mod archive;
mod docs;
pub mod rust_items;
mod source;
mod tools;

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

pub use archive::{ArchiveWriter, Entry, EntryKind};
pub use tools::{
    INTROSPECT_TOOL_NAMES, InspectXaftSourceTool, ListXaftDocsTool, ReadXaftDocTool,
    SearchXaftDocsTool, SearchXaftSourceTool, introspection_tools,
};

/// Hard ceiling on lines one `read_xaft_doc` call returns (agenthicc's).
pub const MAX_DOC_LINES: usize = 2000;
/// Hard ceiling on matches one search returns (agenthicc's).
pub const MAX_SEARCH_RESULTS: usize = 200;
/// Hard ceiling on source bytes one `inspect_xaft_source` call returns.
pub const MAX_SOURCE_BYTES: usize = 120_000;
/// Longest single line a search hit reports.
pub const MAX_LINE_CHARS: usize = 400;

/// xaft's documentation and source, decoded from an archive.
#[derive(Debug, Clone)]
pub struct Corpus {
    bytes: Cow<'static, [u8]>,
    archive: archive::Archive,
}

impl Corpus {
    /// A corpus over bytes embedded in the binary.
    pub fn from_static(bytes: &'static [u8]) -> Result<Self, String> {
        let archive = archive::Archive::parse(bytes)?;
        Ok(Self {
            bytes: Cow::Borrowed(bytes),
            archive,
        })
    }

    /// A corpus over owned bytes (tests, embedders).
    pub fn from_vec(bytes: Vec<u8>) -> Result<Self, String> {
        let archive = archive::Archive::parse(&bytes)?;
        Ok(Self {
            bytes: Cow::Owned(bytes),
            archive,
        })
    }

    /// Every entry, in archive order (documents first, then source).
    pub fn entries(&self) -> &[Entry] {
        self.archive.entries()
    }

    /// The documentation entries.
    pub fn docs(&self) -> impl Iterator<Item = &Entry> {
        self.entries().iter().filter(|e| e.kind == EntryKind::Doc)
    }

    /// The source entries.
    pub fn sources(&self) -> impl Iterator<Item = &Entry> {
        self.entries()
            .iter()
            .filter(|e| e.kind == EntryKind::Source)
    }

    /// The entry of `kind` published under `path`.
    pub fn find(&self, kind: EntryKind, path: &str) -> Option<&Entry> {
        self.entries()
            .iter()
            .find(|e| e.kind == kind && e.path == path)
    }

    /// An entry's text.
    pub fn text(&self, entry: &Entry) -> Result<String, String> {
        self.archive.text(&self.bytes, entry)
    }

    /// Size of the archive in bytes.
    pub fn archive_bytes(&self) -> usize {
        self.bytes.len()
    }
}

static EMBEDDED: OnceLock<Result<Arc<Corpus>, String>> = OnceLock::new();

/// Install the corpus the binary embeds. Called once by `main.rs`; later
/// calls are ignored. A damaged archive is recorded rather than installed, so
/// the tools can say why they have nothing to read.
pub fn install_embedded_corpus(bytes: &'static [u8]) -> Result<(), String> {
    let parsed = Corpus::from_static(bytes).map(Arc::new);
    let outcome = parsed.as_ref().map(|_| ()).map_err(Clone::clone);
    let _ = EMBEDDED.set(parsed);
    outcome
}

/// The installed corpus, or why there is none.
pub fn embedded_corpus() -> Result<Arc<Corpus>, String> {
    match EMBEDDED.get() {
        Some(Ok(c)) => Ok(Arc::clone(c)),
        Some(Err(e)) => Err(format!("the embedded corpus is damaged ({e})")),
        None => Err("this build embeds no corpus".to_string()),
    }
}

/// agenthicc's `_build_matcher`: a case-insensitive regex when `query`
/// compiles as one, else a case-insensitive substring — so a plain phrase
/// always works. (Rust's regex dialect has no look-around or back-references;
/// such a pattern falls back to a literal match.)
pub(crate) fn matcher(query: &str) -> Box<dyn Fn(&str) -> bool + Send + Sync> {
    match regex::RegexBuilder::new(query)
        .case_insensitive(true)
        .size_limit(1 << 20)
        .build()
    {
        Ok(re) => Box::new(move |line: &str| re.is_match(line)),
        Err(_) => {
            let needle = query.to_lowercase();
            Box::new(move |line: &str| line.to_lowercase().contains(&needle))
        }
    }
}

/// agenthicc's `_search_result`.
pub(crate) fn search_result(
    query: &str,
    hits: Vec<serde_json::Value>,
    scanned: usize,
    truncated: bool,
) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "query": query,
        "files_scanned": scanned,
        "match_count": hits.len(),
        "truncated": truncated,
        "matches": hits,
    })
}

/// `{"ok": false, "error": …, "fix": …}` — agenthicc's failure shape.
pub(crate) fn failure(error: impl Into<String>, fix: impl Into<String>) -> serde_json::Value {
    serde_json::json!({"ok": false, "error": error.into(), "fix": fix.into()})
}

/// Python's `repr()` of a string, for messages agenthicc formats with `!r`.
pub(crate) fn repr(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(test)]
pub(crate) mod test_corpus {
    //! A small corpus shaped like the real one.
    use super::*;

    pub const LIB_RS: &str = "//! The demo crate.\n\
        pub mod net;\n\
        /// Registry of tools.\n\
        pub struct ToolRegistry {\n    tools: Vec<String>,\n}\n\
        impl ToolRegistry {\n    /// An empty registry.\n    pub fn new() -> Self {\n        Self { tools: vec![] }\n    }\n}\n";
    pub const NET_RS: &str =
        "//! Networking.\npub fn connect(host: &str) -> bool {\n    !host.is_empty()\n}\n";

    pub fn corpus() -> Arc<Corpus> {
        let mut w = ArchiveWriter::new(6);
        w.add(EntryKind::Doc, "llms.txt", "# xaft\n\n> overview\n");
        w.add(EntryKind::Doc, "README.md", "# Docs index\n");
        w.add(
            EntryKind::Doc,
            "guides/tools.md",
            "# Tools\n\nThe capability gate filters tools.\nline four\n",
        );
        w.add(
            EntryKind::Doc,
            "guides/workflows.md",
            "# Workflows\n\nPhases advance by tools.\n",
        );
        w.add(
            EntryKind::Doc,
            "reference/cli.md",
            "no heading here\nCAPABILITY GATE again\n",
        );
        w.add(EntryKind::Source, "crates/xaft-demo/src/lib.rs", LIB_RS);
        w.add(EntryKind::Source, "crates/xaft-demo/src/net.rs", NET_RS);
        w.add(
            EntryKind::Source,
            "crates/xaft-demo/tests/api.rs",
            "#[test]\nfn registry_starts_empty() {}\n",
        );
        w.add(
            EntryKind::Source,
            "src/main.rs",
            "//! The binary.\nfn main() {}\n",
        );
        Arc::new(Corpus::from_vec(w.finish()).unwrap())
    }
}
