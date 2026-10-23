//! make_book — agenthicc's `workflows/make_book/` gates and builder.
//!
//! Every make_book transition is one of agenthicc's *summary-only* gates: the
//! agent writes artifacts with its normal tools, then calls the gate with a
//! short summary, and the gate's effect inspects the artifacts
//! deterministically before the phase may change:
//!
//! | effect | gate | what it checks and records |
//! |---|---|---|
//! | `book_submit_toc` | `submit_toc` | the run-scoped TOC manifest `.xaft/make_book/<run>/toc.json` |
//! | `book_submit_research` | `submit_research` | research files, one notes file per chapter |
//! | `book_confirm_assets` | `confirm_assets_ready` | ≥ max(6, 3 × chapters) assets and the free-Unsplash provenance manifest |
//! | `book_confirm_chapter` | `confirm_chapter_complete` | the canonical `chapters/NN-slug.md`, its words and referenced assets; advances the chapter cursor |
//! | `book_confirm_layout` / `book_confirm_final_layout` | `confirm_layout_ready` | image and table bounds on the printed page ([`layout`]) |
//! | `book_confirm_front_matter` / `book_confirm_back_matter` | `confirm_*_ready` | Markdown under `front-matter/` / `back-matter/`, no `contents.md` |
//! | `book_mark_complete` | `mark_book_complete` | the build record and `dist/` outputs, built from the current sources |
//!
//! The `chapters_complete` guard (the chapter phase's `skip_when`) holds once
//! every planned chapter is confirmed. Book state lives under the `book`
//! RunData key; the values prompts need are mirrored into `state`
//! (`{state.book_title}`, `{state.book_chapter_path}`, …).
//!
//! The `book` tool group is the compile phase's: `create_build_book` runs
//! [`build`], and `list_build_book_reference` / `read_build_book_reference`
//! expose that builder's own source as the reference agenthicc bundles.

pub(crate) mod build;
pub(crate) mod layout;
pub(crate) mod zip;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use regex::Regex;
use serde_json::{Value, json};

use self::build::{BookMeta, PdfTool, RECORD_NAME, build_book, sources_fingerprint};
use self::layout::{
    CONTENT_WIDTH_IN, MAX_MEDIA_HEIGHT_IN, PAGE_HEIGHT_IN, TARGET_MEDIA_WIDTH_IN, files_under,
    image_references, layout_findings, resolve_image,
};
use super::groups::RunData;

/// RunData key holding the book's state.
const KEY: &str = "book";
/// agenthicc's transition-receipt contract version.
const CONTRACT_VERSION: &str = "make_book.transitions.v2";
const CONTENT_TYPES: &[&str] = &["images", "equations", "code", "tables"];
const ASSET_SUFFIXES: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "svg", "mmd", "py"];
const RASTER_SUFFIXES: &[&str] = &["png", "jpg", "jpeg", "webp"];
const RESEARCH_SUFFIXES: &[&str] = &["md", "txt", "json", "yaml", "yml"];

/// The builder's source, served by the reference tools.
const BUILDER_REFERENCE: &str = include_str!("build.rs");

