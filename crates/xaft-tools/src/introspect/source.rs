//! `inspect_xaft_source` and `search_xaft_source` — agenthicc's
//! `resolve_source` and `search_source`, for Rust.
//!
//! agenthicc addresses its code by dotted module path (`agenthicc.kernel`,
//! `agenthicc.workflows.plugin:PhaseSpec.build_runner`, or the all-dots form).
//! xaft accepts the Rust spelling of the same three forms — `xaft_tools::shell`,
//! `xaft_tools::registry:ToolRegistryBuilder::build_coder`, the all-`::` form —
//! with `.` accepted wherever `::` is, crate names with `-` or `_`, and also a
//! corpus file path (`crates/xaft-tools/tests/foo.rs:helper`) for files no
//! module path names, such as integration tests.

use serde_json::{Value, json};

use super::rust_items::{self, Item, clip};
use super::{
    Corpus, Entry, EntryKind, MAX_LINE_CHARS, MAX_SEARCH_RESULTS, MAX_SOURCE_BYTES, failure,
    matcher, repr, search_result,
};

/// The failure for a build without source.
pub fn unavailable(reason: &str) -> Value {
    failure(
        format!("No xaft source is available in this installation: {reason}."),
        "Use an xaft binary built from a source checkout (its build script embeds crates/ and \
         src/), or read the files with read_file.",
    )
}

/// The crate a corpus path belongs to and its directory prefix:
/// `crates/xaft-tools/…` → (`xaft_tools`, `crates/xaft-tools/`); `src/…` →
/// (`xaft`, ``).
fn crate_of(path: &str) -> Option<(String, String)> {
    if let Some(rest) = path.strip_prefix("crates/") {
        let dir = rest.split('/').next()?;
        return Some((dir.replace('-', "_"), format!("crates/{dir}/")));
    }
    path.starts_with("src/")
        .then(|| ("xaft".to_string(), String::new()))
}

/// The module path of a `src/` file, or `None` for tests, benches, build
/// scripts and binaries.
pub fn module_of(path: &str) -> Option<String> {
    let (krate, prefix) = crate_of(path)?;
    let rel = path.strip_prefix(&prefix)?.strip_prefix("src/")?;
    let rel = rel.strip_suffix(".rs")?;
    let mut parts: Vec<&str> = rel.split('/').collect();
    if parts.first() == Some(&"bin") {
        return None;
    }
    if parts.len() == 1 && matches!(parts[0], "lib" | "main") {
        parts.clear();
    } else if parts.last() == Some(&"mod") {
        parts.pop();
    }
    Some(
        std::iter::once(krate.as_str())
            .chain(parts)
            .collect::<Vec<_>>()
            .join("::"),
    )
}

/// The crates the corpus knows, by module name.
fn crate_names(corpus: &Corpus) -> Vec<(String, String)> {
    let mut names: Vec<(String, String)> =
        corpus.sources().filter_map(|e| crate_of(&e.path)).collect();
    names.sort();
    names.dedup();
    names
}

/// The file that defines module `parts` (`["xaft_tools", "shell"]`).
fn module_file<'a>(corpus: &'a Corpus, parts: &[String]) -> Option<&'a Entry> {
    let (krate, rest) = parts.split_first()?;
    let krate = krate.replace('-', "_");
    let (_, prefix) = crate_names(corpus).into_iter().find(|(n, _)| *n == krate)?;
    let candidates = if rest.is_empty() {
        vec![
            format!("{prefix}src/lib.rs"),
            format!("{prefix}src/main.rs"),
        ]
    } else {
        let rel = rest.join("/");
        vec![
            format!("{prefix}src/{rel}.rs"),
            format!("{prefix}src/{rel}/mod.rs"),
        ]
    };
    candidates
        .iter()
        .find_map(|p| corpus.find(EntryKind::Source, p))
}

