//! The `ui_catalog` tool group — agenthicc's `workflows/name_that_ui.py`, the
//! NameThatUI component dictionary `site_imitate` uses to give its analyze,
//! plan and build phases one canonical UI vocabulary.
//!
//! The catalog is scraped from <https://namethatui.com/> (there is no public
//! API): the page embeds every element record in its Next.js RSC payload, as
//! `self.__next_f.push([1,"..."])` JavaScript string chunks. [`parse_catalog_html`]
//! decodes those chunks and brace-matches each `{"slug": ...}` record, exactly
//! like agenthicc's `fetch_catalog`.
//!
//! Loading **fails soft**: any network, cache or parse error yields an empty
//! catalog and the tools answer that the dictionary is unavailable, so the
//! workflow proceeds without it — agenthicc's contract. A fetched catalog is
//! cached on disk for 24 hours (`namethatui_elements.json` in the system temp
//! directory, agenthicc's `DEFAULT_CACHE`) and memoised in the process, so a
//! session that is offline pays for one failed fetch, not one per lookup.
//!
//! | tool | agenthicc |
//! |---|---|
//! | `lookup_component(description)` | `name_that_ui.lookup` + `match_brief` + `format_matches` |
//! | `list_components(platform, query)` | `name_that_ui.list_names` + `format_name_list` |
//!
//! A phase named `build` gets only `lookup_component`, worded for building —
//! the tool set of agenthicc's `site_imitate` build phase.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use serde_json::{Value, json};
use tracing::{info, warn};

/// The page the catalog is scraped from.
pub const URL: &str = "https://namethatui.com/";
/// agenthicc's `USER_AGENT`.
pub const USER_AGENT: &str = "name_that_ui/1.0 (+site_imitate workflow)";
/// agenthicc's `DEFAULT_TTL`: refresh the catalog daily.
pub const DEFAULT_TTL: Duration = Duration::from_secs(86_400);
/// How long a failed load is remembered before the next tool call retries.
const FAILED_RETRY: Duration = Duration::from_secs(600);
/// agenthicc's fetch timeout.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// One-line description for `list_tool_groups`.
pub const GROUP_SUMMARY: &str = "the NameThatUI component dictionary (site_imitate): \
     lookup_component(description) names a UI element from a sloppy description and returns its \
     API symbols (shadcn/ui first) and build prompt; list_components(platform, query) lists the \
     canonical names. Fails soft when the catalog cannot be fetched; a phase named build gets \
     lookup_component only";

/// agenthicc's `DEFAULT_CACHE`.
pub fn default_cache_path() -> PathBuf {
    std::env::temp_dir().join("namethatui_elements.json")
}

// ── Catalog extraction ─────────────────────────────────────────────────────

/// Decode one level of a JavaScript double-quoted string literal body —
/// agenthicc's `js_unescape`.
pub fn js_unescape(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let hex4 = |at: usize| -> Option<u32> {
        if at + 4 > chars.len() {
            return None;
        }
        let digits: String = chars[at..at + 4].iter().collect();
        u32::from_str_radix(&digits, 16).ok()
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            let next = chars[i + 1];
            let simple = match next {
                'n' => Some('\n'),
                't' => Some('\t'),
                'r' => Some('\r'),
                '"' => Some('"'),
                '\\' => Some('\\'),
                '/' => Some('/'),
                'b' => Some('\u{8}'),
                'f' => Some('\u{c}'),
                _ => None,
            };
            if let Some(ch) = simple {
                out.push(ch);
                i += 2;
                continue;
            }
            if next == 'u' {
                if let Some(code) = hex4(i + 2) {
                    // A UTF-16 surrogate pair spans two escapes.
                    if (0xD800..0xDC00).contains(&code)
                        && chars.get(i + 6) == Some(&'\\')
                        && chars.get(i + 7) == Some(&'u')
                    {
                        if let Some(low) = hex4(i + 8).filter(|l| (0xDC00..0xE000).contains(l)) {
                            let combined = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            out.push(char::from_u32(combined).unwrap_or('\u{FFFD}'));
                            i += 12;
                            continue;
                        }
                    }
                    out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    i += 6;
                    continue;
                }
            }
            out.push(next);
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn push_chunk_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r#"(?s)self\.__next_f\.push\(\[1,"(.*?)"\]\)"#).expect("valid regex")
    })
}