fn book(data: &RunData) -> serde_json::Map<String, Value> {
    match data.get(KEY) {
        Some(Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    }
}

fn save(data: &RunData, book: serde_json::Map<String, Value>) {
    data.set(KEY, Value::Object(book));
}

fn text(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default().to_string()
}

/// agenthicc's `_compact`.
fn compact(value: &str, limit: usize) -> String {
    let t = value.trim();
    if t.chars().count() <= limit {
        return t.to_string();
    }
    let head: String = t.chars().take(limit - 1).collect();
    format!("{}…", head.trim_end())
}

fn summary(args: &Value, tool: &str) -> Result<String, String> {
    let s = text(args.get("summary"));
    if s.trim().is_empty() {
        return Err(format!(
            "{tool} summary is empty. Write the artifacts, then call {tool}(summary=...)."
        ));
    }
    Ok(compact(&s, 4_000))
}

fn failure(message: impl std::fmt::Display, fix: &str) -> String {
    format!("{message} Fix: {fix}")
}

/// Record one bounded transition receipt (agenthicc's `_transition_receipt`).
fn receipt(book: &mut serde_json::Map<String, Value>, phase: &str, outcome: &str, mut body: Value) {
    body["contract_version"] = json!(CONTRACT_VERSION);
    body["workflow"] = json!("make_book");
    body["phase"] = json!(phase);
    body["outcome"] = json!(outcome);
    body["current_chapter_index"] = book
        .get("current_chapter_index")
        .cloned()
        .unwrap_or(json!(0));
    let mut receipts = match book.remove("transition_receipts") {
        Some(Value::Array(a)) => a,
        _ => Vec::new(),
    };
    if !receipts.contains(&body) {
        receipts.push(body);
    }
    book.insert("transition_receipts".into(), Value::Array(receipts));
}

fn root_of(book: &serde_json::Map<String, Value>) -> Result<PathBuf, String> {
    let dir = text(book.get("output_dir"));
    if dir.is_empty() {
        return Err(failure(
            "book output directory is not configured.",
            "Run the TOC phase before this one.",
        ));
    }
    Ok(PathBuf::from(dir))
}

fn relative(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

/// agenthicc's `_chapter_slug`.
pub(crate) fn chapter_slug(title: &str) -> String {
    let slug = build::slugify(title);
    if slug == "book" && !title.to_lowercase().contains("book") {
        "chapter".into()
    } else {
        slug
    }
}

/// The run-scoped TOC manifest path (agenthicc's `_toc_manifest_path`).
pub(crate) fn toc_manifest_path(data: &RunData) -> Option<PathBuf> {
    let wd = data.working_dir()?;
    let id: String = data
        .run_id()
        .unwrap_or("run")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let id = id.trim_matches(|c| c == '.' || c == '-');
    let id = if id.is_empty() { "run" } else { id };
    Some(wd.join(".xaft").join("make_book").join(id).join("toc.json"))
}

// ── Guard ──────────────────────────────────────────────────────────────────

/// `chapters_complete`: every planned chapter has been confirmed.
pub(crate) fn chapters_complete(data: &RunData) -> Result<(), String> {
    let b = book(data);
    let chapters = b
        .get("chapters")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    if chapters == 0 {
        return Err("no table of contents is recorded; run the TOC phase first".into());
    }
    let cursor = b
        .get("current_chapter_index")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    if cursor >= chapters {
        Ok(())
    } else {
        Err(format!(
            "{} of {chapters} chapters are still to be written",
            chapters - cursor
        ))
    }
}

// ── Effects ────────────────────────────────────────────────────────────────

/// Run one of make_book's gate effects.
pub(crate) fn run_effect(effect: &str, data: &RunData, args: &Value) -> Result<String, String> {
    match effect {
        "book_submit_toc" => submit_toc(data, args),
        "book_submit_research" => submit_research(data, args),
        "book_confirm_assets" => confirm_assets(data, args),
        "book_confirm_chapter" => confirm_chapter(data, args),
        "book_confirm_layout" => confirm_layout(data, args, "layout_review"),
        "book_confirm_final_layout" => confirm_layout(data, args, "final_layout_review"),
        "book_confirm_front_matter" => confirm_matter(data, args, "front-matter"),
        "book_confirm_back_matter" => confirm_matter(data, args, "back-matter"),
        "book_mark_complete" => mark_complete(data, args),
        other => Err(format!("unknown effect '{other}'")),
    }
}

/// The RICH CONTENT rules of the chapter prompt for the enabled types.
fn rich_content_rules(types: &[String]) -> String {
    let mut out = Vec::new();
    if types.iter().any(|t| t == "images") {
        out.push(
            "- IMAGES: create BOTH mermaid diagrams AND matplotlib graphs for the figures the \
             outline/research calls for. For structural/flow diagrams write a .mmd source and \
             render it to a PNG via mermaid-cli (npx -y @mermaid-js/mermaid-cli -i fig.mmd -o \
             fig.png); for data charts write a small matplotlib script and run it to produce a \
             PNG. Save the PNGs under <output_dir>/assets/ (create the folder), then reference \
             them with Markdown relative to the chapter file: ![Figure caption](../assets/fig-NN-name.png). \
             Keep the caption descriptive. Include at least one figure per chapter when 'images' \
             is enabled.\n   EVERY image must be APPROPRIATELY SIZED for print: no full-bleed \
             wall-to-wall images. Cap the rendered image at 6.175in wide (95% of the 6.5in \
             content width) and 8.05in high (70% of the 11.5in page height). For wider source \
             images, pre-resize or crop them so the figure fits comfortably on the page next to \
             the caption. Avoid images that are tiny (blurry when scaled up) or enormous \
             (wasting page space).",
        );
    }
    if types.iter().any(|t| t == "equations") {
        out.push(
            "- EQUATIONS: write mathematics in LaTeX — $...$ for inline and $$...$$ for display \
             equations. The PDF typesets them natively through pandoc; the EPUB and HTML keep \
             the LaTeX source, so use proper LaTeX (\\frac, \\sum, \\int, \\sqrt, ...).",
        );
    }
    if types.iter().any(|t| t == "code") {
        out.push(
            "- CODE: include fenced code blocks with a language tag (```python, ```rust, \
             ```ts). Code must be accurate, runnable, and directly relevant. Add a short comment \
             or prose tie-in for each snippet. The language tag is kept in every output.",
        );
    }
    if types.iter().any(|t| t == "tables") {
        out.push(
            "- TABLES: render reference data, comparisons, and parameters as Markdown tables \
             with a header row and aligned columns. Keep every table within 6.175in content \
             width (95% of the 6.5in content width) and 8.05in height (70% of the 11.5in page); \
             split wide or tall tables into multiple readable tables.",
        );
    }
    if out.is_empty() {
        "- (no rich content types are enabled)".into()
    } else {
        out.join("\n")
    }
}

fn unsplash_rule(required: bool) -> &'static str {
    if required {
        "For PHOTOGRAPHIC / ILLUSTRATIVE images (real-world photos, scenery, product shots, \
         case-study visuals), MUST use an available retrieval capability for the free Unsplash \
         service (prefer a configured unsplash_images tool; otherwise use the approved \
         browser/network tools) to download photos into assets/unsplash/. Do NOT use Unsplash+ \
         or any paid Unsplash asset. Keep the source manifest alongside the files at \
         assets/unsplash/manifest.json (one entry per image with file and source_url). They are \
         referenced from chapters exactly like the generated figures."
    } else {
        "Photographs are optional for this book (require_unsplash = false); when you use any, \
         record their source in assets/unsplash/manifest.json."
    }
}

/// Mirror the current chapter into `state`.
fn publish_chapter_state(data: &RunData, b: &serde_json::Map<String, Value>) {
    let chapters = b
        .get("chapters")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let cursor = b
        .get("current_chapter_index")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let root = PathBuf::from(text(b.get("output_dir")));
    let wd = data
        .working_dir()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let (number, title, outline, path) = match chapters.get(cursor) {
        Some(c) => {
            let title = text(c.get("title"));
            let path = root.join("chapters").join(format!(
                "{:02}-{}.md",
                cursor + 1,
                chapter_slug(&title)
            ));
            (
                (cursor + 1).to_string(),
                title,
                text(c.get("outline")),
                relative(&path, &wd),
            )
        }
        None => (String::new(), String::new(), String::new(), String::new()),
    };
    let written: Vec<String> = chapters
        .iter()
        .filter(|c| text(c.get("status")) == "written")
        .map(|c| format!("  {}", text(c.get("file_path"))))
        .collect();
    data.set_state_values([
        ("book_chapter_number", number),
        ("book_chapter_title", title),
        ("book_chapter_outline", outline),
        ("book_chapter_path", path),
        (
            "book_chapter_files",
            if written.is_empty() {
                "  (none yet)".to_string()
            } else {
                written.join("\n")
            },
        ),
    ]);
}

/// agenthicc's `submit_toc` gate: read and validate the run-scoped manifest.
fn submit_toc(data: &RunData, args: &Value) -> Result<String, String> {
    let summary = summary(args, "submit_toc")?;
    let manifest = toc_manifest_path(data).ok_or("the run has no working directory")?;
    let wd = data
        .working_dir()
        .ok_or("the run has no working directory")?
        .to_path_buf();
    if !manifest.is_file() {
        return Err(failure(
            format!("TOC manifest '{}' was not found.", manifest.display()),
            "Write the JSON manifest at the exact path from the phase prompt, then retry \
             submit_toc(summary=...).",
        ));
    }
    let raw: Value = std::fs::read_to_string(&manifest)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
        .map_err(|e| {
            failure(
                format!(
                    "TOC manifest '{}' is not valid JSON ({e}).",
                    manifest.display()
                ),
                "Write a JSON object containing title and chapters, then retry.",
            )
        })?;
    let Some(plan) = raw.as_object() else {
        return Err(failure(
            "TOC manifest must contain a JSON object.",
            "Use {title, chapters, author, ...} in toc.json.",
        ));
    };
    let title = text(plan.get("title")).trim().to_string();
    let Some(chapters) = plan.get("chapters").and_then(Value::as_array) else {
        return Err(failure(
            "TOC manifest chapters must be a list.",
            "Put chapters: [{title: ..., outline: ...}] in toc.json, then retry submit_toc(summary=...).",
        ));
    };
    let author = Some(text(plan.get("author")).trim().to_string())
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| "Anonymous".into());
    let audience = text(plan.get("audience")).trim().to_string();
    let level = Some(text(plan.get("technical_level")).trim().to_string())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "advanced".into());
    let prerequisites = text(plan.get("prerequisites")).trim().to_string();
    if title.is_empty() {
        return Err(failure(
            "TOC manifest title is empty.",
            "Put a non-empty title in toc.json, then retry submit_toc(summary=...).",
        ));
    }
    if chapters.is_empty() {
        return Err(failure(
            "submit_toc plan.chapters is empty.",
            "Provide at least one chapter with a title and outline.",
        ));
    }
    if !["intermediate", "advanced", "expert"].contains(&level.as_str()) {
        return Err(failure(
            format!("Unsupported technical_level '{level}'."),
            "Use 'intermediate', 'advanced', or 'expert'.",
        ));
    }
    let types: Vec<String> = match plan.get("content_types") {
        None | Some(Value::Null) => CONTENT_TYPES.iter().map(|t| t.to_string()).collect(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| v.as_str().map(str::trim).unwrap_or_default().to_string())
            .collect(),
        Some(_) => {
            return Err(failure(
                "TOC manifest content_types must be a list.",
                "Use a list of images, equations, code, and tables.",
            ));
        }
    };
    let bad: Vec<&String> = types
        .iter()
        .filter(|t| !CONTENT_TYPES.contains(&t.as_str()))
        .collect();
    if !bad.is_empty() {
        return Err(failure(
            format!("Unsupported content_types: {bad:?}."),
            &format!("Use only: {}.", CONTENT_TYPES.join(", ")),
        ));
    }
    let cleaned: Vec<Value> = chapters
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|c| {
            let t = text(c.get("title")).trim().to_string();
            (!t.is_empty()).then(|| {
                json!({
                    "title": t,
                    "outline": text(c.get("outline")).trim(),
                    "file_path": "",
                    "word_count": 0,
                    "status": "pending",
                    "assets": [],
                })
            })
        })
        .collect();
    if cleaned.is_empty() {
        return Err(failure(
            "No valid chapters were supplied.",
            "Each chapter needs a non-empty 'title'; outline is recommended.",
        ));
    }
    let requested = text(plan.get("output_dir")).trim().to_string();
    if requested.contains('\0') {
        return Err(failure(
            "TOC manifest output_dir contains a NUL character.",
            "Use a normal book directory or omit output_dir.",
        ));
    }
    let requested = if requested.is_empty() {
        build::slugify(&title)
    } else {
        requested
    };
    let output = layout::normalise(&{
        let p = PathBuf::from(&requested);
        if p.is_absolute() { p } else { wd.join(p) }
    });
    let wd_norm = wd.canonicalize().unwrap_or(wd.clone());
    if !output.starts_with(&wd) && !output.starts_with(&wd_norm)
        || output == wd
        || output == wd_norm
    {
        return Err(failure(
            format!(
                "output_dir '{requested}' resolves to {}, outside a sub-directory of the working directory.",
                output.display()
            ),
            "Use a book directory inside the working directory, e.g. the title slug.",
        ));
    }
    std::fs::create_dir_all(&output)
        .map_err(|e| format!("cannot create {}: {e}", output.display()))?;

    let chapter_count = cleaned.len();
    let list: Vec<String> = cleaned
        .iter()
        .enumerate()
        .map(|(i, c)| {
            format!(
                "  {}. {} — {}",
                i + 1,
                text(c.get("title")),
                text(c.get("outline"))
            )
        })
        .collect();
    let toc_summary = format!(
        "Title: {title}\nAuthor: {author}\nAudience: {}\nTechnical level: {level}\nPrerequisites: {}\n\
         Content: {}\nChapters: {chapter_count}\n{}",
        if audience.is_empty() {
            "(unspecified)"
        } else {
            &audience
        },
        if prerequisites.is_empty() {
            "(unspecified)"
        } else {
            &prerequisites
        },
        types.join(", "),
        cleaned
            .iter()
            .enumerate()
            .map(|(i, c)| format!("  {}. {}", i + 1, text(c.get("title"))))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let require_unsplash = data.param_bool("require_unsplash", true);
    let mut b = book(data);
    b.insert("title".into(), json!(title));
    b.insert("author".into(), json!(author));
    b.insert("audience".into(), json!(audience));
    b.insert("technical_level".into(), json!(level));
    b.insert("prerequisites".into(), json!(prerequisites));
    b.insert("content_types".into(), json!(types));
    b.insert("output_dir".into(), json!(output.display().to_string()));
    b.insert(
        "assets_dir".into(),
        json!(output.join("assets").display().to_string()),
    );
    b.insert(
        "research_dir".into(),
        json!(output.join("research").display().to_string()),
    );
    b.insert(
        "toc_manifest_path".into(),
        json!(manifest.display().to_string()),
    );
    b.insert("chapters".into(), Value::Array(cleaned));
    b.insert("current_chapter_index".into(), json!(0));
    b.insert("toc_summary".into(), json!(toc_summary));
    b.insert("summary".into(), json!(summary));
    receipt(
        &mut b,
        "toc",
        "accepted",
        json!({
            "summary": compact(&summary, 1_000),
            "manifest_path": manifest.display().to_string(),
            "title": title,
            "chapter_count": chapter_count,
            "output_dir": output.display().to_string(),
        }),
    );
    let min_assets = std::cmp::max(6, chapter_count * 3);
    data.set_state_values([
        ("book_title", title.clone()),
        ("book_author", author.clone()),
        (
            "book_audience",
            if audience.is_empty() {
                "(specialist reader)".into()
            } else {
                audience
            },
        ),
        ("book_technical_level", level.clone()),
        (
            "book_prerequisites",
            if prerequisites.is_empty() {
                "(assume domain knowledge)".into()
            } else {
                prerequisites
            },
        ),
        ("book_content_types", types.join(", ")),
        ("book_output_dir", relative(&output, &wd)),
        ("book_assets_dir", relative(&output.join("assets"), &wd)),
        ("book_research_dir", relative(&output.join("research"), &wd)),
        ("book_toc_summary", toc_summary),
        ("book_chapter_count", chapter_count.to_string()),
        ("book_chapter_list", list.join("\n")),
        ("book_min_assets", min_assets.to_string()),
        ("book_rich_content", rich_content_rules(&types)),
        (
            "book_unsplash_rule",
            unsplash_rule(require_unsplash).to_string(),
        ),
        (
            "book_research_files",
            "  (no research file inventory recorded)".to_string(),
        ),
        ("book_asset_files", "  (none)".to_string()),
        ("book_front_matter_files", "  (none)".to_string()),
        ("book_back_matter_files", "  (none)".to_string()),
    ]);
    publish_chapter_state(data, &b);
    save(data, b);
    Ok(format!(
        "Table of contents recorded: '{title}' with {chapter_count} chapters (level: {level}, \
         content: {}). One chapter phase will run per chapter.",
        types.join(", ")
    ))
}