fn split_path(text: &str) -> Vec<String> {
    text.replace('.', "::")
        .split("::")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Position of a lone `:` (agenthicc's `module:Symbol` separator), as
/// opposed to a `::` path separator.
fn lone_colon(text: &str) -> Option<usize> {
    let b = text.as_bytes();
    (0..b.len()).find(|&i| {
        b[i] == b':' && (i == 0 || b[i - 1] != b':') && b.get(i + 1).is_none_or(|c| *c != b':')
    })
}

/// A resolved target: the file, how to name its module, and the symbol path.
struct Target<'a> {
    entry: &'a Entry,
    module: String,
    symbol: Vec<String>,
}

enum Resolve<'a> {
    Found(Target<'a>),
    NotXaft,
    NoModule,
}

fn resolve<'a>(corpus: &'a Corpus, target: &str) -> Resolve<'a> {
    // A file path, optionally with `:Symbol`.
    if let Some(i) = target.find(".rs") {
        let (file, rest) = target.split_at(i + 3);
        if file.contains('/') || rest.is_empty() || rest.starts_with(':') {
            let file = file.trim().trim_start_matches("./");
            let Some(entry) = corpus.find(EntryKind::Source, file) else {
                return if crate_of(file).is_some() {
                    Resolve::NoModule
                } else {
                    Resolve::NotXaft
                };
            };
            return Resolve::Found(Target {
                entry,
                module: module_of(file).unwrap_or_else(|| file.to_string()),
                symbol: split_path(rest.trim_start_matches(':')),
            });
        }
    }

    let (module_part, symbol_part) = match lone_colon(target) {
        Some(i) => (&target[..i], Some(&target[i + 1..])),
        None => (target, None),
    };
    let parts = split_path(module_part);
    let Some(first) = parts.first() else {
        return Resolve::NoModule;
    };
    let krate = first.replace('-', "_");
    if !crate_names(corpus).iter().any(|(n, _)| *n == krate) {
        return Resolve::NotXaft;
    }
    if let Some(symbol) = symbol_part {
        return match module_file(corpus, &parts) {
            Some(entry) => Resolve::Found(Target {
                entry,
                module: normalise(&parts),
                symbol: split_path(symbol),
            }),
            None => Resolve::NoModule,
        };
    }
    // The all-path form: the longest prefix that names a module file.
    for cut in (1..=parts.len()).rev() {
        if let Some(entry) = module_file(corpus, &parts[..cut]) {
            return Resolve::Found(Target {
                entry,
                module: normalise(&parts[..cut]),
                symbol: parts[cut..].to_vec(),
            });
        }
    }
    Resolve::NoModule
}

fn normalise(parts: &[String]) -> String {
    let mut out: Vec<String> = parts.to_vec();
    if let Some(first) = out.first_mut() {
        *first = first.replace('-', "_");
    }
    out.join("::")
}

fn bounded(text: &str) -> (String, bool) {
    if text.len() <= MAX_SOURCE_BYTES {
        return (text.to_string(), false);
    }
    let mut cut = MAX_SOURCE_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    (text[..cut].to_string(), true)
}

fn member(item: &Item) -> Value {
    json!({
        "name": item.name,
        "kind": item.kind,
        "lineno": item.start_line,
        "signature": item.signature,
    })
}

/// agenthicc's `_outline`: one entry per top-level item, with the members of
/// containers nested.
fn outline_of(items: &[Item]) -> Vec<Value> {
    items
        .iter()
        .map(|it| {
            let mut entry = member(it);
            if !it.children.is_empty() {
                entry["members"] = Value::Array(it.children.iter().map(member).collect());
            }
            entry
        })
        .collect()
}

