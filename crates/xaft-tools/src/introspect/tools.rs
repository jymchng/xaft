//! The five self-introspection tools.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};

use super::{Corpus, docs, embedded_corpus, source};

/// The five tools, in agenthicc's `INTROSPECT_AGENT_TOOLS` order.
pub const INTROSPECT_TOOL_NAMES: [&str; 5] = [
    "list_xaft_docs",
    "read_xaft_doc",
    "search_xaft_docs",
    "inspect_xaft_source",
    "search_xaft_source",
];

/// Which corpus a tool reads: a given one, else the binary's embedded one.
#[derive(Clone, Default)]
struct CorpusSource(Option<Arc<Corpus>>);

impl CorpusSource {
    fn get(&self) -> Result<Arc<Corpus>, String> {
        match &self.0 {
            Some(c) => Ok(Arc::clone(c)),
            None => embedded_corpus(),
        }
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn envelope(value: Value, tool_use_id: &str) -> ToolResult {
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        ToolResult::error(pretty(&value), tool_use_id)
    } else {
        ToolResult::ok(pretty(&value), tool_use_id)
    }
}

fn bad_input(tool: &str, reason: impl Into<String>) -> AgtrsError {
    AgtrsError::ToolCallFailed {
        tool_name: tool.to_string(),
        reason: reason.into(),
    }
}

fn string<'a>(input: &'a Value, field: &str, tool: &str) -> Result<&'a str, AgtrsError> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(""),
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(_) => Err(bad_input(tool, format!("'{field}' must be a string"))),
    }
}

/// agenthicc's `int(value)`: an integer, an integral float or a numeric string.
fn integer(input: &Value, field: &str, default: i64, tool: &str) -> Result<i64, AgtrsError> {
    let value = match input.get(field) {
        None | Some(Value::Null) => return Ok(default),
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => s.trim().parse().ok(),
        Some(_) => None,
    };
    value.ok_or_else(|| bad_input(tool, format!("'{field}' must be an integer")))
}

fn boolean(input: &Value, field: &str, default: bool, tool: &str) -> Result<bool, AgtrsError> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("true") => Ok(true),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("false") => Ok(false),
        Some(_) => Err(bad_input(tool, format!("'{field}' must be a boolean"))),
    }
}

macro_rules! introspection_tool {
    (
        $(#[$meta:meta])*
        $ty:ident, $name:literal, $description:expr, $schema:expr,
        |$corpus:ident, $input:ident| $body:expr, $unavailable:path
    ) => {
        $(#[$meta])*
        #[derive(Clone, Default)]
        pub struct $ty {
            corpus: CorpusSource,
        }

        impl $ty {
            /// The tool over the binary's embedded corpus.
            pub fn new() -> Self {
                Self::default()
            }

            /// The tool over `corpus` (tests, embedders).
            pub fn with_corpus(corpus: Arc<Corpus>) -> Self {
                Self {
                    corpus: CorpusSource(Some(corpus)),
                }
            }
        }

        impl std::fmt::Debug for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($ty)).finish_non_exhaustive()
            }
        }

        #[async_trait]
        impl Tool for $ty {
            type Inputs = Value;
            type Output = ToolResult;

            fn name(&self) -> &str {
                $name
            }

            fn description(&self) -> &str {
                $description
            }

            fn schema(&self) -> Value {
                $schema
            }

            /// Read-only and bounded: safe to run alongside other reads.
            fn parallel_safe(&self) -> bool {
                true
            }

            async fn call(&self, $input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
                if ctx.is_cancelled() {
                    return Err(AgtrsError::Cancelled {
                        reason: format!("{} cancelled before it started", $name),
                    });
                }
                let value = match self.corpus.get() {
                    Ok($corpus) => {
                        let $corpus: &Corpus = &$corpus;
                        $body
                    }
                    Err(reason) => $unavailable(&reason),
                };
                Ok(envelope(value, &ctx.tool_use_id))
            }
        }
    };
}

introspection_tool!(
    /// `list_xaft_docs` — agenthicc's `list_agenthicc_docs`.
    ListXaftDocsTool,
    "list_xaft_docs",
    "List xaft's own documentation: path, title, line count and size of every document. \
     Start here when you need to know how xaft itself works — its architecture, workflows, \
     tools, memory, security, configuration or CLI. The listing includes the guides and \
     reference pages plus llms.txt, llms-full.txt (the complete reference) and README.md. \
     Read one with read_xaft_doc(path), or jump straight to a phrase with \
     search_xaft_docs(query).",
    json!({
        "type": "object",
        "properties": {
            "section": {"type": "string", "default": "",
                        "description": "Optional path prefix to filter by, e.g. \"guides\" or \"reference\"."}
        }
    }),
    |corpus, input| docs::list_result(corpus, string(&input, "section", "list_xaft_docs")?),
    docs::unavailable
);