/// agenthicc's `submit_research` gate: research files, one notes file per
/// chapter (`ch01-notes.md`, …).
fn submit_research(data: &RunData, args: &Value) -> Result<String, String> {
    let summary = summary(args, "submit_research")?;
    let mut b = book(data);
    let dir = text(b.get("research_dir"));
    if dir.is_empty() {
        return Err(failure(
            "research directory is not configured.",
            "Run the TOC phase first.",
        ));
    }
    let root = PathBuf::from(&dir);
    let files = files_under(&root, RESEARCH_SUFFIXES);
    if files.is_empty() {
        return Err(failure(
            format!("no research files were found under '{}'.", root.display()),
            "Write the chapter notes, sources, and summary files, then retry.",
        ));
    }
    static CHAPTER: OnceLock<Regex> = OnceLock::new();
    let re = CHAPTER.get_or_init(|| {
        Regex::new(r"(?:^|[/\\])(?:ch|chapter)[-_]?0*(\d+)(?:[-_]|\.)").expect("valid")
    });
    let canonical_root = root.canonicalize().unwrap_or(root.clone());
    let mut covered = BTreeSet::new();
    for f in &files {
        let rel = relative(f, &canonical_root).to_lowercase();
        if let Some(c) = re.captures(&rel) {
            let n: usize = c[1].parse().unwrap_or(0);
            covered.insert(n.saturating_sub(1));
        }
    }
    let count = b
        .get("chapters")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let missing: Vec<usize> = (0..count).filter(|i| !covered.contains(i)).collect();
    if !missing.is_empty() {
        return Err(failure(
            format!(
                "research handoff is missing chapter note files for {missing:?} under '{}'.",
                root.display()
            ),
            "Use names such as ch01-notes.md, ch02-notes.md, then retry submit_research(summary=...).",
        ));
    }
    let wd = data
        .working_dir()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let stored: Vec<String> = files.iter().map(|f| relative(f, &canonical_root)).collect();
    let shown: Vec<String> = files
        .iter()
        .map(|f| {
            format!(
                "  {}",
                relative(f, &wd.canonicalize().unwrap_or(wd.clone()))
            )
        })
        .collect();
    b.insert("research_files".into(), json!(stored));
    b.insert("research_summary".into(), json!(summary));
    receipt(
        &mut b,
        "research",
        "accepted",
        json!({ "files": stored, "chapter_count": count }),
    );
    save(data, b);
    data.set_state_values([("book_research_files", shown.join("\n"))]);
    Ok(format!(
        "Research recorded: {count} chapters covered, {} files stored under the research/ \
         directory. The assets phase starts next.",
        stored.len()
    ))
}