/// The `inspect_xaft_source` result.
pub fn inspect(corpus: &Corpus, target: &str, include_source: bool) -> Value {
    if corpus.sources().next().is_none() {
        return unavailable("the embedded corpus holds no source");
    }
    let cleaned = target.trim();
    if cleaned.is_empty() {
        return failure(
            "A target is required.",
            "Pass a module such as 'xaft_tools::shell::run_command' or a symbol such as \
             'xaft_tools::registry:ToolRegistryBuilder'.",
        );
    }
    let Target {
        entry,
        module,
        symbol,
    } = match resolve(corpus, cleaned) {
        Resolve::Found(t) => t,
        Resolve::NotXaft => {
            let crates: Vec<String> = crate_names(corpus).into_iter().map(|(n, _)| n).collect();
            return failure(
                format!("{} is not part of xaft.", repr(cleaned)),
                format!(
                    "Only xaft's own crates ({}) and files under crates/ and src/ are \
                     inspectable. Use read_file for other files.",
                    crates.join(", ")
                ),
            );
        }
        Resolve::NoModule => {
            return failure(
                format!("No xaft module matches {}.", repr(cleaned)),
                "Check the module path, or search for the symbol with \
                 search_xaft_source('<name>').",
            );
        }
    };
    let text = match corpus.text(entry) {
        Ok(t) => t,
        Err(_) => {
            return failure(
                format!("{module} could not be read from {}.", entry.path),
                "Inspect a different module.",
            );
        }
    };
    let outline = rust_items::outline(&text);
    let lines: Vec<&str> = text.lines().collect();

    if symbol.is_empty() {
        let mut body = json!({
            "ok": true,
            "module": module,
            "symbol": "",
            "path": entry.path,
            "kind": "module",
            "lineno": 1,
            "end_lineno": lines.len(),
            "signature": module,
            "docstring": outline.docstring,
        });
        if include_source {
            let (source, truncated) = bounded(&lines.join("\n"));
            body["source"] = json!(source);
            body["truncated"] = json!(truncated);
        }
        let members = outline_of(&outline.items);
        if !members.is_empty() {
            body["members"] = Value::Array(members);
        }
        return body;
    }

    let path: Vec<&str> = symbol.iter().map(String::as_str).collect();
    // A crate name is both its library root and its binary root: `xaft`'s
    // `lib.rs` is a stub and its code is in `main.rs`.
    if rust_items::find(&outline.items, &path).is_none() {
        if let Some(main) = entry
            .path
            .strip_suffix("src/lib.rs")
            .map(|prefix| format!("{prefix}src/main.rs"))
            .and_then(|p| corpus.find(EntryKind::Source, &p))
        {
            if let Ok(main_text) = corpus.text(main) {
                if rust_items::find(&rust_items::outline(&main_text).items, &path).is_some() {
                    let target = format!("{}:{}", main.path, symbol.join("::"));
                    let mut found = inspect(corpus, &target, include_source);
                    found["module"] = json!(module);
                    return found;
                }
            }
        }
    }
    let Some(item) = rust_items::find(&outline.items, &path) else {
        let available: Vec<&str> = outline
            .items
            .iter()
            .filter(|i| i.kind != "impl")
            .map(|i| i.name.as_str())
            .take(40)
            .collect();
        return failure(
            format!(
                "{module} defines no symbol named {}.",
                repr(&symbol.join("::"))
            ),
            format!(
                "Available top-level names: {}",
                if available.is_empty() {
                    "(none)".to_string()
                } else {
                    available.join(", ")
                }
            ),
        );
    };

    let start = item.start_line.max(1);
    let end = item.end_line.max(start);
    let mut body = json!({
        "ok": true,
        "module": module,
        "symbol": symbol.join("::"),
        "path": entry.path,
        "kind": item.kind,
        "lineno": start,
        "end_lineno": end,
        "signature": item.signature,
        "docstring": item.docstring,
    });
    if include_source {
        let window = lines
            .get(start - 1..end.min(lines.len()))
            .unwrap_or_default()
            .join("\n");
        let (source, truncated) = bounded(&window);
        if !source.is_empty() {
            body["source"] = json!(source);
            body["truncated"] = json!(truncated);
        }
    }
    // A type's members: its fields or variants, then the items of every
    // `impl` block for it in this file (agenthicc lists a class's methods).
    let mut members: Vec<Value> = item.children.iter().map(member).collect();
    if matches!(item.kind, "struct" | "enum" | "union" | "trait") {
        for block in rust_items::impls_of(&outline.items, &item.name) {
            for child in &block.children {
                let mut m = member(child);
                m["impl"] = json!(block.name);
                members.push(m);
            }
        }
    }
    if !members.is_empty() {
        body["members"] = Value::Array(members);
    }
    body
}