introspection_tool!(
    /// `read_xaft_doc` — agenthicc's `read_agenthicc_doc`.
    ReadXaftDocTool,
    "read_xaft_doc",
    "Read one xaft documentation page, a bounded window at a time. Use a path from \
     list_xaft_docs, e.g. \"guides/workflows.md\", \"reference/cli.md\" or \"llms-full.txt\". \
     When the result reports truncated=true, call again with start_line=next_start_line.",
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "Document path relative to the documentation root."},
            "start_line": {"type": "integer", "default": 1, "description": "1-based first line to return (default 1)."},
            "max_lines": {"type": "integer", "default": 400, "description": "Maximum lines to return (default 400, hard cap 2000)."}
        },
        "required": ["path"]
    }),
    |corpus, input| {
        let tool = "read_xaft_doc";
        let path = match input.get("path").and_then(Value::as_str) {
            Some(p) => p,
            None => return Err(bad_input(tool, "required field 'path' is missing or not a string")),
        };
        docs::read(
            corpus,
            path,
            integer(&input, "start_line", 1, tool)?,
            integer(&input, "max_lines", 400, tool)?,
        )
    },
    docs::unavailable
);

introspection_tool!(
    /// `search_xaft_docs` — agenthicc's `search_agenthicc_docs`.
    SearchXaftDocsTool,
    "search_xaft_docs",
    "Search xaft's documentation for a phrase and return matching lines. Faster than \
     reading whole pages when you know what you are looking for — \"capability gate\", \
     \"on_reject\", \"browser_backend\". Each match reports its document path and line \
     number, so follow up with read_xaft_doc(path, start_line=<line>).",
    json!({
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": "Regular expression, or a plain phrase when it is not valid regex."},
            "max_results": {"type": "integer", "default": 40, "description": "Maximum matches to return (default 40, hard cap 200)."},
            "section": {"type": "string", "default": "", "description": "Optional path prefix to restrict the search to."}
        },
        "required": ["query"]
    }),
    |corpus, input| {
        let tool = "search_xaft_docs";
        docs::search(
            corpus,
            string(&input, "query", tool)?,
            integer(&input, "max_results", 40, tool)?,
            string(&input, "section", tool)?,
        )
    },
    docs::unavailable
);

introspection_tool!(
    /// `inspect_xaft_source` — agenthicc's `inspect_agenthicc_source`.
    InspectXaftSourceTool,
    "inspect_xaft_source",
    "Read the source of any xaft module, type, function or method. The authoritative \
     answer to \"what does this actually do\" and \"what fields does this really have\". \
     Accepts a module (\"xaft_runtime::phased\"), an item (\"xaft_tools::registry:ToolRegistryBuilder\"), \
     a method (\"xaft_tools::registry:ToolRegistryBuilder::build_coder\"), the all-`::` form, \
     or a file path (\"crates/xaft-tools/tests/tool_parity_reachability.rs:coder_registry\"). \
     Returns the file, line range, signature, doc comment, source and — for a module or \
     type — an outline of its members. Nothing is compiled. Pass include_source=false \
     for a cheap outline of a large module first.",
    json!({
        "type": "object",
        "properties": {
            "target": {"type": "string", "description": "Module path, optionally with \":Item\" or \":Type::method\"; or a source file path."},
            "include_source": {"type": "boolean", "default": true, "description": "Include the source text (default true)."}
        },
        "required": ["target"]
    }),
    |corpus, input| {
        let tool = "inspect_xaft_source";
        source::inspect(
            corpus,
            string(&input, "target", tool)?,
            boolean(&input, "include_source", true, tool)?,
        )
    },
    source::unavailable
);

introspection_tool!(
    /// `search_xaft_source` — agenthicc's `search_agenthicc_source`.
    SearchXaftSourceTool,
    "search_xaft_source",
    "Search xaft's own source code for a pattern and return matching lines. Use this to \
     locate a symbol before inspecting it, or to find every call site of something. Each \
     match reports a repository-relative path and line number; follow up with \
     inspect_xaft_source to read the definition.",
    json!({
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": "Regular expression, or a plain substring when it is not valid regex."},
            "max_results": {"type": "integer", "default": 40, "description": "Maximum matches to return (default 40, hard cap 200)."},
            "module": {"type": "string", "default": "", "description": "Optional crate, module or path to restrict the search to, e.g. \"xaft_runtime::phased\" or \"crates/xaft-tools\"."}
        },
        "required": ["query"]
    }),
    |corpus, input| {
        let tool = "search_xaft_source";
        source::search(
            corpus,
            string(&input, "query", tool)?,
            integer(&input, "max_results", 40, tool)?,
            string(&input, "module", tool)?,
        )
    },
    source::unavailable
);