/// agenthicc's free-Unsplash URL rule.
fn is_free_unsplash_url(source: &str) -> bool {
    let lower = source.to_lowercase();
    let Some(rest) = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
    else {
        return false;
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .trim_end_matches('.');
    (host == "unsplash.com" || host.ends_with(".unsplash.com"))
        && !host.starts_with("plus.")
        && !lower.contains("unsplash+")
        && !lower.contains("unsplash.plus")
}

/// agenthicc's `confirm_assets_ready` gate.
fn confirm_assets(data: &RunData, args: &Value) -> Result<String, String> {
    let summary = summary(args, "confirm_assets_ready")?;
    let mut b = book(data);
    let dir = text(b.get("assets_dir"));
    if dir.is_empty() {
        return Err(failure(
            "assets directory is not configured.",
            "Use the asset directory supplied by the phase prompt.",
        ));
    }
    let root = PathBuf::from(&dir);
    let assets = files_under(&root, ASSET_SUFFIXES);
    let chapters = b
        .get("chapters")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let minimum = std::cmp::max(6, chapters * 3);
    if assets.len() < minimum {
        return Err(failure(
            format!(
                "only {} supported assets were found under '{}'; {minimum} are required.",
                assets.len(),
                root.display()
            ),
            "Create many varied assets in the assets directory, then retry confirm_assets_ready(summary=...).",
        ));
    }
    let canonical = root.canonicalize().unwrap_or(root.clone());
    let unsplash_rasters: Vec<&PathBuf> = assets
        .iter()
        .filter(|p| {
            p.extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .is_some_and(|e| RASTER_SUFFIXES.contains(&e.as_str()))
                && p.strip_prefix(&canonical)
                    .map(|rel| {
                        rel.components().any(|c| {
                            c.as_os_str()
                                .to_string_lossy()
                                .eq_ignore_ascii_case("unsplash")
                        })
                    })
                    .unwrap_or(false)
        })
        .collect();
    let mut manifest_rel = String::new();
    if data.param_bool("require_unsplash", true) {
        if unsplash_rasters.is_empty() {
            return Err(failure(
                format!(
                    "no Unsplash image was found under '{}/unsplash/'.",
                    root.display()
                ),
                "Use the free Unsplash service, save at least one raster result under \
                 assets/unsplash/, never Unsplash+, write assets/unsplash/manifest.json, then \
                 retry confirm_assets_ready(summary=...).",
            ));
        }
        let manifest = canonical.join("unsplash").join("manifest.json");
        let parsed: Value = std::fs::read_to_string(&manifest)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
            .map_err(|e| {
                failure(
                    format!("the Unsplash provenance manifest is missing or invalid ({e})."),
                    "Write valid JSON to assets/unsplash/manifest.json with the free Unsplash \
                     source URL for each downloaded image; never record Unsplash+ assets.",
                )
            })?;
        let raw_entries: Vec<Value> = match &parsed {
            Value::Array(a) => a.clone(),
            Value::Object(o) => ["images", "assets", "entries"]
                .iter()
                .find_map(|k| o.get(*k).and_then(Value::as_array).cloned())
                .unwrap_or_else(|| vec![parsed.clone()]),
            _ => Vec::new(),
        };
        let entries: Vec<&serde_json::Map<String, Value>> =
            raw_entries.iter().filter_map(Value::as_object).collect();
        if raw_entries.is_empty() || entries.is_empty() {
            return Err(failure(
                "the Unsplash provenance manifest has no image entries.",
                "Record one JSON object per free Unsplash image with file and source_url fields.",
            ));
        }
        if entries.len() != raw_entries.len() {
            return Err(failure(
                "the Unsplash provenance manifest contains malformed entries.",
                "Make every manifest entry a JSON object with file and source_url fields.",
            ));
        }
        let mut sources = Vec::new();
        let mut declared = BTreeSet::new();
        for e in &entries {
            if let Some(s) = ["source_url", "url", "download_url", "photo_url"]
                .iter()
                .find_map(|k| e.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()))
            {
                sources.push(s.to_string());
            }
            if let Some(f) = ["file", "path", "filename"].iter().find_map(|k| {
                e.get(*k)
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
            }) {
                if let Some(name) = Path::new(f).file_name() {
                    declared.insert(name.to_string_lossy().to_lowercase());
                }
            }
        }
        if sources.is_empty() || sources.iter().any(|s| !is_free_unsplash_url(s)) {
            return Err(failure(
                "the Unsplash provenance manifest does not prove free Unsplash sources only.",
                "Record free unsplash.com source URLs in assets/unsplash/manifest.json and remove \
                 any Unsplash+ or paid source.",
            ));
        }
        if entries.len() < unsplash_rasters.len()
            || unsplash_rasters.iter().any(|p| {
                !declared.contains(
                    &p.file_name()
                        .map(|n| n.to_string_lossy().to_lowercase())
                        .unwrap_or_default(),
                )
            })
        {
            return Err(failure(
                "the Unsplash provenance manifest does not cover every downloaded image.",
                "Add one manifest entry with file and free source_url for every raster under \
                 assets/unsplash/.",
            ));
        }
        manifest_rel = relative(&manifest, canonical.parent().unwrap_or(&canonical));
    }
    let base = canonical
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or(canonical.clone());
    let listed: Vec<String> = assets.iter().map(|p| relative(p, &base)).collect();
    b.insert("assets".into(), json!(listed));
    b.insert("assets_summary".into(), json!(summary));
    if !manifest_rel.is_empty() {
        b.insert("unsplash_manifest".into(), json!(manifest_rel));
    }
    receipt(
        &mut b,
        "assets",
        "accepted",
        json!({ "assets": listed, "asset_count": listed.len(), "unsplash_manifest": manifest_rel }),
    );
    save(data, b);
    data.set_state_values([
        (
            "book_asset_files",
            listed
                .iter()
                .map(|a| format!("  {a}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        ("book_asset_count", listed.len().to_string()),
    ]);
    Ok(format!(
        "{} assets confirmed. The chapter phases start next.",
        listed.len()
    ))
}

/// agenthicc's `_inspect_chapter_artifact`: the canonical chapter file of
/// the cursor's chapter, its words and its referenced assets.
fn inspect_chapter(b: &serde_json::Map<String, Value>, index: usize) -> Result<Value, String> {
    let root = root_of(b)?;
    let root = root.canonicalize().unwrap_or(root);
    let assets = PathBuf::from(text(b.get("assets_dir")));
    let assets = assets.canonicalize().unwrap_or(assets);
    let chapters = b
        .get("chapters")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let chapter = chapters
        .get(index)
        .ok_or_else(|| format!("chapter {} is not in the table of contents", index + 1))?;
    let title = text(chapter.get("title"));
    let path = root
        .join("chapters")
        .join(format!("{:02}-{}.md", index + 1, chapter_slug(&title)));
    if !path.is_file() {
        return Err(format!(
            "chapter {} canonical artifact was not found at {} or is outside the book output directory",
            index + 1,
            path.display()
        ));
    }
    let contents = std::fs::read_to_string(&path)
        .map_err(|_| format!("chapter {} artifact could not be read", index + 1))?;
    if contents.trim().is_empty() {
        return Err(format!("chapter {} artifact is empty", index + 1));
    }
    let mut referenced = Vec::new();
    for (target, _) in image_references(&contents) {
        let candidate = target.trim();
        if candidate.is_empty() {
            continue;
        }
        let resolved = resolve_image(candidate, &path, &root)
            .filter(|p| p.is_file() && p.starts_with(&assets))
            .ok_or_else(|| {
                format!(
                    "chapter {} has invalid referenced assets: '{candidate}' is missing or outside \
                     the assets directory",
                    index + 1
                )
            })?;
        let rel = relative(&resolved, &root);
        if !referenced.contains(&rel) {
            referenced.push(rel);
        }
    }
    referenced.sort();
    static WORD: OnceLock<Regex> = OnceLock::new();
    let words = WORD
        .get_or_init(|| Regex::new(r"\b[\w][\w'’-]*\b").expect("valid"))
        .find_iter(&contents)
        .count();
    Ok(json!({
        "chapter_index": index,
        "file_path": relative(&path, &root),
        "word_count": words,
        "assets": referenced,
        "bytes": contents.len(),
    }))
}

/// agenthicc's `confirm_chapter_complete` gate; advances the cursor.
fn confirm_chapter(data: &RunData, args: &Value) -> Result<String, String> {
    let summary = summary(args, "confirm_chapter_complete")?;
    let mut b = book(data);
    let index = b
        .get("current_chapter_index")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;
    let inspected = inspect_chapter(&b, index).map_err(|e| {
        failure(
            e,
            "Write the canonical chapter and ensure all referenced assets exist, then retry.",
        )
    })?;
    let mut chapters = b
        .get("chapters")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(c) = chapters.get_mut(index) {
        c["file_path"] = inspected["file_path"].clone();
        c["word_count"] = inspected["word_count"].clone();
        c["assets"] = inspected["assets"].clone();
        c["status"] = json!("written");
        c["summary"] = json!(summary);
    }
    b.insert("chapters".into(), Value::Array(chapters));
    b.insert("current_chapter_index".into(), json!(index + 1));
    receipt(&mut b, "chapter", "accepted", inspected.clone());
    publish_chapter_state(data, &b);
    save(data, b);
    Ok(format!(
        "Chapter {} confirmed at '{}' ({} words).",
        index + 1,
        text(inspected.get("file_path")),
        inspected["word_count"]
    ))
}

/// agenthicc's `confirm_layout_ready` gate (both reviews).
fn confirm_layout(data: &RunData, args: &Value, phase: &str) -> Result<String, String> {
    let summary = summary(args, "confirm_layout_ready")?;
    let mut b = book(data);
    let root = root_of(&b)?;
    let assets = PathBuf::from(text(b.get("assets_dir")));
    let findings = layout_findings(&root, &assets);
    if !findings.issues.is_empty() {
        let shown: Vec<String> = findings
            .issues
            .iter()
            .take(12)
            .map(|i| format!("- {i}"))
            .collect();
        return Err(format!(
            "layout bounds are not satisfied:\n{}\nFix: Edit the existing Markdown sources: every \
             image/diagram must fit within {TARGET_MEDIA_WIDTH_IN:.3}in (95% of the \
             {CONTENT_WIDTH_IN:.2}in content width) and {MAX_MEDIA_HEIGHT_IN:.2}in (70% of the \
             {PAGE_HEIGHT_IN:.1}in page height); every table must fit within the same \
             width/height budget; then retry.",
            shown.join("\n")
        ));
    }
    b.insert(
        phase.to_string(),
        json!({
            "summary": summary,
            "files": findings.files,
            "image_count": findings.image_count,
            "table_count": findings.table_count,
        }),
    );
    receipt(
        &mut b,
        phase,
        "accepted",
        json!({
            "files": findings.files,
            "image_count": findings.image_count,
            "table_count": findings.table_count,
            "max_width_in": TARGET_MEDIA_WIDTH_IN,
            "max_height_in": MAX_MEDIA_HEIGHT_IN,
        }),
    );
    save(data, b);
    Ok(format!(
        "Layout confirmed for {} sources: {} images/diagrams and {} tables fit the \
         {TARGET_MEDIA_WIDTH_IN:.3}in x {MAX_MEDIA_HEIGHT_IN:.1}in media bounds.",
        findings.files.len(),
        findings.image_count,
        findings.table_count
    ))
}

/// agenthicc's `confirm_front_matter_ready` / `confirm_back_matter_ready`.
fn confirm_matter(data: &RunData, args: &Value, dir: &str) -> Result<String, String> {
    let tool = if dir == "front-matter" {
        "confirm_front_matter_ready"
    } else {
        "confirm_back_matter_ready"
    };
    let summary = summary(args, tool)?;
    let mut b = book(data);
    let root = root_of(&b)?;
    let root = root.canonicalize().unwrap_or(root);
    let files = files_under(&root.join(dir), &["md"]);
    if files.iter().any(|p| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("contents.md"))
    }) {
        return Err(failure(
            format!(
                "contents.md must not be written in {dir}/; create_build_book generates the table of contents."
            ),
            &format!("Remove contents.md and retry {tool}(summary=...)."),
        ));
    }
    if files.is_empty() {
        return Err(failure(
            format!(
                "{} is missing under '{}'.",
                dir.replace('-', " "),
                root.join(dir).display()
            ),
            &format!(
                "Write the {} Markdown file there; create_build_book generates the table of \
                 contents. Then retry {tool}(summary=...).",
                if dir == "front-matter" {
                    "preface"
                } else {
                    "index"
                }
            ),
        ));
    }
    let listed: Vec<String> = files.iter().map(|p| relative(p, &root)).collect();
    let key = if dir == "front-matter" {
        "front_matter"
    } else {
        "back_matter"
    };
    b.insert(format!("{key}_files"), json!(listed));
    b.insert(format!("{key}_summary"), json!(summary));
    receipt(
        &mut b,
        &dir.replace('-', "_"),
        "accepted",
        json!({ "files": listed }),
    );
    save(data, b);
    data.set_state_values([(
        if dir == "front-matter" {
            "book_front_matter_files"
        } else {
            "book_back_matter_files"
        },
        listed
            .iter()
            .map(|f| format!("  {f}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )]);
    Ok(format!(
        "{} confirmed.",
        if dir == "front-matter" {
            "Front matter"
        } else {
            "Back matter"
        }
    ))
}

/// The files directly under `dist/` with `ext`.
fn dist_files(dist: &Path, ext: &str) -> Vec<PathBuf> {
    files_under(dist, &[ext])
}

/// agenthicc's `mark_book_complete`: validate the build and its outputs.
fn mark_complete(data: &RunData, args: &Value) -> Result<String, String> {
    let summary = compact(&summary(args, "mark_book_complete")?, 2_000);
    let mut b = book(data);
    let root = root_of(&b)?;
    let root = root.canonicalize().unwrap_or(root);
    let dist = root.join("dist");
    let record: Value = std::fs::read_to_string(dist.join(RECORD_NAME))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| {
            failure(
                format!("no build record exists under '{}'.", dist.display()),
                "Call create_build_book() first, then retry mark_book_complete(summary=...).",
            )
        })?;
    let built: Vec<(String, String)> = record["sources"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| (text(s.get("path")), text(s.get("sha256"))))
                .collect()
        })
        .unwrap_or_default();
    if built != sources_fingerprint(&root) {
        return Err(failure(
            "the book sources changed after the last build.",
            "Call create_build_book() again so the outputs match the sources, then retry.",
        ));
    }
    let epubs = dist_files(&dist, "epub");
    if epubs.len() != 1 {
        return Err(failure(
            format!(
                "expected exactly one EPUB under '{}', found {}.",
                dist.display(),
                epubs.len()
            ),
            "Remove stale EPUBs, call create_build_book() again, and retry.",
        ));
    }
    let epub = &epubs[0];
    let entries = std::fs::read(epub)
        .map_err(|e| e.to_string())
        .and_then(|bytes| zip::read_stored(&bytes))
        .map_err(|e| {
            failure(
                format!(
                    "artifact '{}' could not be validated ({e}).",
                    epub.display()
                ),
                "Rebuild with create_build_book().",
            )
        })?;
    if entries.first().map(|(n, d)| (n.as_str(), d.as_slice()))
        != Some(("mimetype", b"application/epub+zip".as_slice()))
        || !entries.iter().any(|(n, _)| n.ends_with(".opf"))
    {
        return Err(failure(
            format!(
                "artifact '{}' does not contain EPUB mimetype and OPF metadata.",
                epub.display()
            ),
            "Rebuild with create_build_book().",
        ));
    }
    let slug = epub
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let html = dist.join(format!("{slug}.html"));
    if !html.is_file() {
        return Err(failure(
            format!("the HTML book '{}' is missing.", html.display()),
            "Rebuild with create_build_book().",
        ));
    }
    let pdfs = dist_files(&dist, "pdf");
    let pdf_expected = record.get("pdf").and_then(Value::as_str).is_some();
    let pdf_rel = if pdf_expected || !pdfs.is_empty() {
        if pdfs.len() != 1 {
            return Err(failure(
                format!(
                    "expected exactly one PDF under '{}', found {}.",
                    dist.display(),
                    pdfs.len()
                ),
                "Remove stale PDFs, call create_build_book() again, and retry.",
            ));
        }
        let pdf = &pdfs[0];
        if pdf.file_stem() != epub.file_stem() {
            return Err(failure(
                format!(
                    "EPUB '{}' does not match the PDF '{}' by filename.",
                    epub.display(),
                    pdf.display()
                ),
                "Build both with create_build_book() so the PDF and EPUB pair up.",
            ));
        }
        let header = std::fs::read(pdf)
            .ok()
            .map(|b| b.get(..5).map(<[u8]>::to_vec).unwrap_or_default());
        if header.as_deref() != Some(b"%PDF-") {
            return Err(failure(
                format!(
                    "artifact '{}' does not have a valid PDF header.",
                    pdf.display()
                ),
                "Rebuild with create_build_book().",
            ));
        }
        Some(relative(pdf, &root))
    } else {
        None
    };
    let epub_rel = relative(epub, &root);
    let html_rel = relative(&html, &root);
    b.insert("epub_path".into(), json!(epub_rel));
    b.insert("html_path".into(), json!(html_rel));
    b.insert("pdf_path".into(), json!(pdf_rel));
    b.insert("compile_summary".into(), json!(summary));
    receipt(
        &mut b,
        "compile",
        "accepted",
        json!({
            "pdf_path": pdf_rel,
            "epub_path": epub_rel,
            "html_path": html_rel,
            "pdf_note": record.get("pdf_note"),
        }),
    );
    save(data, b);
    Ok(match &pdf_rel {
        Some(pdf) => format!("Book marked as complete: {pdf}, {epub_rel} and {html_rel}."),
        None => format!(
            "Book marked as complete: {epub_rel} and {html_rel} (no PDF: {}).",
            text(record.get("pdf_note"))
        ),
    })
}

// ── The `book` tool group ──────────────────────────────────────────────────

/// The compile phase's builder tools.
///
/// `[workflows.make_book] pdf = false` skips the PDF even when pandoc is
/// installed.
pub(crate) fn book_tools(data: &RunData) -> Vec<Arc<ErasedTool>> {
    let pdf = if data.param_bool("pdf", true) {
        PdfTool::Auto
    } else {
        PdfTool::Disabled
    };
    [Kind::Create, Kind::List, Kind::Read]
        .into_iter()
        .map(|kind| {
            Arc::new(BookTool {
                kind,
                data: data.clone(),
                pdf: pdf.clone(),
            }) as Arc<ErasedTool>
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Create,
    List,
    Read,
}

struct BookTool {
    kind: Kind,
    data: RunData,
    pdf: PdfTool,
}

fn reference_sections() -> Vec<(usize, String)> {
    BUILDER_REFERENCE
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim_start();
            l.starts_with("pub(crate) fn ")
                || l.starts_with("fn ")
                || l.starts_with("pub(crate) struct ")
                || l.starts_with("pub(crate) enum ")
                || l.starts_with("struct ")
                || l.starts_with("const ")
                || l.starts_with("pub(crate) const ")
                || t.starts_with("// ── ")
        })
        .map(|(i, l)| (i + 1, l.trim().to_string()))
        .collect()
}

impl BookTool {
    async fn create(&self) -> Result<String, String> {
        let b = book(&self.data);
        let root = root_of(&b).map_err(|_| {
            failure(
                "No output directory is available for the book builder.",
                "Set the book output directory (the TOC phase) before building.",
            )
        })?;
        let meta = BookMeta {
            title: Some(text(b.get("title")))
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "Untitled Book".into()),
            author: Some(text(b.get("author")))
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "Anonymous".into()),
        };
        let pdf = self.pdf.clone();
        let built = tokio::task::spawn_blocking(move || build_book(&root, &meta, &pdf))
            .await
            .map_err(|e| format!("the builder failed: {e}"))?
            .map_err(|e| {
                failure(
                    format!("Could not build the book: {e}"),
                    "Fix the sources and call create_build_book() again.",
                )
            })?;
        let mut b = book(&self.data);
        b.insert(
            "build_script_path".into(),
            json!(built.script.display().to_string()),
        );
        save(&self.data, b);
        let mut lines = vec![
            format!(
                "Built '{}' from {} sources ({}).",
                built.slug,
                built.sources.len(),
                built.sources.join(", ")
            ),
            format!("Chapter titles found: {}", built.titles.join(" | ")),
            format!("EPUB: {}", built.epub.display()),
            format!("HTML: {}", built.html.display()),
            format!("Markdown: {}", built.markdown.display()),
            match &built.pdf {
                Ok(p) => format!("PDF: {}", p.display()),
                Err(e) => format!("PDF: not built — {e}"),
            },
            format!(
                "Images embedded: {}; cover: {}",
                built.images,
                if built.cover {
                    "user-supplied assets/cover"
                } else {
                    "none (assets/cover.png not found)"
                }
            ),
            format!("Rebuild outside a session: {}", built.script.display()),
        ];
        if !built.missing_images.is_empty() {
            lines.push(format!(
                "MISSING IMAGES (fix the references and rebuild):\n{}",
                built
                    .missing_images
                    .iter()
                    .map(|m| format!("  - {m}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        Ok(lines.join("\n"))
    }

    fn list(&self) -> String {
        let sections = reference_sections();
        json!({
            "ok": true,
            "path": "crates/xaft-runtime/src/phased/book/build.rs",
            "line_count": BUILDER_REFERENCE.lines().count(),
            "sections": sections
                .iter()
                .map(|(line, definition)| json!({"line": line, "definition": definition}))
                .collect::<Vec<_>>(),
            "message": "This is the source of xaft's book builder — create_build_book runs exactly this code. \
                        Read the ranges you need with read_build_book_reference(start_line, end_line)."
        })
        .to_string()
    }

    fn read(&self, input: &Value) -> Result<String, String> {
        let int = |k: &str, default: i64| match input.get(k) {
            Some(Value::Number(n)) => n.as_i64().unwrap_or(default),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(default),
            _ => default,
        };
        let start = int("start_line", 1);
        let end = int("end_line", 160);
        if start < 1 || end < start {
            return Err("start_line must be >= 1 and end_line must be >= start_line.".into());
        }
        if end - start + 1 > 200 {
            return Err("Read at most 200 reference lines per call.".into());
        }
        let lines: Vec<&str> = BUILDER_REFERENCE.lines().collect();
        if start as usize > lines.len() {
            return Err(format!(
                "start_line {start} is beyond the {}-line reference.",
                lines.len()
            ));
        }
        let end = (end as usize).min(lines.len());
        Ok(json!({
            "ok": true,
            "start_line": start,
            "end_line": end,
            "content": lines[start as usize - 1..end].join("\n"),
        })
        .to_string())
    }
}

#[async_trait::async_trait]
impl Tool for BookTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        match self.kind {
            Kind::Create => "create_build_book",
            Kind::List => "list_build_book_reference",
            Kind::Read => "read_build_book_reference",
        }
    }

    fn description(&self) -> &str {
        match self.kind {
            Kind::Create => {
                "Build the book: compile front-matter/, chapters/ and back-matter/ into \
                 dist/<slug>.epub, a self-contained dist/<slug>.html and dist/<slug>.md, plus \
                 dist/<slug>.pdf when pandoc is on PATH (it says so when it is not), and write \
                 build_book.sh. Call it again after every source fix."
            }
            Kind::List => {
                "List the book builder reference (the builder's source) and its sections."
            }
            Kind::Read => {
                "Read a bounded line range (at most 200 lines) of the book builder reference."
            }
        }
    }

    fn schema(&self) -> Value {
        match self.kind {
            Kind::Read => json!({
                "type": "object",
                "properties": {
                    "start_line": {"type": "integer", "description": "First line (1-based)."},
                    "end_line": {"type": "integer", "description": "Last line (inclusive)."}
                },
                "required": [],
                "additionalProperties": false
            }),
            _ => {
                json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false})
            }
        }
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        let outcome = match self.kind {
            Kind::Create => self.create().await,
            Kind::List => Ok(self.list()),
            Kind::Read => self.read(&input),
        };
        Ok(match outcome {
            Ok(text) => ToolResult::ok(text, &ctx.tool_use_id),
            Err(text) => ToolResult::error(text, &ctx.tool_use_id),
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::layout::tests::png;
    use super::*;
    use std::collections::BTreeMap;

    pub(crate) fn data_in(dir: &Path, params: &[(&str, toml::Value)]) -> RunData {
        let params: BTreeMap<String, toml::Value> = params
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect();
        RunData::new(BTreeMap::new(), dir).with_run("run-42", &params)
    }

    pub(crate) fn write_toc(data: &RunData, chapters: &[&str]) {
        let path = toc_manifest_path(data).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let chapters: Vec<Value> = chapters
            .iter()
            .map(|t| json!({"title": t, "outline": format!("- {t} details")}))
            .collect();
        std::fs::write(
            &path,
            json!({
                "title": "Kalman Filtering",
                "author": "Ada",
                "technical_level": "expert",
                "output_dir": "kalman",
                "content_types": ["images", "code"],
                "chapters": chapters,
            })
            .to_string(),
        )
        .unwrap();
    }

    fn s(summary: &str) -> Value {
        json!({ "summary": summary })
    }

    #[test]
    fn submit_toc_validates_the_manifest_and_seeds_the_prompts() {
        let dir = tempfile::tempdir().unwrap();
        let data = data_in(dir.path(), &[]);
        let err = run_effect("book_submit_toc", &data, &s("plan")).unwrap_err();
        assert!(err.contains("was not found"), "{err}");
        assert!(err.contains(".xaft/make_book/run-42/toc.json"), "{err}");

        let path = toc_manifest_path(&data).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for (body, needle) in [
            ("[1]", "must contain a JSON object"),
            (
                r#"{"title": "", "chapters": [{"title": "a"}]}"#,
                "title is empty",
            ),
            (r#"{"title": "T", "chapters": []}"#, "chapters is empty"),
            (
                r#"{"title": "T", "technical_level": "novice", "chapters": [{"title": "a"}]}"#,
                "Unsupported technical_level",
            ),
            (
                r#"{"title": "T", "content_types": ["video"], "chapters": [{"title": "a"}]}"#,
                "Unsupported content_types",
            ),
            (
                r#"{"title": "T", "output_dir": "/etc", "chapters": [{"title": "a"}]}"#,
                "inside the working directory",
            ),
        ] {
            std::fs::write(&path, body).unwrap();
            let err = run_effect("book_submit_toc", &data, &s("plan")).unwrap_err();
            assert!(err.contains(needle), "{body}: {err}");
        }

        write_toc(&data, &["Basics", "Tuning"]);
        let note = run_effect("book_submit_toc", &data, &s("plan")).unwrap();
        assert!(
            note.contains("'Kalman Filtering' with 2 chapters"),
            "{note}"
        );
        assert_eq!(data.state_value("book_chapter_number"), "1");
        assert_eq!(
            data.state_value("book_chapter_path"),
            "kalman/chapters/01-basics.md"
        );
        assert_eq!(data.state_value("book_min_assets"), "6");
        assert!(data.state_value("book_rich_content").contains("- CODE:"));
        assert!(!data.state_value("book_rich_content").contains("- TABLES:"));
        assert!(data.state_value("book_unsplash_rule").contains("MUST use"));
        assert!(chapters_complete(&data).is_err());
    }

    fn planned(
        chapters: &[&str],
        params: &[(&str, toml::Value)],
    ) -> (tempfile::TempDir, RunData, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let data = data_in(dir.path(), params);
        write_toc(&data, chapters);
        run_effect("book_submit_toc", &data, &s("plan")).unwrap();
        let root = dir.path().join("kalman");
        (dir, data, root)
    }

    #[test]
    fn research_needs_a_notes_file_per_chapter() {
        let (_dir, data, root) = planned(&["Basics", "Tuning"], &[]);
        let err = run_effect("book_submit_research", &data, &s("r")).unwrap_err();
        assert!(err.contains("no research files"), "{err}");
        std::fs::create_dir_all(root.join("research")).unwrap();
        std::fs::write(root.join("research/ch01-notes.md"), "notes").unwrap();
        std::fs::write(root.join("research/sources.md"), "src").unwrap();
        let err = run_effect("book_submit_research", &data, &s("r")).unwrap_err();
        assert!(err.contains("missing chapter note files for [1]"), "{err}");
        std::fs::write(root.join("research/chapter-2.md"), "notes").unwrap();
        let note = run_effect("book_submit_research", &data, &s("r")).unwrap();
        assert!(note.contains("2 chapters covered, 3 files"), "{note}");
        assert!(
            data.state_value("book_research_files")
                .contains("kalman/research/ch01-notes.md")
        );
    }

    #[test]
    fn assets_need_volume_and_free_unsplash_provenance() {
        let (_dir, data, root) = planned(&["Basics"], &[]);
        let assets = root.join("assets");
        std::fs::create_dir_all(assets.join("unsplash")).unwrap();
        for i in 0..5 {
            std::fs::write(assets.join(format!("fig-{i}.png")), png(100, 100)).unwrap();
        }
        let err = run_effect("book_confirm_assets", &data, &s("a")).unwrap_err();
        assert!(err.contains("only 5 supported assets"), "{err}");
        std::fs::write(assets.join("fig.mmd"), "graph TD").unwrap();
        let err = run_effect("book_confirm_assets", &data, &s("a")).unwrap_err();
        assert!(err.contains("no Unsplash image"), "{err}");
        std::fs::write(assets.join("unsplash/photo.jpg"), [0xFF, 0xD8]).unwrap();
        let err = run_effect("book_confirm_assets", &data, &s("a")).unwrap_err();
        assert!(err.contains("missing or invalid"), "{err}");
        std::fs::write(
            assets.join("unsplash/manifest.json"),
            r#"[{"file": "photo.jpg", "source_url": "https://plus.unsplash.com/x"}]"#,
        )
        .unwrap();
        let err = run_effect("book_confirm_assets", &data, &s("a")).unwrap_err();
        assert!(err.contains("free Unsplash sources only"), "{err}");
        std::fs::write(
            assets.join("unsplash/manifest.json"),
            r#"{"images": [{"file": "photo.jpg", "source_url": "https://unsplash.com/photos/abc"}]}"#,
        )
        .unwrap();
        let note = run_effect("book_confirm_assets", &data, &s("a")).unwrap();
        assert!(note.contains("7 assets confirmed"), "{note}");
    }

    #[test]
    fn the_unsplash_gate_can_be_switched_off() {
        let (_dir, data, root) = planned(
            &["Basics"],
            &[("require_unsplash", toml::Value::Boolean(false))],
        );
        std::fs::create_dir_all(root.join("assets")).unwrap();
        for i in 0..6 {
            std::fs::write(root.join(format!("assets/fig-{i}.png")), png(10, 10)).unwrap();
        }
        assert!(run_effect("book_confirm_assets", &data, &s("a")).is_ok());
        assert!(data.state_value("book_unsplash_rule").contains("optional"));
    }

    #[test]
    fn chapters_are_confirmed_in_order_until_the_guard_holds() {
        let (_dir, data, root) = planned(&["Basics", "Tuning Noise"], &[]);
        std::fs::create_dir_all(root.join("chapters")).unwrap();
        std::fs::create_dir_all(root.join("assets")).unwrap();
        let err = run_effect("book_confirm_chapter", &data, &s("c")).unwrap_err();
        assert!(
            err.contains("chapter 1 canonical artifact was not found"),
            "{err}"
        );
        std::fs::write(
            root.join("chapters/01-basics.md"),
            "# Basics\n\n![x](../assets/missing.png)\n",
        )
        .unwrap();
        let err = run_effect("book_confirm_chapter", &data, &s("c")).unwrap_err();
        assert!(err.contains("invalid referenced assets"), "{err}");
        std::fs::write(root.join("assets/fig.png"), png(10, 10)).unwrap();
        std::fs::write(
            root.join("chapters/01-basics.md"),
            "# Basics\n\nThe filter's state isn't magic.\n\n![x](../assets/fig.png)\n",
        )
        .unwrap();
        let note = run_effect("book_confirm_chapter", &data, &s("c")).unwrap();
        assert!(
            note.contains("Chapter 1 confirmed at 'chapters/01-basics.md'"),
            "{note}"
        );
        assert_eq!(data.state_value("book_chapter_number"), "2");
        assert_eq!(
            data.state_value("book_chapter_path"),
            "kalman/chapters/02-tuning-noise.md"
        );
        assert!(chapters_complete(&data).is_err());
        std::fs::write(
            root.join("chapters/02-tuning-noise.md"),
            "# Tuning Noise\n\nText.\n",
        )
        .unwrap();
        run_effect("book_confirm_chapter", &data, &s("c")).unwrap();
        assert!(chapters_complete(&data).is_ok());
        assert!(
            data.state_value("book_chapter_files")
                .contains("chapters/02-tuning-noise.md")
        );
        let b = book(&data);
        let receipts = b["transition_receipts"].as_array().unwrap();
        assert_eq!(receipts.last().unwrap()["phase"], json!("chapter"));
        assert_eq!(receipts.last().unwrap()["word_count"], json!(3));
    }

    #[test]
    fn front_and_back_matter_refuse_a_contents_file() {
        let (_dir, data, root) = planned(&["Basics"], &[]);
        let err = run_effect("book_confirm_front_matter", &data, &s("f")).unwrap_err();
        assert!(err.contains("front matter is missing"), "{err}");
        std::fs::create_dir_all(root.join("front-matter")).unwrap();
        std::fs::write(root.join("front-matter/contents.md"), "# Contents").unwrap();
        std::fs::write(root.join("front-matter/preface.md"), "# Preface").unwrap();
        let err = run_effect("book_confirm_front_matter", &data, &s("f")).unwrap_err();
        assert!(err.contains("contents.md must not be written"), "{err}");
        std::fs::remove_file(root.join("front-matter/contents.md")).unwrap();
        assert!(run_effect("book_confirm_front_matter", &data, &s("f")).is_ok());
        assert_eq!(
            data.state_value("book_front_matter_files"),
            "  front-matter/preface.md"
        );
    }

    #[test]
    fn the_reference_tools_serve_the_builder_source() {
        let sections = reference_sections();
        assert!(
            sections
                .iter()
                .any(|(_, d)| d.starts_with("pub(crate) fn build_book")),
            "{sections:?}"
        );
        let tool = BookTool {
            kind: Kind::Read,
            data: RunData::default(),
            pdf: PdfTool::Disabled,
        };
        assert!(
            tool.read(&json!({"start_line": 5, "end_line": 300}))
                .unwrap_err()
                .contains("at most 200")
        );
        assert!(tool.read(&json!({"start_line": 0})).is_err());
        let read: Value =
            serde_json::from_str(&tool.read(&json!({"start_line": 1, "end_line": 3})).unwrap())
                .unwrap();
        assert!(
            read["content"]
                .as_str()
                .unwrap()
                .starts_with("//! make_book's builder")
        );
    }

    #[tokio::test]
    async fn create_build_book_then_mark_book_complete() {
        let (_dir, data, root) = planned(&["Basics"], &[]);
        std::fs::create_dir_all(root.join("chapters")).unwrap();
        std::fs::create_dir_all(root.join("front-matter")).unwrap();
        std::fs::write(
            root.join("chapters/01-basics.md"),
            "# Basics\n\n\\newpage\n\nText.\n",
        )
        .unwrap();
        std::fs::write(root.join("front-matter/preface.md"), "# Preface\n\nWhy.\n").unwrap();
        let err = run_effect("book_mark_complete", &data, &s("done")).unwrap_err();
        assert!(err.contains("no build record"), "{err}");

        let create = BookTool {
            kind: Kind::Create,
            data: data.clone(),
            pdf: PdfTool::Disabled,
        };
        let report = create
            .call(json!({}), &ToolContext::new("t"))
            .await
            .unwrap();
        assert!(!report.is_error, "{}", report.content);
        assert!(
            report.content.contains("PDF: not built"),
            "{}",
            report.content
        );
        let note = run_effect("book_mark_complete", &data, &s("done")).unwrap();
        assert!(
            note.contains("dist/kalman-filtering.epub and dist/kalman-filtering.html"),
            "{note}"
        );

        // An edit after the build makes it stale.
        std::fs::write(
            root.join("chapters/01-basics.md"),
            "# Basics\n\nNew text.\n",
        )
        .unwrap();
        let err = run_effect("book_mark_complete", &data, &s("done")).unwrap_err();
        assert!(err.contains("changed after the last build"), "{err}");
        // A stray PDF that the build did not produce is refused.
        create
            .call(json!({}), &ToolContext::new("t"))
            .await
            .unwrap();
        std::fs::write(root.join("dist/other.pdf"), "%PDF-1.4").unwrap();
        let err = run_effect("book_mark_complete", &data, &s("done")).unwrap_err();
        assert!(err.contains("does not match the PDF"), "{err}");
    }
}