/// The source entries `module` selects: a crate (all of it, tests included),
/// a module (its file, and its directory for `mod.rs`/`foo.rs` + `foo/`), or a
/// path prefix.
fn scope<'a>(corpus: &'a Corpus, module: &str) -> Option<Vec<&'a Entry>> {
    let wanted = module.trim();
    let mut prefixes: Vec<String> = Vec::new();
    let mut exact: Option<String> = None;
    let as_path = wanted.trim_start_matches("./").trim_end_matches('/');
    let krate = wanted.replace('-', "_");
    let crate_prefix = crate_names(corpus)
        .into_iter()
        .find(|(n, _)| *n == krate)
        .map(|(_, p)| if p.is_empty() { "src/".to_string() } else { p });
    let is_dir = corpus
        .sources()
        .any(|e| e.path.starts_with(&format!("{as_path}/")));
    if wanted.contains('/') || wanted.ends_with(".rs") || (crate_prefix.is_none() && is_dir) {
        exact = Some(as_path.to_string());
        prefixes.push(format!("{as_path}/"));
    } else {
        let parts = split_path(wanted);
        if parts.len() == 1 {
            prefixes.push(crate_prefix?);
        } else {
            let entry = module_file(corpus, &parts)?;
            let file = entry.path.clone();
            if let Some(dir) = file.strip_suffix("/mod.rs") {
                prefixes.push(format!("{dir}/"));
            } else if let Some(stem) = file.strip_suffix(".rs") {
                prefixes.push(format!("{stem}/"));
                exact = Some(file);
            }
        }
    }
    let mut selected: Vec<&Entry> = corpus
        .sources()
        .filter(|e| {
            exact.as_deref() == Some(e.path.as_str())
                || prefixes.iter().any(|p| e.path.starts_with(p.as_str()))
        })
        .collect();
    selected.sort_by(|a, b| a.path.cmp(&b.path));
    (!selected.is_empty()).then_some(selected)
}

/// The `search_xaft_source` result.
pub fn search(corpus: &Corpus, query: &str, max_results: i64, module: &str) -> Value {
    if corpus.sources().next().is_none() {
        return unavailable("the embedded corpus holds no source");
    }
    if query.trim().is_empty() {
        return failure(
            "A non-empty query is required.",
            "Call search_xaft_source('fn build_coder') with the text to find.",
        );
    }
    let files: Vec<&Entry> = if module.trim().is_empty() {
        let mut all: Vec<&Entry> = corpus.sources().collect();
        all.sort_by(|a, b| a.path.cmp(&b.path));
        all
    } else {
        match scope(corpus, module) {
            Some(files) => files,
            None => {
                return failure(
                    format!("No xaft module or path matches {}.", repr(module)),
                    "Drop the module filter, or use a module path such as 'xaft_runtime::phased' \
                     or a path such as 'crates/xaft-tools/src/shell'.",
                );
            }
        }
    };
    let is_match = matcher(query);
    let limit = max_results.clamp(1, MAX_SEARCH_RESULTS as i64) as usize;
    let mut hits = Vec::new();
    let mut scanned = 0;
    for entry in files {
        let Ok(text) = corpus.text(entry) else {
            continue;
        };
        scanned += 1;
        for (number, line) in text.lines().enumerate() {
            if is_match(line) {
                hits.push(json!({
                    "path": entry.path,
                    "line": number + 1,
                    "text": clip(line.trim_end(), MAX_LINE_CHARS),
                }));
                if hits.len() >= limit {
                    return search_result(query, hits, scanned, true);
                }
            }
        }
    }
    search_result(query, hits, scanned, false)
}

#[cfg(test)]
mod tests {
    use super::super::test_corpus::corpus;
    use super::*;

    #[test]
    fn module_paths_follow_rust_file_layout() {
        assert_eq!(
            module_of("crates/xaft-tools/src/lib.rs").as_deref(),
            Some("xaft_tools")
        );
        assert_eq!(
            module_of("crates/xaft-tools/src/shell/mod.rs").as_deref(),
            Some("xaft_tools::shell")
        );
        assert_eq!(
            module_of("crates/xaft-tools/src/shell/run_tests/report.rs").as_deref(),
            Some("xaft_tools::shell::run_tests::report")
        );
        assert_eq!(module_of("src/main.rs").as_deref(), Some("xaft"));
        assert_eq!(module_of("crates/xaft-tools/tests/a.rs"), None);
        assert_eq!(module_of("crates/xaft-cli/src/bin/x.rs"), None);
    }