/// All five tools, in [`INTROSPECT_TOOL_NAMES`] order, over `corpus` — or,
/// when `None`, over the corpus the binary installed with
/// [`super::install_embedded_corpus`].
pub fn introspection_tools(corpus: Option<Arc<Corpus>>) -> Vec<Arc<ErasedTool>> {
    let source = CorpusSource(corpus);
    vec![
        Arc::new(ListXaftDocsTool {
            corpus: source.clone(),
        }) as Arc<ErasedTool>,
        Arc::new(ReadXaftDocTool {
            corpus: source.clone(),
        }),
        Arc::new(SearchXaftDocsTool {
            corpus: source.clone(),
        }),
        Arc::new(InspectXaftSourceTool {
            corpus: source.clone(),
        }),
        Arc::new(SearchXaftSourceTool { corpus: source }),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::test_corpus::corpus;
    use super::*;

    async fn run<T: Tool<Inputs = Value, Output = ToolResult>>(
        tool: &T,
        input: Value,
    ) -> (Value, bool) {
        let r = tool.call(input, &ToolContext::new("c")).await.unwrap();
        (serde_json::from_str(&r.content).unwrap(), r.is_error)
    }

    #[tokio::test]
    async fn the_tools_answer_from_their_corpus() {
        let c = corpus();
        let (list, err) = run(
            &ListXaftDocsTool::with_corpus(Arc::clone(&c)),
            json!({"section": "guides"}),
        )
        .await;
        assert!(!err);
        assert_eq!(list["count"], json!(2));
        let (doc, _) = run(
            &ReadXaftDocTool::with_corpus(Arc::clone(&c)),
            json!({"path": "guides/tools.md", "start_line": "1", "max_lines": 1.0}),
        )
        .await;
        assert_eq!(doc["content"], json!("# Tools"));
        let (hits, _) = run(
            &SearchXaftDocsTool::with_corpus(Arc::clone(&c)),
            json!({"query": "gate"}),
        )
        .await;
        assert_eq!(hits["match_count"], json!(2));
        let (item, _) = run(
            &InspectXaftSourceTool::with_corpus(Arc::clone(&c)),
            json!({"target": "xaft_demo::ToolRegistry", "include_source": false}),
        )
        .await;
        assert_eq!(item["kind"], json!("struct"));
        let (found, _) = run(
            &SearchXaftSourceTool::with_corpus(c),
            json!({"query": "connect"}),
        )
        .await;
        assert_eq!(found["match_count"], json!(1));
    }

    #[tokio::test]
    async fn failures_are_results_and_bad_arguments_are_errors() {
        let c = corpus();
        let (missing, is_error) = run(
            &ReadXaftDocTool::with_corpus(Arc::clone(&c)),
            json!({"path": "nope.md"}),
        )
        .await;
        assert!(is_error);
        assert_eq!(missing["ok"], json!(false));
        let read = ReadXaftDocTool::with_corpus(Arc::clone(&c));
        assert!(read.call(json!({}), &ToolContext::new("c")).await.is_err());
        assert!(
            read.call(
                json!({"path": "x", "max_lines": "lots"}),
                &ToolContext::new("c")
            )
            .await
            .is_err()
        );
        let inspect = InspectXaftSourceTool::with_corpus(c);
        assert!(
            inspect
                .call(
                    json!({"target": "x", "include_source": 3}),
                    &ToolContext::new("c")
                )
                .await
                .is_err()
        );
    }

    #[test]
    fn names_and_capabilities_match_the_plan() {
        use crate::capabilities::{ToolCapability, capability_of};
        let tools = introspection_tools(None);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, INTROSPECT_TOOL_NAMES);
        for tool in &tools {
            let cap = capability_of(tool.name());
            assert!(cap.is_read_only(), "{} is {cap}", tool.name());
            assert!(
                !tool.requires_confirmation(),
                "{} is read-only",
                tool.name()
            );
        }
        assert_eq!(capability_of("search_xaft_docs"), ToolCapability::Search);
        assert_eq!(capability_of("search_xaft_source"), ToolCapability::Search);
        assert_eq!(capability_of("inspect_xaft_source"), ToolCapability::Read);
    }

    /// Without a corpus — a library build, a test — the tools still exist and
    /// say why they have nothing to read.
    #[tokio::test]
    async fn without_a_corpus_the_tools_explain_themselves() {
        let tools = introspection_tools(None);
        if super::super::embedded_corpus().is_ok() {
            return; // another test in this process installed one
        }
        let r = tools[0]
            .call(json!({}), &ToolContext::new("c"))
            .await
            .unwrap();
        assert!(r.is_error);
        let body: Value = serde_json::from_str(&r.content).unwrap();
        assert!(
            body["error"].as_str().unwrap().contains("embeds no corpus"),
            "{body}"
        );
        let r = tools[3]
            .call(json!({"target": "xaft"}), &ToolContext::new("c"))
            .await
            .unwrap();
        let body: Value = serde_json::from_str(&r.content).unwrap();
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .starts_with("No xaft source"),
            "{body}"
        );
    }
}
