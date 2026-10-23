//! make_book's builder — what `create_build_book` runs.
//!
//! agenthicc generates a Python `build_book.py` that drives Pandoc and
//! XeLaTeX. xaft compiles the book itself, deterministically:
//!
//! 1. **Sources** — `front-matter/`, `chapters/` and `back-matter/` under the
//!    book root, each sorted by file name; `contents.md` is excluded because
//!    the table of contents is generated (`source_files`).
//! 2. **Page breaks** — a `\newpage` / `\clearpage` / `#pagebreak()` line
//!    right after a source's first `# ` title is the prescribed "this source
//!    starts a new page" marker: every source already starts its own page, so
//!    it is dropped; one anywhere else becomes a page break (`preprocess`).
//!    Pandoc attribute blocks after images (`{width=80%}`) are dropped for
//!    HTML, where images are bounded by CSS instead.
//! 3. **Images** — a reference resolves against its source's directory, then
//!    the book root; anything outside the book root, missing, or not an image
//!    is reported and never embedded (`collect_images`).
//! 4. **EPUB 3** (`dist/<slug>.epub`) — `mimetype` first and stored,
//!    `META-INF/container.xml`, `OEBPS/content.opf` (metadata, manifest,
//!    spine), `OEBPS/nav.xhtml` (the generated table of contents, also a
//!    page of the book), `OEBPS/toc.ncx` for older readers, one XHTML file
//!    per source, the images, a stylesheet with accent-coloured headings and
//!    justified text, and the user's `assets/cover.{png,jpg,jpeg}` as the
//!    cover when present (`write_epub`).
//! 5. **HTML** (`dist/<slug>.html`) — one self-contained file: cover, table
//!    of contents, every source, images embedded as data URIs.
//! 6. **Markdown** (`dist/<slug>.md`) — the sources in order under a title
//!    block, for Pandoc.
//! 7. **PDF** (`dist/<slug>.pdf`) — only when `pandoc` is on PATH, through
//!    the first PDF engine found (xelatex, pdflatex, typst, wkhtmltopdf,
//!    weasyprint) on an 8 × 11.5 in page with 0.75 in margins; otherwise the
//!    build reports exactly why there is no PDF.
//! 8. **Record** — `dist/build.json` (outputs, missing images, and a SHA-256
//!    of every source, so a stale build is detectable) and `build_book.sh`,
//!    which rebuilds the PDF and EPUB with Pandoc outside a session.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use pulldown_cmark::{CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde_json::{Value, json};

use super::layout::{normalise, resolve_image};
use super::zip::ZipWriter;
use xaft_tools::trust::sha256_hex;

/// The build record's file name, under `dist/`.
pub(crate) const RECORD_NAME: &str = "build.json";

/// Seconds Pandoc may take to typeset the PDF.
const PANDOC_TIMEOUT: Duration = Duration::from_secs(600);

/// The accent colour of headings (agenthicc's deep navy).
const ACCENT: &str = "#1F4E79";

/// What the book is called.
#[derive(Debug, Clone)]
pub(crate) struct BookMeta {
    /// Title.
    pub title: String,
    /// Author.
    pub author: String,
}

/// How to produce the PDF.
#[derive(Debug, Clone)]
pub(crate) enum PdfTool {
    /// Find `pandoc` and a PDF engine on PATH.
    Auto,
    /// This Pandoc, with this `--pdf-engine` (or Pandoc's default) — how a
    /// test pins the toolchain.
    #[cfg_attr(not(test), allow(dead_code))]
    Pandoc {
        program: PathBuf,
        engine: Option<String>,
    },
    /// No PDF.
    Disabled,
}

/// What a build produced.
#[derive(Debug)]
pub(crate) struct BuiltBook {
    /// The file-name slug of the title.
    pub slug: String,
    /// Sources compiled, relative to the book root.
    pub sources: Vec<String>,
    /// The `# ` titles found, in order.
    pub titles: Vec<String>,
    /// `dist/<slug>.epub`.
    pub epub: PathBuf,
    /// `dist/<slug>.html`.
    pub html: PathBuf,
    /// `dist/<slug>.md`.
    pub markdown: PathBuf,
    /// The PDF, or why there is none.
    pub pdf: Result<PathBuf, String>,
    /// Images embedded.
    pub images: usize,
    /// References that could not be embedded.
    pub missing_images: Vec<String>,
    /// Whether the user's cover was used.
    pub cover: bool,
    /// `build_book.sh`.
    pub script: PathBuf,
}

/// agenthicc's `slugify_book_title`.
pub(crate) fn slugify(title: &str) -> String {
    let lower = title.to_lowercase();
    let mut out = String::new();
    let mut dash = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    let slug = out.trim_matches('-').to_string();
    if slug.is_empty() { "book".into() } else { slug }
}

/// The book's sources in reading order: front matter, chapters, back matter.
pub(crate) fn source_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in ["front-matter", "chapters", "back-matter"] {
        let mut part: Vec<PathBuf> = std::fs::read_dir(root.join(dir))
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| {
                        p.is_file()
                            && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("md"))
                            && !p.file_name().is_some_and(|n| {
                                n.to_string_lossy().eq_ignore_ascii_case("contents.md")
                            })
                    })
                    .collect()
            })
            .unwrap_or_default();
        part.sort_by_key(|p| p.file_name().map(|n| n.to_os_string()));
        files.extend(part);
    }
    files
}