    #[test]
    fn a_module_reports_its_docs_outline_and_bounded_source() {
        let c = corpus();
        let r = inspect(&c, "xaft_demo", true);
        assert_eq!(r["ok"], json!(true), "{r}");
        assert_eq!(r["path"], json!("crates/xaft-demo/src/lib.rs"));
        assert_eq!(r["kind"], json!("module"));
        assert_eq!(r["signature"], json!("xaft_demo"));
        assert_eq!(r["docstring"], json!("The demo crate."));
        assert_eq!(r["truncated"], json!(false));
        let names: Vec<&str> = r["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["net", "ToolRegistry", "impl ToolRegistry"]);
        let outline_only = inspect(&c, "xaft-demo", false);
        assert!(outline_only.get("source").is_none());
        assert!(outline_only.get("members").is_some());
    }

    #[test]
    fn every_target_spelling_resolves_the_same_symbol() {
        let c = corpus();
        for target in [
            "xaft_demo:ToolRegistry::new",
            "xaft_demo:ToolRegistry.new",
            "xaft_demo::ToolRegistry::new",
            "xaft_demo.ToolRegistry.new",
            "crates/xaft-demo/src/lib.rs:ToolRegistry::new",
        ] {
            let r = inspect(&c, target, true);
            assert_eq!(r["ok"], json!(true), "{target}: {r}");
            assert_eq!(r["symbol"], json!("ToolRegistry::new"), "{target}");
            assert_eq!(r["kind"], json!("function"));
            assert_eq!(r["docstring"], json!("An empty registry."));
            assert_eq!(r["signature"], json!("pub fn new() -> Self"));
            assert!(
                r["source"]
                    .as_str()
                    .unwrap()
                    .contains("Self { tools: vec![] }")
            );
        }
        let ty = inspect(&c, "xaft_demo::ToolRegistry", false);
        assert_eq!(ty["kind"], json!("struct"));
        let members: Vec<&str> = ty["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(members, ["tools", "new"], "fields, then impl members");
        let net = inspect(&c, "xaft_demo::net::connect", true);
        assert_eq!(net["path"], json!("crates/xaft-demo/src/net.rs"));
        let test_file = inspect(
            &c,
            "crates/xaft-demo/tests/api.rs:registry_starts_empty",
            false,
        );
        assert_eq!(test_file["kind"], json!("function"));
        assert_eq!(test_file["module"], json!("crates/xaft-demo/tests/api.rs"));
    }

    #[test]
    fn misses_explain_themselves() {
        let c = corpus();
        let empty = inspect(&c, "  ", true);
        assert_eq!(empty["error"], json!("A target is required."));
        let foreign = inspect(&c, "serde::de", true);
        assert_eq!(foreign["error"], json!("'serde::de' is not part of xaft."));
        assert!(foreign["fix"].as_str().unwrap().contains("xaft_demo"));
        let outside = inspect(&c, "/etc/passwd.rs", true);
        assert_eq!(outside["ok"], json!(false));
        let no_module = inspect(&c, "xaft_demo:nope:x", true);
        assert_eq!(no_module["ok"], json!(false));
        let no_symbol = inspect(&c, "xaft_demo:Nope", true);
        assert_eq!(
            no_symbol["error"],
            json!("xaft_demo defines no symbol named 'Nope'.")
        );
        assert_eq!(
            no_symbol["fix"],
            json!("Available top-level names: net, ToolRegistry")
        );
    }

    #[test]
    fn source_search_scopes_by_crate_module_or_path() {
        let c = corpus();
        let all = search(&c, "fn ", 40, "");
        assert_eq!(all["files_scanned"], json!(4));
        let krate = search(&c, "registry", 40, "xaft_demo");
        assert_eq!(
            krate["files_scanned"],
            json!(3),
            "a crate includes its tests"
        );
        assert_eq!(
            krate["matches"][0]["path"],
            json!("crates/xaft-demo/src/lib.rs")
        );
        let module = search(&c, "connect", 40, "xaft_demo::net");
        assert_eq!(module["files_scanned"], json!(1));
        assert_eq!(
            module["matches"][0],
            json!({"path": "crates/xaft-demo/src/net.rs", "line": 2, "text": "pub fn connect(host: &str) -> bool {"})
        );
        let by_path = search(&c, "main", 40, "src");
        assert_eq!(by_path["files_scanned"], json!(1));
        let missing = search(&c, "x", 40, "xaft_demo::nope");
        assert_eq!(missing["ok"], json!(false));
        assert_eq!(search(&c, "", 40, "")["ok"], json!(false));
    }
}