/// Pull every element record out of the homepage HTML — agenthicc's
/// `fetch_catalog` minus the network.
pub fn parse_catalog_html(raw: &str) -> Vec<Value> {
    let payload: String = push_chunk_re()
        .captures_iter(raw)
        .map(|c| js_unescape(&c[1]))
        .collect();
    let bytes = payload.as_bytes();
    let mut records = Vec::new();
    let mut pos = 0;
    while let Some(found) = payload[pos..].find("{\"slug\":") {
        let start = pos + found;
        // Brace-match the record, JSON-aware: skip escapes inside strings.
        let mut depth = 0usize;
        let mut in_str = false;
        let mut j = start;
        let mut end = None;
        while j < bytes.len() {
            let ch = bytes[j];
            if in_str {
                if ch == b'\\' {
                    j += 2;
                    continue;
                }
                if ch == b'"' {
                    in_str = false;
                }
            } else if ch == b'"' {
                in_str = true;
            } else if ch == b'{' {
                depth += 1;
            } else if ch == b'}' {
                depth -= 1;
                if depth == 0 {
                    end = Some(j + 1);
                    break;
                }
            }
            j += 1;
        }
        let Some(end) = end else { break };
        if let Some(slice) = payload.get(start..end) {
            if let Ok(record @ Value::Object(_)) = serde_json::from_str::<Value>(slice) {
                records.push(record);
            }
        }
        pos = end;
    }
    records
}

/// Fetch the homepage HTML.
pub async fn fetch_html() -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| e.to_string())?;
    let response = client.get(URL).send().await.map_err(|e| e.to_string())?;
    let body = response.bytes().await.map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// The catalog from `cache` when fresh, otherwise fetched with `fetch` and
/// cached — agenthicc's `load_catalog`. Never fails: any error is `[]`.
pub async fn load_catalog_with<F, Fut>(cache: &Path, ttl: Duration, fetch: F) -> Vec<Value>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let fresh = std::fs::metadata(cache)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < ttl);
    if fresh {
        if let Ok(text) = std::fs::read_to_string(cache) {
            if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&text) {
                if !items.is_empty() {
                    return items;
                }
            }
        }
    }
    match fetch().await {
        Ok(html) => {
            let records = parse_catalog_html(&html);
            if !records.is_empty() {
                // Caching is best-effort.
                if let Ok(text) = serde_json::to_string(&records) {
                    let _ = std::fs::write(cache, text);
                }
            }
            records
        }
        Err(e) => {
            warn!(error = %e, "name_that_ui catalog fetch failed; proceeding without it");
            Vec::new()
        }
    }
}

/// The process-wide catalog: loaded on first use, then reused.
async fn shared_catalog() -> Arc<Vec<Value>> {
    type Memo = tokio::sync::Mutex<Option<(Instant, Arc<Vec<Value>>)>>;
    static MEMO: OnceLock<Memo> = OnceLock::new();
    let mut slot = MEMO
        .get_or_init(|| tokio::sync::Mutex::new(None))
        .lock()
        .await;
    if let Some((at, catalog)) = slot.as_ref() {
        let keep = if catalog.is_empty() {
            FAILED_RETRY
        } else {
            DEFAULT_TTL
        };
        if at.elapsed() < keep {
            return Arc::clone(catalog);
        }
    }
    let catalog = Arc::new(load_catalog_with(&default_cache_path(), DEFAULT_TTL, fetch_html).await);
    if !catalog.is_empty() {
        info!("name_that_ui catalog loaded: {} elements", catalog.len());
    }
    *slot = Some((Instant::now(), Arc::clone(&catalog)));
    catalog
}

// ── Fuzzy lookup ───────────────────────────────────────────────────────────

/// agenthicc's `_norm`: lower-case, non-alphanumeric runs become one space.
fn norm(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut gap = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

fn text<'a>(record: &'a Value, key: &str) -> &'a str {
    record.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn texts<'a>(record: &'a Value, key: &str) -> Vec<&'a str> {
    match record.get(key) {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn entries<'a>(record: &'a Value, key: &str) -> Vec<&'a Value> {
    match record.get(key) {
        Some(Value::Array(a)) => a.iter().filter(|v| v.is_object()).collect(),
        _ => Vec::new(),
    }
}