/// `(relative path, sha256)` of every source, for staleness checks.
pub(crate) fn sources_fingerprint(root: &Path) -> Vec<(String, String)> {
    source_files(root)
        .iter()
        .map(|p| {
            let rel = p
                .strip_prefix(root)
                .map(|r| r.display().to_string())
                .unwrap_or_else(|_| p.display().to_string());
            let sha = std::fs::read(p).map(|b| sha256_hex(&b)).unwrap_or_default();
            (rel, sha)
        })
        .collect()
}

fn is_page_break(line: &str) -> bool {
    matches!(
        line.trim(),
        "\\newpage" | "\\clearpage" | "\\pagebreak" | "#pagebreak()"
    )
}

/// Normalise one source for HTML: drop the post-title page-break marker,
/// turn other markers into page breaks, drop Pandoc image attribute blocks.
fn preprocess(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut seen_title = false;
    let mut only_blank_since_title = false;
    for line in text.lines() {
        if is_page_break(line) {
            if !(seen_title && only_blank_since_title) {
                out.push_str("\n<div class=\"page-break\"></div>\n\n");
            }
            only_blank_since_title = false;
            continue;
        }
        if !seen_title && line.starts_with("# ") {
            seen_title = true;
            only_blank_since_title = true;
        } else if !line.trim().is_empty() {
            only_blank_since_title = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    strip_image_attributes(&out)
}

fn strip_image_attributes(text: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re =
        RE.get_or_init(|| regex::Regex::new(r"(!\[[^\]]*\]\([^)]*\))\{[^}]*\}").expect("valid"));
    re.replace_all(text, "$1").into_owned()
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn media_type(path: &Path) -> Option<&'static str> {
    match path
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "svg" => Some("image/svg+xml"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// The images a book embeds, keyed by resolved path.
#[derive(Default)]
struct Images {
    /// resolved path → (EPUB href, media type, bytes)
    by_path: BTreeMap<PathBuf, (String, &'static str, Vec<u8>)>,
    missing: Vec<String>,
}

impl Images {
    /// Resolve and load one reference; its EPUB href, or `None` when it
    /// cannot be embedded (recorded as missing).
    fn add(&mut self, target: &str, source: &Path, root: &Path, label: &str) -> Option<String> {
        if target.contains("://") || target.starts_with("data:") {
            return None;
        }
        let resolved = resolve_image(target, source, root);
        let Some(path) = resolved.filter(|p| p.is_file()) else {
            self.missing.push(format!("{label}: {target}"));
            return None;
        };
        if let Some((href, _, _)) = self.by_path.get(&path) {
            return Some(href.clone());
        }
        let Some(media) = media_type(&path) else {
            self.missing
                .push(format!("{label}: {target} (not an image)"));
            return None;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            self.missing.push(format!("{label}: {target} (unreadable)"));
            return None;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image".into());
        let href = format!("images/{:03}-{}", self.by_path.len() + 1, sanitize(&name));
        self.by_path.insert(path, (href.clone(), media, bytes));
        Some(href)
    }

    fn data_uri(&self, href: &str) -> Option<String> {
        use base64::Engine as _;
        self.by_path
            .values()
            .find(|(h, _, _)| h == href)
            .map(|(_, media, bytes)| {
                format!(
                    "data:{media};base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                )
            })
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// One heading for the table of contents.
#[derive(Debug, Clone)]
struct Heading {
    level: u8,
    text: String,
    id: String,
}

/// One source rendered to HTML.
struct Rendered {
    body: String,
    headings: Vec<Heading>,
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Render one source; image references become EPUB hrefs (or data URIs for
/// the HTML book via `inline`).
fn render(
    text: &str,
    source: &Path,
    root: &Path,
    images: &mut Images,
    ids: &mut BTreeMap<String, usize>,
    prefix: &str,
) -> Rendered {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH;
    let label = source
        .strip_prefix(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| source.display().to_string());
    let prepared = preprocess(text);
    let mut events: Vec<Event<'_>> = Parser::new_ext(&prepared, options).collect();

    // Give every heading a stable, book-unique id and collect it.
    let mut headings = Vec::new();
    let mut i = 0;
    while i < events.len() {
        if let Event::Start(Tag::Heading { level, .. }) = &events[i] {
            let level = *level;
            let mut text = String::new();
            let mut j = i + 1;
            while j < events.len() && !matches!(events[j], Event::End(TagEnd::Heading(_))) {
                match &events[j] {
                    Event::Text(t) | Event::Code(t) | Event::InlineMath(t) => text.push_str(t),
                    _ => {}
                }
                j += 1;
            }
            let base = format!("{prefix}{}", slugify(&text));
            let n = ids.entry(base.clone()).or_insert(0);
            *n += 1;
            let id = if *n == 1 { base } else { format!("{base}-{n}") };
            events[i] = Event::Start(Tag::Heading {
                level,
                id: Some(CowStr::from(id.clone())),
                classes: Vec::new(),
                attrs: Vec::new(),
            });
            headings.push(Heading {
                level: heading_level(level),
                text: text.trim().to_string(),
                id,
            });
            i = j;
        }
        i += 1;
    }

    // Point images at their embedded copies.
    for event in events.iter_mut() {
        if let Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) = event
        {
            if let Some(href) = images.add(dest_url, source, root, &label) {
                *event = Event::Start(Tag::Image {
                    link_type: *link_type,
                    dest_url: CowStr::from(format!("../{href}")),
                    title: title.clone(),
                    id: id.clone(),
                });
            }
        }
    }

    let mut body = String::new();
    pulldown_cmark::html::push_html(&mut body, events.into_iter());
    Rendered { body, headings }
}

const STYLE: &str = r#"body { font-family: Georgia, "Times New Roman", serif; line-height: 1.5; margin: 0 5%; }
p, li { text-align: justify; hyphens: auto; }
h1, h2, h3 { color: ACCENT; font-family: "Helvetica Neue", Arial, sans-serif; line-height: 1.2; }
h1 { font-size: 1.9em; margin-top: 1.2em; }
h2 { font-size: 1.4em; }
img { max-width: 100%; height: auto; }
figure { margin: 1em 0; }
table { border-collapse: collapse; margin: 1em 0; max-width: 100%; }
th, td { border: 1px solid #b8c4d6; padding: 0.3em 0.6em; text-align: left; vertical-align: top; }
th { background: #eef3f9; }
pre { background: #f5f7fa; padding: 0.8em; overflow-x: auto; white-space: pre-wrap; font-size: 0.85em; }
code { font-family: "DejaVu Sans Mono", Menlo, Consolas, monospace; }
blockquote { border-left: 4px solid ACCENT; margin-left: 0; padding-left: 1em; color: #333; }
.math { font-family: "Latin Modern Math", "STIX Two Math", serif; }
.math-display { display: block; text-align: center; margin: 0.8em 0; }
.page-break { page-break-after: always; break-after: page; }
section.source { page-break-before: always; break-before: page; }
nav#toc ol { list-style: none; padding-left: 1em; }
nav#toc a { color: ACCENT; text-decoration: none; }
.cover { text-align: center; page-break-after: always; break-after: page; }
.cover img { max-height: 95vh; }
"#;

fn style() -> String {
    STYLE.replace("ACCENT", ACCENT)
}

fn xhtml(title: &str, body: &str, kind: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!DOCTYPE html>\n<html xmlns=\"http://www.w3.org/1999/xhtml\" \
         xmlns:epub=\"http://www.idpf.org/2007/ops\" xml:lang=\"en\" lang=\"en\">\n<head>\n<meta charset=\"utf-8\"/>\n\
         <title>{}</title>\n<link rel=\"stylesheet\" type=\"text/css\" href=\"../style.css\"/>\n</head>\n\
         <body>\n<section epub:type=\"{kind}\">\n{body}</section>\n</body>\n</html>\n",
        escape(title)
    )
}

/// The table of contents as nested `<ol>` items over `(href, headings)`.
fn toc_items(entries: &[(String, Vec<Heading>)]) -> String {
    let mut out = String::from("<ol>\n");
    for (href, headings) in entries {
        let mut iter = headings.iter().filter(|h| h.level <= 2).peekable();
        while let Some(h) = iter.next() {
            if h.level == 1 || out.ends_with("<ol>\n") {
                let _ = write!(
                    out,
                    "<li><a href=\"{href}#{}\">{}</a>",
                    h.id,
                    escape(&h.text)
                );
                let mut subs = String::new();
                while let Some(sub) = iter.peek().filter(|s| s.level == 2) {
                    let _ = writeln!(
                        subs,
                        "<li><a href=\"{href}#{}\">{}</a></li>",
                        sub.id,
                        escape(&sub.text)
                    );
                    iter.next();
                }
                if !subs.is_empty() {
                    let _ = write!(out, "\n<ol>\n{subs}</ol>\n");
                }
                out.push_str("</li>\n");
            } else {
                let _ = writeln!(
                    out,
                    "<li><a href=\"{href}#{}\">{}</a></li>",
                    h.id,
                    escape(&h.text)
                );
            }
        }
    }
    out.push_str("</ol>\n");
    out
}

/// The user-supplied cover, if any.
fn cover_path(root: &Path) -> Option<PathBuf> {
    ["cover.png", "cover.jpg", "cover.jpeg"]
        .iter()
        .map(|n| root.join("assets").join(n))
        .find(|p| p.is_file())
}

/// Run `command` for at most `timeout`, returning `(success, output)`.
fn run_bounded(mut command: Command, timeout: Duration) -> Result<(bool, String), String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(e.to_string()),
        }
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok((output.status.success(), text))
}

fn on_path(program: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(program))
        .find(|p| p.is_file())
}

/// Pandoc and the PDF engine to use, or why there is none.
fn pdf_tool(tool: &PdfTool) -> Result<(PathBuf, Option<String>), String> {
    match tool {
        PdfTool::Disabled => Err("PDF output is disabled for this build".into()),
        PdfTool::Pandoc { program, engine } => Ok((program.clone(), engine.clone())),
        PdfTool::Auto => {
            let pandoc = on_path("pandoc").ok_or(
                "pandoc is not on PATH, so no PDF was built (the EPUB, HTML and Markdown books \
                 were). Install pandoc and a PDF engine (e.g. xelatex), then call \
                 create_build_book() again or run build_book.sh.",
            )?;
            let engine = ["xelatex", "pdflatex", "typst", "wkhtmltopdf", "weasyprint"]
                .iter()
                .find(|e| on_path(e).is_some())
                .ok_or(
                    "pandoc is on PATH but no PDF engine is (xelatex, pdflatex, typst, \
                     wkhtmltopdf or weasyprint), so no PDF was built.",
                )?;
            Ok((pandoc, Some((*engine).to_string())))
        }
    }
}

/// The resource path Pandoc resolves images against.
fn resource_path(root: &Path) -> String {
    ["", "front-matter", "chapters", "back-matter", "assets"]
        .iter()
        .map(|d| {
            if d.is_empty() {
                root.display().to_string()
            } else {
                root.join(d).display().to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(if cfg!(windows) { ";" } else { ":" })
}

fn yaml_quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Build `dist/<slug>.pdf` from the combined Markdown.
fn build_pdf(root: &Path, markdown: &Path, pdf: &Path, tool: &PdfTool) -> Result<PathBuf, String> {
    let (pandoc, engine) = pdf_tool(tool)?;
    let _ = std::fs::remove_file(pdf);
    let mut command = Command::new(&pandoc);
    command
        .current_dir(root)
        .arg(markdown)
        .args(["--from", "markdown", "--toc", "--toc-depth=2"])
        .arg(format!("--resource-path={}", resource_path(root)))
        .args([
            "-V",
            "geometry:paperwidth=8in,paperheight=11.5in,margin=0.75in",
            "-V",
            "colorlinks=true",
        ])
        .arg("-o")
        .arg(pdf);
    if let Some(engine) = &engine {
        command.arg(format!("--pdf-engine={engine}"));
    }
    let (ok, output) =
        run_bounded(command, PANDOC_TIMEOUT).map_err(|e| format!("pandoc could not run: {e}"))?;
    let header = std::fs::read(pdf)
        .ok()
        .map(|b| b.get(..5).map(<[u8]>::to_vec).unwrap_or_default());
    if ok && header.as_deref() == Some(b"%PDF-") {
        return Ok(pdf.to_path_buf());
    }
    let tail: String = output
        .lines()
        .rev()
        .take(15)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    Err(format!(
        "pandoc ran (engine: {}) but produced no valid PDF:\n{tail}",
        engine.as_deref().unwrap_or("default")
    ))
}

fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// `build_book.sh`: rebuild the PDF and EPUB with Pandoc, without xaft.
fn script(meta: &BookMeta, slug: &str) -> String {
    format!(
        "#!/bin/sh\n\
         # Rebuild this book with pandoc, outside an xaft session. Generated by\n\
         # make_book's create_build_book; inside a session, create_build_book\n\
         # rebuilds the EPUB, HTML and Markdown books itself.\n\
         set -eu\n\
         cd \"$(dirname \"$0\")\"\n\
         mkdir -p dist\n\
         set --\n\
         for f in front-matter/*.md chapters/*.md back-matter/*.md; do\n\
         \x20 [ -f \"$f\" ] || continue\n\
         \x20 case \"$(basename \"$f\")\" in [Cc][Oo][Nn][Tt][Ee][Nn][Tt][Ss].md) continue ;; esac\n\
         \x20 set -- \"$@\" \"$f\"\n\
         done\n\
         ENGINE=\"${{PDF_ENGINE:-xelatex}}\"\n\
         pandoc \"$@\" --toc --toc-depth=2 --metadata title={title} --metadata author={author} \\\n\
         \x20 --resource-path=.:front-matter:chapters:back-matter:assets \\\n\
         \x20 -V geometry:paperwidth=8in,paperheight=11.5in,margin=0.75in -V colorlinks=true \\\n\
         \x20 --pdf-engine=\"$ENGINE\" -o dist/{slug}.pdf\n\
         pandoc \"$@\" --toc --toc-depth=2 --metadata title={title} --metadata author={author} \\\n\
         \x20 --resource-path=.:front-matter:chapters:back-matter:assets -o dist/{slug}-pandoc.epub\n\
         echo \"Built dist/{slug}.pdf and dist/{slug}-pandoc.epub\"\n",
        title = sh_quote(&meta.title),
        author = sh_quote(&meta.author),
    )
}

/// Compile the book under `root`.
pub(crate) fn build_book(root: &Path, meta: &BookMeta, pdf: &PdfTool) -> Result<BuiltBook, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("the book directory {} is unusable: {e}", root.display()))?;
    let sources = source_files(&root);
    if sources.is_empty() {
        return Err(
            "no markdown sources found in front-matter/, chapters/, or back-matter/".into(),
        );
    }
    let slug = slugify(&meta.title);
    let dist = root.join("dist");
    std::fs::create_dir_all(&dist).map_err(|e| e.to_string())?;

    let mut images = Images::default();
    let mut ids: BTreeMap<String, usize> = BTreeMap::new();
    let mut rendered: Vec<(PathBuf, String, Rendered)> = Vec::new();
    for (n, source) in sources.iter().enumerate() {
        let text = std::fs::read(source)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .map_err(|e| format!("cannot read {}: {e}", source.display()))?;
        let href = format!("text/s{:03}.xhtml", n + 1);
        let r = render(&text, source, &root, &mut images, &mut ids, "");
        rendered.push((source.clone(), href, r));
    }
    let cover = cover_path(&root);
    let cover_href = cover.as_ref().and_then(|c| {
        let ext = c.extension()?.to_string_lossy().to_ascii_lowercase();
        let media = media_type(c)?;
        let bytes = std::fs::read(c).ok()?;
        let href = format!("images/cover.{ext}");
        images
            .by_path
            .insert(normalise(c), (href.clone(), media, bytes));
        Some(href)
    });
    let titles: Vec<String> = rendered
        .iter()
        .flat_map(|(_, _, r)| {
            r.headings
                .iter()
                .filter(|h| h.level == 1)
                .map(|h| h.text.clone())
        })
        .collect();
    let kind_of = |source: &Path| {
        let dir = source
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match dir.as_str() {
            "front-matter" => "frontmatter",
            "back-matter" => "backmatter",
            _ => "chapter",
        }
    };

    // ── EPUB ──
    let mut zip = ZipWriter::default();
    zip.add("mimetype", b"application/epub+zip")?;
    zip.add(
        "META-INF/container.xml",
        b"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<container version=\"1.0\" \
          xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\">\n<rootfiles>\n\
          <rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/>\n\
          </rootfiles>\n</container>\n",
    )?;
    zip.add("OEBPS/style.css", style().as_bytes())?;
    let toc_entries: Vec<(String, Vec<Heading>)> = rendered
        .iter()
        .map(|(_, href, r)| (href.clone(), r.headings.clone()))
        .collect();
    let nav_body = format!(
        "<nav epub:type=\"toc\" id=\"toc\">\n<h1>Contents</h1>\n{}</nav>\n",
        toc_items(&toc_entries)
    );
    let nav = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!DOCTYPE html>\n<html xmlns=\"http://www.w3.org/1999/xhtml\" \
         xmlns:epub=\"http://www.idpf.org/2007/ops\" xml:lang=\"en\" lang=\"en\">\n<head>\n<meta charset=\"utf-8\"/>\n\
         <title>Contents</title>\n<link rel=\"stylesheet\" type=\"text/css\" href=\"style.css\"/>\n</head>\n\
         <body>\n{nav_body}</body>\n</html>\n"
    );
    zip.add("OEBPS/nav.xhtml", nav.as_bytes())?;
    if let Some(href) = &cover_href {
        let page = xhtml(
            &meta.title,
            &format!(
                "<div class=\"cover\"><img src=\"../{href}\" alt=\"{}\"/></div>\n",
                escape(&meta.title)
            ),
            "cover",
        );
        zip.add("OEBPS/text/cover.xhtml", page.as_bytes())?;
    }
    for (source, href, r) in &rendered {
        let title = r
            .headings
            .first()
            .map(|h| h.text.clone())
            .unwrap_or_else(|| meta.title.clone());
        zip.add(
            &format!("OEBPS/{href}"),
            xhtml(&title, &r.body, kind_of(source)).as_bytes(),
        )?;
    }
    for (href, _, bytes) in images.by_path.values() {
        zip.add(&format!("OEBPS/{href}"), bytes)?;
    }
    let identifier = format!(
        "urn:xaft:book:{}",
        &sha256_hex(format!("{}\u{0}{}", meta.title, meta.author).as_bytes())[..32]
    );
    let modified = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let mut manifest = String::from(
        "<item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n\
         <item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\"/>\n\
         <item id=\"css\" href=\"style.css\" media-type=\"text/css\"/>\n",
    );
    let mut spine = String::new();
    if cover_href.is_some() {
        manifest.push_str(
            "<item id=\"cover\" href=\"text/cover.xhtml\" media-type=\"application/xhtml+xml\"/>\n",
        );
        spine.push_str("<itemref idref=\"cover\"/>\n");
    }
    spine.push_str("<itemref idref=\"nav\"/>\n");
    for (n, (_, href, _)) in rendered.iter().enumerate() {
        let _ = writeln!(
            manifest,
            "<item id=\"s{:03}\" href=\"{href}\" media-type=\"application/xhtml+xml\"/>",
            n + 1
        );
        let _ = writeln!(spine, "<itemref idref=\"s{:03}\"/>", n + 1);
    }
    for (n, (href, media, _)) in images.by_path.values().enumerate() {
        let is_cover = Some(href) == cover_href.as_ref();
        let _ = writeln!(
            manifest,
            "<item id=\"{}\" href=\"{href}\" media-type=\"{media}\"{}/>",
            if is_cover {
                "cover-image".to_string()
            } else {
                format!("img{:03}", n + 1)
            },
            if is_cover {
                " properties=\"cover-image\""
            } else {
                ""
            }
        );
    }
    let opf = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" \
         unique-identifier=\"bookid\" xml:lang=\"en\">\n<metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n\
         <dc:identifier id=\"bookid\">{identifier}</dc:identifier>\n<dc:title>{}</dc:title>\n\
         <dc:creator>{}</dc:creator>\n<dc:language>en</dc:language>\n\
         <meta property=\"dcterms:modified\">{modified}</meta>\n{}</metadata>\n\
         <manifest>\n{manifest}</manifest>\n<spine toc=\"ncx\">\n{spine}</spine>\n</package>\n",
        escape(&meta.title),
        escape(&meta.author),
        if cover_href.is_some() {
            "<meta name=\"cover\" content=\"cover-image\"/>\n"
        } else {
            ""
        }
    );
    zip.add("OEBPS/content.opf", opf.as_bytes())?;
    let mut nav_points = String::new();
    let mut order = 0usize;
    for (href, headings) in &toc_entries {
        for h in headings.iter().filter(|h| h.level == 1) {
            order += 1;
            let _ = writeln!(
                nav_points,
                "<navPoint id=\"np{order}\" playOrder=\"{order}\"><navLabel><text>{}</text></navLabel>\
                 <content src=\"{href}#{}\"/></navPoint>",
                escape(&h.text),
                h.id
            );
        }
    }
    let ncx = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\" version=\"2005-1\">\n\
         <head><meta name=\"dtb:uid\" content=\"{identifier}\"/></head>\n\
         <docTitle><text>{}</text></docTitle>\n<navMap>\n{nav_points}</navMap>\n</ncx>\n",
        escape(&meta.title)
    );
    zip.add("OEBPS/toc.ncx", ncx.as_bytes())?;
    let epub = dist.join(format!("{slug}.epub"));
    std::fs::write(&epub, zip.finish()).map_err(|e| format!("cannot write the EPUB: {e}"))?;

    // ── HTML ──
    let mut html = format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\"/>\n<title>{}</title>\n\
         <meta name=\"author\" content=\"{}\"/>\n<style>\n{}</style>\n</head>\n<body>\n",
        escape(&meta.title),
        escape(&meta.author),
        style()
    );
    if let Some(uri) = cover_href.as_ref().and_then(|h| images.data_uri(h)) {
        let _ = writeln!(
            html,
            "<div class=\"cover\"><img src=\"{uri}\" alt=\"{}\"/></div>",
            escape(&meta.title)
        );
    } else {
        let _ = writeln!(
            html,
            "<header class=\"cover\"><h1>{}</h1><p>{}</p></header>",
            escape(&meta.title),
            escape(&meta.author)
        );
    }
    let html_toc: Vec<(String, Vec<Heading>)> = toc_entries
        .iter()
        .map(|(_, h)| (String::new(), h.clone()))
        .collect();
    let _ = writeln!(
        html,
        "<nav id=\"toc\" class=\"page-break\">\n<h1>Contents</h1>\n{}</nav>",
        toc_items(&html_toc)
    );
    for (_, _, r) in &rendered {
        let mut body = r.body.clone();
        for (href, _, _) in images.by_path.values() {
            if let Some(uri) = images.data_uri(href) {
                body = body.replace(&format!("src=\"../{href}\""), &format!("src=\"{uri}\""));
            }
        }
        let _ = writeln!(html, "<section class=\"source\">\n{body}</section>");
    }
    html.push_str("</body>\n</html>\n");
    let html_path = dist.join(format!("{slug}.html"));
    std::fs::write(&html_path, html).map_err(|e| format!("cannot write the HTML book: {e}"))?;

    // ── Markdown ──
    let mut combined = format!(
        "---\ntitle: {}\nauthor: {}\n---\n\n",
        yaml_quote(&meta.title),
        yaml_quote(&meta.author)
    );
    for source in &sources {
        let text = std::fs::read(source)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        for line in text.lines() {
            if line.trim() == "#pagebreak()" {
                continue;
            }
            combined.push_str(line);
            combined.push('\n');
        }
        combined.push('\n');
    }
    let markdown = dist.join(format!("{slug}.md"));
    std::fs::write(&markdown, combined)
        .map_err(|e| format!("cannot write the Markdown book: {e}"))?;

    // ── PDF ──
    let pdf_path = dist.join(format!("{slug}.pdf"));
    let pdf = build_pdf(&root, &markdown, &pdf_path, pdf);
    if pdf.is_err() {
        let _ = std::fs::remove_file(&pdf_path);
    }

    // ── Script and record ──
    let script_path = root.join("build_book.sh");
    std::fs::write(&script_path, script(meta, &slug)).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755));
    }
    let rel = |p: &Path| {
        p.strip_prefix(&root)
            .map(|r| r.display().to_string())
            .unwrap_or_else(|_| p.display().to_string())
    };
    let record: Value = json!({
        "title": meta.title,
        "author": meta.author,
        "slug": slug,
        "built_at": chrono::Utc::now().to_rfc3339(),
        "sources": sources_fingerprint(&root)
            .into_iter()
            .map(|(path, sha256)| json!({"path": path, "sha256": sha256}))
            .collect::<Vec<_>>(),
        "titles": titles,
        "epub": rel(&epub),
        "html": rel(&html_path),
        "markdown": rel(&markdown),
        "pdf": pdf.as_ref().ok().map(|p| rel(p)),
        "pdf_note": pdf.as_ref().err(),
        "images": images.by_path.len() - usize::from(cover_href.is_some()),
        "missing_images": images.missing,
        "cover": cover_href.is_some(),
    });
    std::fs::write(
        dist.join(RECORD_NAME),
        serde_json::to_vec_pretty(&record).unwrap_or_default(),
    )
    .map_err(|e| format!("cannot write the build record: {e}"))?;

    Ok(BuiltBook {
        slug,
        sources: sources.iter().map(|s| rel(s)).collect(),
        titles,
        epub,
        html: html_path,
        markdown,
        pdf,
        images: images.by_path.len() - usize::from(cover_href.is_some()),
        missing_images: images.missing,
        cover: cover_href.is_some(),
        script: script_path,
    })
}

#[cfg(test)]
mod tests {
    use super::super::layout::tests::png;
    use super::super::zip::read_stored;
    use super::*;

    pub(crate) fn sample_book(root: &Path) {
        for dir in ["front-matter", "chapters", "back-matter", "assets"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("assets/fig-01.png"), png(600, 400)).unwrap();
        std::fs::write(
            root.join("front-matter/00-preface.md"),
            "# Preface\n\n\\newpage\n\nWhy this book exists.\n",
        )
        .unwrap();
        std::fs::write(
            root.join("chapters/01-kalman-basics.md"),
            "# Kalman Basics\n\n\\newpage\n\n## The **State** Vector\n\nThe update is $x = Fx$.\n\n\
             ![The loop](../assets/fig-01.png){width=4in}\n\n```rust\nfn main() {}\n```\n\n\
             | a | b |\n|---|---|\n| 1 | 2 |\n\n#pagebreak()\n\nMore [RFC 8446].\n",
        )
        .unwrap();
        std::fs::write(
            root.join("chapters/02-tuning.md"),
            "# Tuning\n\n## Noise\n\n![gone](assets/nope.png)\n",
        )
        .unwrap();
        std::fs::write(root.join("chapters/contents.md"), "# Contents\n").unwrap();
        std::fs::write(
            root.join("back-matter/99-index.md"),
            "# Index\n\n- **State**: 1\n",
        )
        .unwrap();
    }

    fn meta() -> BookMeta {
        BookMeta {
            title: "Kalman Filtering: A Field Guide".into(),
            author: "Ada O'Neil".into(),
        }
    }

    #[test]
    fn slugs_match_agenthicc() {
        assert_eq!(
            slugify("Kalman Filtering: A Field Guide"),
            "kalman-filtering-a-field-guide"
        );
        assert_eq!(slugify("!!!"), "book");
    }

    #[test]
    fn sources_are_ordered_and_contents_is_excluded() {
        let dir = tempfile::tempdir().unwrap();
        sample_book(dir.path());
        let names: Vec<String> = source_files(dir.path())
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [
                "00-preface.md",
                "01-kalman-basics.md",
                "02-tuning.md",
                "99-index.md"
            ]
        );
    }

    #[test]
    fn page_break_markers_follow_the_book_rules() {
        let out =
            preprocess("# T\n\n\\newpage\n\ntext\n\\clearpage\nmore ![x](a.png){width=2in}\n");
        assert_eq!(out.matches("page-break").count(), 1, "{out}");
        assert!(
            !out.contains("\\newpage") && !out.contains("{width"),
            "{out}"
        );
    }

    #[test]
    fn a_book_compiles_to_epub_html_and_markdown_without_pandoc() {
        let dir = tempfile::tempdir().unwrap();
        sample_book(dir.path());
        let built = build_book(dir.path(), &meta(), &PdfTool::Disabled).unwrap();
        assert_eq!(built.slug, "kalman-filtering-a-field-guide");
        assert_eq!(
            built.titles,
            ["Preface", "Kalman Basics", "Tuning", "Index"]
        );
        assert_eq!(built.images, 1);
        assert_eq!(
            built.missing_images,
            ["chapters/02-tuning.md: assets/nope.png"]
        );
        assert!(built.pdf.is_err());

        let epub = read_stored(&std::fs::read(&built.epub).unwrap()).unwrap();
        assert_eq!(
            epub[0],
            ("mimetype".to_string(), b"application/epub+zip".to_vec())
        );
        let names: Vec<&str> = epub.iter().map(|(n, _)| n.as_str()).collect();
        for needed in [
            "META-INF/container.xml",
            "OEBPS/content.opf",
            "OEBPS/nav.xhtml",
            "OEBPS/toc.ncx",
            "OEBPS/style.css",
            "OEBPS/text/s001.xhtml",
            "OEBPS/text/s004.xhtml",
        ] {
            assert!(names.contains(&needed), "{needed} missing from {names:?}");
        }
        assert!(
            names
                .iter()
                .any(|n| n.starts_with("OEBPS/images/") && n.ends_with("fig-01.png"))
        );
        let text = |name: &str| {
            String::from_utf8(epub.iter().find(|(n, _)| n == name).unwrap().1.clone()).unwrap()
        };
        let opf = text("OEBPS/content.opf");
        assert!(opf.contains("<dc:title>Kalman Filtering: A Field Guide</dc:title>"));
        assert!(opf.contains("<dc:creator>Ada O&#39;Neil</dc:creator>"));
        assert!(opf.contains("properties=\"nav\""));
        let nav = text("OEBPS/nav.xhtml");
        assert!(
            nav.contains("text/s002.xhtml#kalman-basics\">Kalman Basics</a>"),
            "{nav}"
        );
        assert!(
            nav.contains("#the-state-vector\">The State Vector</a>"),
            "{nav}"
        );
        let chapter = text("OEBPS/text/s002.xhtml");
        assert!(
            chapter.contains("<h1 id=\"kalman-basics\">Kalman Basics</h1>"),
            "{chapter}"
        );
        assert!(chapter.contains("class=\"math math-inline\""), "{chapter}");
        assert!(chapter.contains("class=\"language-rust\""), "{chapter}");
        assert!(chapter.contains("<table>"), "{chapter}");
        assert!(chapter.contains("src=\"../images/"), "{chapter}");
        assert!(
            chapter.contains("page-break"),
            "a mid-chapter break survives"
        );
        assert!(!chapter.contains("\\newpage"), "{chapter}");

        let html = std::fs::read_to_string(&built.html).unwrap();
        assert!(
            html.contains("src=\"data:image/png;base64,"),
            "images are embedded"
        );
        assert!(html.contains("<nav id=\"toc\""));
        let md = std::fs::read_to_string(&built.markdown).unwrap();
        assert!(md.starts_with("---\ntitle: \"Kalman Filtering: A Field Guide\""));
        assert!(
            md.contains("\\newpage"),
            "pandoc keeps the LaTeX page breaks"
        );
        assert!(!md.contains("#pagebreak()"));

        let record: Value = serde_json::from_slice(
            &std::fs::read(dir.path().join("dist").join(RECORD_NAME)).unwrap(),
        )
        .unwrap();
        assert_eq!(record["pdf"], Value::Null);
        assert_eq!(record["sources"].as_array().unwrap().len(), 4);
        let script = std::fs::read_to_string(&built.script).unwrap();
        assert!(script.contains("--metadata title='Kalman Filtering: A Field Guide'"));
        assert!(script.contains("author='Ada O'\\''Neil'"), "{script}");
    }

    #[cfg(unix)]
    #[test]
    fn a_pdf_is_built_through_pandoc_when_one_is_available() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        sample_book(dir.path());
        // A stand-in pandoc that writes a PDF header to its -o target.
        let fake = dir.path().join("fake-pandoc");
        std::fs::write(
            &fake,
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do if [ \"$1\" = -o ]; then shift; printf '%%PDF-1.7\\n' > \"$1\"; fi; shift; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let built = build_book(
            dir.path(),
            &meta(),
            &PdfTool::Pandoc {
                program: fake.clone(),
                engine: Some("xelatex".into()),
            },
        )
        .unwrap();
        let pdf = built.pdf.unwrap();
        assert_eq!(
            pdf.file_name().unwrap(),
            "kalman-filtering-a-field-guide.pdf"
        );

        // A pandoc that fails is reported, and leaves no PDF behind.
        std::fs::write(&fake, "#!/bin/sh\necho 'xelatex not found' >&2\nexit 43\n").unwrap();
        let built = build_book(
            dir.path(),
            &meta(),
            &PdfTool::Pandoc {
                program: fake,
                engine: None,
            },
        )
        .unwrap();
        let err = built.pdf.unwrap_err();
        assert!(err.contains("xelatex not found"), "{err}");
        assert!(
            !dir.path()
                .join("dist/kalman-filtering-a-field-guide.pdf")
                .exists()
        );
    }

    #[test]
    fn the_user_cover_becomes_the_epub_cover() {
        let dir = tempfile::tempdir().unwrap();
        sample_book(dir.path());
        std::fs::write(dir.path().join("assets/cover.png"), png(1600, 2400)).unwrap();
        let built = build_book(dir.path(), &meta(), &PdfTool::Disabled).unwrap();
        assert!(built.cover);
        let epub = read_stored(&std::fs::read(&built.epub).unwrap()).unwrap();
        let opf = String::from_utf8(
            epub.iter()
                .find(|(n, _)| n == "OEBPS/content.opf")
                .unwrap()
                .1
                .clone(),
        )
        .unwrap();
        assert!(opf.contains("properties=\"cover-image\""), "{opf}");
        assert!(
            opf.contains("<itemref idref=\"cover\"/>\n<itemref idref=\"nav\"/>"),
            "{opf}"
        );
    }
}