/// Rank the catalog against a sloppy description, like the site's search —
/// agenthicc's `lookup`.
pub fn lookup<'a>(query: &str, records: &'a [Value], top: usize) -> Vec<&'a Value> {
    let q = norm(query);
    let qwords: std::collections::HashSet<&str> = q.split(' ').filter(|w| !w.is_empty()).collect();
    let mut scored: Vec<(f64, &Value)> = Vec::new();
    for r in records {
        let mut aliases = texts(r, "fuzzy");
        aliases.extend(texts(r, "aka"));
        let mut hay = vec![text(r, "name"), text(r, "tagline"), text(r, "description")];
        hay.extend(aliases.iter().copied());
        let blob = norm(&hay.join(" "));
        let blob_words: std::collections::HashSet<&str> =
            blob.split(' ').filter(|w| !w.is_empty()).collect();
        let mut score = 0.0;
        if !q.is_empty() {
            for field in &aliases {
                if norm(field).contains(&q) {
                    score += 3.0;
                }
            }
            if norm(text(r, "name")).contains(&q) {
                score += 3.0;
            }
            if norm(text(r, "description")).contains(&q) {
                score += 2.0;
            }
        }
        score += qwords.intersection(&blob_words).count() as f64 * 0.5;
        if score > 0.0 {
            scored.push((score, r));
        }
    }
    // A stable sort, so equal scores keep catalog order (Python's sort).
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(top).map(|(_, r)| r).collect()
}

/// The shadcn/ui symbols of a record — agenthicc's `shadcn_symbols`.
pub fn shadcn_symbols(record: &Value) -> Vec<String> {
    entries(record, "api")
        .into_iter()
        .filter(|api| text(api, "framework").to_lowercase().contains("shadcn"))
        .map(|api| text(api, "symbol").to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Compact JSON summary of a match — agenthicc's `match_brief`.
pub fn match_brief(record: &Value) -> Value {
    let apis: Vec<Value> = entries(record, "api")
        .into_iter()
        .map(|api| {
            json!({
                "framework": api.get("framework").cloned().unwrap_or(Value::Null),
                "symbol": api.get("symbol").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    json!({
        "name": record.get("name").cloned().unwrap_or(Value::Null),
        "platform": record.get("platform").cloned().unwrap_or(Value::Null),
        "api": apis,
        "shadcn": shadcn_symbols(record),
        "prompt": take_chars(text(record, "prompt"), 600),
    })
}

/// Human-readable match list — agenthicc's `format_matches`.
pub fn format_matches(matches: &[&Value]) -> String {
    matches
        .iter()
        .map(|r| {
            let name = match text(r, "name") {
                "" => "?",
                n => n,
            };
            let platform = match text(r, "platform") {
                "" => "?",
                p => p,
            };
            let symbols = entries(r, "api")
                .into_iter()
                .map(|api| text(api, "symbol"))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            let shad = shadcn_symbols(r).join(", ");
            let prompt = match text(r, "prompt").trim() {
                "" => "-",
                p => p,
            };
            format!(
                "* {name} ({platform})\n  api symbols: {}\n  shadcn/ui  : {}\n  prompt     : {}",
                if symbols.is_empty() { "-" } else { &symbols },
                if shad.is_empty() { "-" } else { &shad },
                take_chars(prompt, 300)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A sorted list of catalog component names — agenthicc's `list_names`.
pub fn list_names(
    records: &[Value],
    platform: &str,
    query: &str,
    top: Option<usize>,
) -> Vec<Value> {
    let platform = platform.trim().to_lowercase();
    let q = norm(query);
    let mut out: Vec<Value> = Vec::new();
    for r in records {
        let name = text(r, "name");
        if name.is_empty() {
            continue;
        }
        let rp = match r.get("platform") {
            Some(Value::String(s)) => s.to_lowercase(),
            _ => String::new(),
        };
        if !platform.is_empty() && rp != platform {
            continue;
        }
        if !q.is_empty() {
            let mut hay = vec![name, text(r, "tagline"), text(r, "description")];
            hay.extend(texts(r, "aka"));
            hay.extend(texts(r, "fuzzy"));
            if !norm(&hay.join(" ")).contains(&q) && !norm(name).contains(&q) {
                continue;
            }
        }
        let shown_platform = match r.get("platform") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => "web".to_string(),
        };
        out.push(json!({
            "name": name,
            "platform": shown_platform,
            "shadcn": shadcn_symbols(r),
        }));
    }
    out.sort_by(|a, b| {
        let key = |v: &Value| {
            (
                text(v, "platform").to_string(),
                text(v, "name").to_lowercase(),
            )
        };
        key(a).cmp(&key(b))
    });
    if let Some(n) = top.filter(|n| *n > 0) {
        out.truncate(n);
    }
    out
}

/// Platform-grouped listing — agenthicc's `format_name_list`.
pub fn format_name_list(names: &[Value]) -> String {
    if names.is_empty() {
        return "(no components match the filter)".to_string();
    }
    let mut by_platform: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for n in names {
        let p = match text(n, "platform") {
            "" => "web",
            p => p,
        };
        by_platform
            .entry(p.to_string())
            .or_default()
            .push(text(n, "name").to_string());
    }
    let mut lines = Vec::new();
    for (p, entries) in by_platform {
        lines.push(format!("[{p}] ({}):", entries.len()));
        lines.push(entries.join(", "));
    }
    lines.join("\n")
}

// ── Tools ──────────────────────────────────────────────────────────────────

/// Where the tools get their catalog.
#[derive(Clone)]
pub enum CatalogSource {
    /// The process-wide catalog, fetched on first use.
    Shared,
    /// A fixed catalog (tests, offline use).
    Fixed(Arc<Vec<Value>>),
}

impl CatalogSource {
    async fn get(&self) -> Arc<Vec<Value>> {
        match self {
            Self::Shared => shared_catalog().await,
            Self::Fixed(c) => Arc::clone(c),
        }
    }
}

/// Wording of the tools: the analyze phase describes, the build phase builds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Voice {
    Describe,
    Build,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Lookup,
    List,
}

struct CatalogTool {
    kind: Kind,
    voice: Voice,
    source: CatalogSource,
}

/// The `ui_catalog` group for `phase`, over the shared catalog.
pub fn ui_catalog_tools(phase: &str) -> Vec<Arc<ErasedTool>> {
    ui_catalog_tools_with(phase, CatalogSource::Shared)
}

/// The `ui_catalog` group for `phase`, over `source`.
pub fn ui_catalog_tools_with(phase: &str, source: CatalogSource) -> Vec<Arc<ErasedTool>> {
    let voice = if phase == "build" {
        Voice::Build
    } else {
        Voice::Describe
    };
    let tool = |kind| {
        Arc::new(CatalogTool {
            kind,
            voice,
            source: source.clone(),
        }) as Arc<ErasedTool>
    };
    match voice {
        Voice::Build => vec![tool(Kind::Lookup)],
        Voice::Describe => vec![tool(Kind::List), tool(Kind::Lookup)],
    }
}

#[async_trait::async_trait]
impl Tool for CatalogTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        match self.kind {
            Kind::Lookup => "lookup_component",
            Kind::List => "list_components",
        }
    }

    fn description(&self) -> &str {
        match self.kind {
            Kind::Lookup => {
                "Name a UI element from a sloppy description using the NameThatUI dictionary. \
                 Returns canonical names, API symbols (shadcn/ui first) and a paste-ready build \
                 prompt."
            }
            Kind::List => {
                "List the component names available in the NameThatUI dictionary. Use this to \
                 see what canonical components exist before describing an element, so the \
                 analysis uses the dictionary's own vocabulary."
            }
        }
    }

    fn schema(&self) -> Value {
        match (self.kind, self.voice) {
            (Kind::Lookup, Voice::Describe) => json!({
                "type": "object",
                "properties": { "description": { "type": "string", "description":
                    "How the element looks or where it sits, in plain words (e.g. \"the three lines \
                     that open a side panel\", \"the gray text inside the box that disappears when \
                     you type\")." } },
                "required": ["description"],
                "additionalProperties": false
            }),
            (Kind::Lookup, Voice::Build) => json!({
                "type": "object",
                "properties": { "description": { "type": "string", "description":
                    "How the element looks or where it sits, in plain words." } },
                "required": ["description"],
                "additionalProperties": false
            }),
            (Kind::List, _) => json!({
                "type": "object",
                "properties": {
                    "platform": { "type": "string", "description":
                        "Optional filter - \"web\" or \"macos\". Empty = all." },
                    "query": { "type": "string", "description":
                        "Optional keyword to narrow the list (matched against the name and its \
                         aliases), e.g. \"menu\", \"field\", \"scroll\"." }
                },
                "required": [],
                "additionalProperties": false
            }),
        }
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        let arg = |k: &str| {
            input
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let catalog = self.source.get().await;
        let body = match self.kind {
            Kind::Lookup => {
                if catalog.is_empty() {
                    json!({
                        "ok": false,
                        "error": "The NameThatUI dictionary is unavailable.",
                        "fix": match self.voice {
                            Voice::Describe => "Describe this element from scratch in the analysis.",
                            Voice::Build => "Build this component from the plan and your best judgment.",
                        },
                    })
                } else {
                    let matches = lookup(&arg("description"), &catalog, 5);
                    if matches.is_empty() {
                        json!({
                            "ok": true,
                            "matches": [],
                            "message": match self.voice {
                                Voice::Describe => "Nothing matched that description - describe this element from scratch.",
                                Voice::Build => "Nothing matched that description - build this component from scratch.",
                            },
                        })
                    } else {
                        json!({
                            "ok": true,
                            "matches": matches.iter().map(|m| match_brief(m)).collect::<Vec<_>>(),
                            "message": format_matches(&matches),
                        })
                    }
                }
            }
            Kind::List => {
                if catalog.is_empty() {
                    json!({
                        "ok": false,
                        "error": "The NameThatUI dictionary is unavailable.",
                        "fix": "Describe observed elements from scratch in the analysis.",
                    })
                } else {
                    let names = list_names(&catalog, &arg("platform"), &arg("query"), None);
                    json!({
                        "ok": true,
                        "total": names.len(),
                        "message": format_name_list(&names),
                        "components": names,
                    })
                }
            }
        };
        Ok(ToolResult::ok(body.to_string(), &ctx.tool_use_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed copy of the shape namethatui.com serves: element records
    /// inside escaped RSC push chunks, split mid-record across two chunks,
    /// with an escaped quote and a unicode escape inside the strings.
    const FIXTURE: &str = r#"<!DOCTYPE html><html><head><title>Name That UI</title></head><body>
<script>self.__next_f.push([1,"0:[\"$\",\"main\",null,{\"children\":[{\"slug\":\"hamburger-menu\",\"name\":\"Hamburger Menu\",\"platform\":\"web\",\"tagline\":\"The three lines that open a side panel\",\"description\":\"A button of three horizontal lines that toggles a navigation drawer.\",\"aka\":[\"Nav Drawer Toggle\"],\"fuzzy\":[\"three lines\",\"menu icon\"],\"api\":[{\"framework\":\"shadcn/ui\",\"symbol\":\"Sheet side=\\\"left\\\"\"},{\"framework\":\"ARIA\",\"symbol\":\"aria-expanded\"}],\"prompt\":\"Build a hamburger button that opens a left Sheet.\"},"]);</script>
<script>self.__next_f.push([1,"{\"slug\":\"placeholder\",\"name\":\"Placeholder\",\"platform\":\"web\",\"tagline\":\"Gray hint text\",\"description\":\"The gray text inside an input that disappears when you type — not a label.\",\"aka\":[\"Hint text\"],\"fuzzy\":[\"gray text in the box\"],\"api\":[{\"framework\":\"HTML\",\"symbol\":\"placeholder\"}],\"prompt\":\"Use a real label; placeholder is a hint.\"},{\"slug\":\"menu-bar\",\"name\":\"Menu Bar\",\"platform\":\"macos\",\"tagline\":\"Top-of-screen menus\",\"description\":\"The strip of menus at the top of the screen.\",\"api\":[{\"framework\":\"AppKit\",\"symbol\":\"NSMenu\"}],\"prompt\":\"\"}]}]\n"]);</script>
<script>self.__next_f.push([1,"{\"slug\":broken"]);</script>
</body></html>"#;

    fn catalog() -> Vec<Value> {
        parse_catalog_html(FIXTURE)
    }

    #[test]
    fn js_unescape_decodes_one_literal_level() {
        assert_eq!(js_unescape(r#"a\"b\\c\ndé\/"#), "a\"b\\c\nd\u{e9}/");
        assert_eq!(js_unescape(r"😀"), "\u{1F600}");
        assert_eq!(js_unescape(r"\q"), "q");
        assert_eq!(js_unescape("trailing\\"), "trailing\\");
    }

    #[test]
    fn the_parser_extracts_every_record_from_the_page() {
        let records = catalog();
        let slugs: Vec<&str> = records.iter().map(|r| text(r, "slug")).collect();
        assert_eq!(slugs, ["hamburger-menu", "placeholder", "menu-bar"]);
        assert_eq!(
            shadcn_symbols(&records[0]),
            ["Sheet side=\"left\""],
            "escaped quotes survive both decoding levels"
        );
        assert!(text(&records[1], "description").contains('\u{2014}'));
    }

    #[test]
    fn a_page_without_records_parses_to_nothing() {
        assert!(parse_catalog_html("<html>maintenance</html>").is_empty());
        assert!(parse_catalog_html("").is_empty());
    }

    #[test]
    fn lookup_ranks_sloppy_descriptions_like_the_site() {
        let records = catalog();
        let hits = lookup("the three lines that open a side panel", &records, 5);
        assert_eq!(text(hits[0], "slug"), "hamburger-menu");
        let hits = lookup("gray text in the box", &records, 5);
        assert_eq!(text(hits[0], "slug"), "placeholder");
        assert!(lookup("zzz qqq", &records, 5).is_empty());
        assert_eq!(lookup("menu", &records, 1).len(), 1, "top caps the result");
    }

    #[test]
    fn briefs_and_formatting_match_agenthicc() {
        let records = catalog();
        let brief = match_brief(&records[0]);
        assert_eq!(brief["name"], "Hamburger Menu");
        assert_eq!(brief["shadcn"], json!(["Sheet side=\"left\""]));
        assert_eq!(
            brief["api"][1],
            json!({"framework": "ARIA", "symbol": "aria-expanded"})
        );
        let text = format_matches(&[&records[0], &records[2]]);
        assert!(text.starts_with(
            "* Hamburger Menu (web)\n  api symbols: Sheet side=\"left\", aria-expanded"
        ));
        assert!(text.contains(
            "* Menu Bar (macos)\n  api symbols: NSMenu\n  shadcn/ui  : -\n  prompt     : -"
        ));
    }

    #[test]
    fn list_names_filters_by_platform_and_query() {
        let records = catalog();
        let all = list_names(&records, "", "", None);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0]["platform"], "macos", "sorted by platform, then name");
        let web = list_names(&records, "WEB", "", None);
        assert_eq!(web.len(), 2);
        let menus = list_names(&records, "", "menu", None);
        let names: Vec<&str> = menus.iter().map(|n| text(n, "name")).collect();
        assert_eq!(names, ["Menu Bar", "Hamburger Menu"]);
        assert_eq!(
            format_name_list(&web),
            "[web] (2):\nHamburger Menu, Placeholder"
        );
        assert_eq!(format_name_list(&[]), "(no components match the filter)");
    }

    async fn must_not_fetch() -> Result<String, String> {
        panic!("a fresh cache must not fetch")
    }

    #[tokio::test]
    async fn loading_prefers_a_fresh_cache_and_fails_soft() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("namethatui_elements.json");

        // No cache, failing fetch: an empty catalog, no error.
        let empty = load_catalog_with(&cache, DEFAULT_TTL, || async {
            Err::<String, String>("offline".into())
        })
        .await;
        assert!(empty.is_empty());
        assert!(!cache.exists());

        // A fetched page is parsed and cached.
        let fetched =
            load_catalog_with(&cache, DEFAULT_TTL, || async { Ok(FIXTURE.to_string()) }).await;
        assert_eq!(fetched.len(), 3);
        assert!(cache.is_file());

        // A fresh cache is used without fetching.
        let cached = load_catalog_with(&cache, DEFAULT_TTL, must_not_fetch).await;
        assert_eq!(cached.len(), 3);

        // An expired cache is refetched; a failed refetch is still soft.
        let stale = load_catalog_with(&cache, Duration::ZERO, || async {
            Err::<String, String>("offline".into())
        })
        .await;
        assert!(stale.is_empty());
    }

    #[tokio::test]
    async fn the_tools_answer_from_the_catalog_or_say_it_is_unavailable() {
        let source = CatalogSource::Fixed(Arc::new(catalog()));
        let tools = ui_catalog_tools_with("analyze", source);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, ["list_components", "lookup_component"]);
        let ctx = ToolContext::new("t");
        let r = tools[1]
            .call(json!({"description": "three lines menu"}), &ctx)
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&r.content).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["matches"][0]["name"], "Hamburger Menu");
        let r = tools[0]
            .call(json!({"platform": "macos"}), &ctx)
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&r.content).unwrap();
        assert_eq!(v["total"], 1);

        let build = ui_catalog_tools_with("build", CatalogSource::Fixed(Arc::new(Vec::new())));
        assert_eq!(build.len(), 1, "the build phase only looks components up");
        let r = build[0]
            .call(json!({"description": "x"}), &ctx)
            .await
            .unwrap();
        assert!(
            !r.is_error,
            "an unavailable dictionary is not a tool failure"
        );
        let v: Value = serde_json::from_str(&r.content).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(
            v["fix"],
            "Build this component from the plan and your best judgment."
        );
    }
}
