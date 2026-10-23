//! make_book's layout gate — agenthicc's `_layout_findings` and helpers.
//!
//! The book is laid out on an 8 × 11.5 in page with 0.75 in margins. Every
//! referenced image or diagram must fit within 95% of the 6.5 in content
//! width (6.175 in) and 70% of the page height (8.05 in); every Markdown table
//! is estimated against the same budget. Dimensions are read from the image
//! headers themselves (PNG, GIF, WebP, SVG, JPEG) — no image library — and from
//! explicit Markdown `{width=… height=…}` attributes.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

pub(crate) const PAGE_WIDTH_IN: f64 = 8.0;
pub(crate) const PAGE_HEIGHT_IN: f64 = 11.5;
pub(crate) const PAGE_MARGIN_IN: f64 = 0.75;
pub(crate) const CONTENT_WIDTH_IN: f64 = PAGE_WIDTH_IN - 2.0 * PAGE_MARGIN_IN;
/// 95% of the content width, rounded like agenthicc (6.175 in).
pub(crate) const TARGET_MEDIA_WIDTH_IN: f64 = 6.175;
/// 70% of the page height, rounded like agenthicc (8.05 in).
pub(crate) const MAX_MEDIA_HEIGHT_IN: f64 = 8.05;
const TABLE_ROW_HEIGHT_IN: f64 = 0.28;
const TABLE_CELL_CHAR_WIDTH_IN: f64 = 0.055;
const TABLE_CELL_PADDING_IN: f64 = 0.18;
const IMAGE_SUFFIXES: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "svg"];

/// What the layout gate found.
#[derive(Debug, Default)]
pub(crate) struct LayoutFindings {
    /// Book sources scanned, relative to the book root.
    pub files: Vec<String>,
    /// Images and diagrams referenced.
    pub image_count: usize,
    /// Markdown tables found.
    pub table_count: usize,
    /// Every violation.
    pub issues: Vec<String>,
}

fn image_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"!\[[^\]]*\]\((?P<target><[^>]+>|[^)\s]+)(?:\s+"[^"]*")?\)(?:\{(?P<attributes>[^}]*)\})?"#,
        )
        .expect("valid")
    })
}

/// `![alt](target)` references in Markdown, with their attribute blocks.
pub(crate) fn image_references(text: &str) -> Vec<(String, String)> {
    image_re()
        .captures_iter(text)
        .map(|c| {
            let target = c["target"]
                .trim_matches(|ch| ch == '<' || ch == '>')
                .split('#')
                .next()
                .unwrap_or_default()
                .to_string();
            let attributes = c
                .name("attributes")
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            (target, attributes)
        })
        .collect()
}

/// A Markdown image dimension in inches; bare numbers are CSS pixels and
/// percentages are of the content width (width) or the page (height).
pub(crate) fn parse_measurement(value: &str, axis_is_width: bool) -> Option<f64> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)^([0-9]+(?:\.[0-9]+)?)\s*(in|cm|mm|pt|px|%)?$").expect("valid")
    });
    let caps = re.captures(value.trim())?;
    let amount: f64 = caps[1].parse().ok()?;
    let unit = caps
        .get(2)
        .map(|m| m.as_str().to_ascii_lowercase())
        .unwrap_or_else(|| "px".into());
    Some(match unit.as_str() {
        "in" => amount,
        "cm" => amount / 2.54,
        "mm" => amount / 25.4,
        "pt" => amount / 72.0,
        "px" => amount / 96.0,
        _ => {
            let base = if axis_is_width {
                CONTENT_WIDTH_IN
            } else {
                PAGE_HEIGHT_IN
            };
            base * amount / 100.0
        }
    })
}

fn image_attributes(attributes: &str) -> (Option<String>, Option<String>) {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)\b(width|height)\s*=\s*([^\s}]+)").expect("valid"));
    let (mut width, mut height) = (None, None);
    for caps in re.captures_iter(attributes) {
        let value = caps[2].trim_matches(|c| c == '"' || c == '\'').to_string();
        if caps[1].eq_ignore_ascii_case("width") {
            width = Some(value);
        } else {
            height = Some(value);
        }
    }
    (width, height)
}

fn be16(b: &[u8], i: usize) -> Option<u32> {
    b.get(i..i + 2)
        .map(|s| u32::from(u16::from_be_bytes([s[0], s[1]])))
}

fn le16(b: &[u8], i: usize) -> Option<u32> {
    b.get(i..i + 2)
        .map(|s| u32::from(u16::from_le_bytes([s[0], s[1]])))
}

fn be32(b: &[u8], i: usize) -> Option<u32> {
    b.get(i..i + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn le24(b: &[u8], i: usize) -> Option<u32> {
    b.get(i..i + 3)
        .map(|s| u32::from(s[0]) | u32::from(s[1]) << 8 | u32::from(s[2]) << 16)
}

fn jpeg_dimensions(raw: &[u8]) -> Option<(u32, u32)> {
    if !raw.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut i = 2usize;
    while i + 9 < raw.len() {
        if raw[i] != 0xFF {
            i += 1;
            continue;
        }
        while i < raw.len() && raw[i] == 0xFF {
            i += 1;
        }
        if i >= raw.len() {
            break;
        }
        let marker = raw[i];
        i += 1;
        if marker == 0xD8 || marker == 0xD9 {
            continue;
        }
        let len = be16(raw, i)? as usize;
        if len < 2 || i + len > raw.len() {
            break;
        }
        let is_sof = matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF);
        if is_sof && len >= 7 {
            let height = be16(raw, i + 3)?;
            let width = be16(raw, i + 5)?;
            return Some((width, height));
        }
        i += len;
    }
    None
}

fn svg_number(text: &str, re: &Regex) -> Option<f64> {
    re.captures(text).and_then(|c| c[1].parse::<f64>().ok())
}

/// Width and height in pixels, read from the file's header.
pub(crate) fn image_dimensions(path: &Path) -> Option<(u32, u32)> {
    let raw = std::fs::read(path).ok()?;
    if raw.starts_with(b"\x89PNG\r\n\x1a\n") && raw.len() >= 24 {
        return Some((be32(&raw, 16)?, be32(&raw, 20)?));
    }
    if (raw.starts_with(b"GIF87a") || raw.starts_with(b"GIF89a")) && raw.len() >= 10 {
        return Some((le16(&raw, 6)?, le16(&raw, 8)?));
    }
    if raw.starts_with(b"RIFF") && raw.get(8..12) == Some(b"WEBP") {
        if raw.get(12..16) == Some(b"VP8X") && raw.len() >= 30 {
            return Some((1 + le24(&raw, 24)?, 1 + le24(&raw, 27)?));
        }
        // A lossy frame: 3-byte frame tag, the start code 9d 01 2a, then
        // 14-bit width and height.
        if raw.get(12..16) == Some(b"VP8 ")
            && raw.len() >= 30
            && raw.get(23..26) == Some(&[0x9d, 0x01, 0x2a][..])
        {
            return Some((le16(&raw, 26)? & 0x3FFF, le16(&raw, 28)? & 0x3FFF));
        }
    }
    let head = &raw[..raw.len().min(512)];
    if raw.starts_with(b"<")
        || String::from_utf8_lossy(head)
            .to_lowercase()
            .contains("<svg")
    {
        static VIEWBOX: OnceLock<Regex> = OnceLock::new();
        static WIDTH: OnceLock<Regex> = OnceLock::new();
        static HEIGHT: OnceLock<Regex> = OnceLock::new();
        let text = String::from_utf8_lossy(&raw[..raw.len().min(16_384)]).into_owned();
        let viewbox = VIEWBOX.get_or_init(|| {
            Regex::new(
                r#"\bviewBox\s*=\s*["']\s*[-+0-9.eE]+\s+[-+0-9.eE]+\s+([0-9.eE+-]+)\s+([0-9.eE+-]+)"#,
            )
            .expect("valid")
        });
        if let Some(c) = viewbox.captures(&text) {
            let w: f64 = c[1].parse().ok()?;
            let h: f64 = c[2].parse().ok()?;
            return Some((w.round().max(1.0) as u32, h.round().max(1.0) as u32));
        }
        let width = WIDTH
            .get_or_init(|| Regex::new(r#"\bwidth\s*=\s*["']\s*([0-9.eE+-]+)"#).expect("valid"));
        let height = HEIGHT
            .get_or_init(|| Regex::new(r#"\bheight\s*=\s*["']\s*([0-9.eE+-]+)"#).expect("valid"));
        if let (Some(w), Some(h)) = (svg_number(&text, width), svg_number(&text, height)) {
            return Some((w.round().max(1.0) as u32, h.round().max(1.0) as u32));
        }
    }
    jpeg_dimensions(&raw)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// agenthicc's `_layout_image_issues`.
fn image_issues(path: &Path, attributes: &str) -> Vec<String> {
    let name = file_name(path);
    let (w_attr, h_attr) = image_attributes(attributes);
    let mut width = w_attr.as_deref().and_then(|v| parse_measurement(v, true));
    let mut height = h_attr.as_deref().and_then(|v| parse_measurement(v, false));
    if (w_attr.is_some() && width.is_none()) || (h_attr.is_some() && height.is_none()) {
        return vec![format!(
            "{name} uses an unsupported width/height expression; use in, cm, mm, pt, px, or %."
        )];
    }
    let dimensions = image_dimensions(path);
    if dimensions.is_none() && (width.is_none() || height.is_none()) {
        return vec![format!(
            "{name} has no readable dimensions; add explicit width and height attributes."
        )];
    }
    if let Some((iw, ih)) = dimensions {
        let ratio = if iw > 0 {
            f64::from(ih) / f64::from(iw)
        } else {
            0.0
        };
        match (width, height) {
            (None, Some(h)) if ratio > 0.0 => width = Some(h / ratio),
            (Some(w), None) => height = Some(w * ratio),
            _ => {}
        }
        // agenthicc leaves an unsized raster unsized: the builder scales it
        // to the content width at typesetting time.
    }
    let mut issues = Vec::new();
    if let Some(w) = width {
        if w > TARGET_MEDIA_WIDTH_IN + 1e-6 {
            issues.push(format!(
                "{name} is {w:.2}in wide; keep it <= {TARGET_MEDIA_WIDTH_IN:.3}in (95% of \
                 {CONTENT_WIDTH_IN:.2}in content width)."
            ));
        }
    }
    if let Some(h) = height {
        if h > MAX_MEDIA_HEIGHT_IN + 1e-6 {
            issues.push(format!(
                "{name} is {h:.2}in tall; keep it <= {MAX_MEDIA_HEIGHT_IN:.1}in (70% of page height)."
            ));
        }
    }
    issues
}

fn split_table_row(line: &str) -> Vec<String> {
    let mut row = line.trim();
    if let Some(r) = row.strip_prefix('|') {
        row = r;
    }
    if row.ends_with('|') && !row.ends_with("\\|") {
        row = &row[..row.len() - 1];
    }
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut chars = row.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'|') {
            current.push('|');
            chars.next();
        } else if c == '|' {
            cells.push(current.trim().to_string());
            current.clear();
        } else {
            current.push(c);
        }
    }
    cells.push(current.trim().to_string());
    cells
}

fn is_table_separator(line: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^:?-{3,}:?$").expect("valid"));
    let cells = split_table_row(line);
    cells.len() >= 2 && cells.iter().all(|c| re.is_match(c))
}

/// agenthicc's `_table_layout_issues`: estimated table sizes for one file.
pub(crate) fn table_issues(name: &str, text: &str) -> (usize, Vec<String>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut in_fence = false;
    let mut count = 0usize;
    let mut issues = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            i += 1;
            continue;
        }
        if in_fence || i == 0 || !is_table_separator(line) {
            i += 1;
            continue;
        }
        let header = lines[i - 1];
        if !header.contains('|') {
            i += 1;
            continue;
        }
        let mut rows = vec![split_table_row(header)];
        let mut cursor = i + 1;
        while cursor < lines.len()
            && !lines[cursor].trim().is_empty()
            && lines[cursor].contains('|')
        {
            rows.push(split_table_row(lines[cursor]));
            cursor += 1;
        }
        count += 1;
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut widths = vec![0usize; columns];
        for row in &rows {
            for (column, cell) in row.iter().enumerate() {
                widths[column] = widths[column].max(cell.chars().count());
            }
        }
        let est_width = widths.iter().sum::<usize>() as f64 * TABLE_CELL_CHAR_WIDTH_IN
            + columns as f64 * TABLE_CELL_PADDING_IN;
        let est_height = rows.len() as f64 * TABLE_ROW_HEIGHT_IN;
        let label = format!("{name} table {count}");
        if est_width > TARGET_MEDIA_WIDTH_IN {
            issues.push(format!(
                "{label} is approximately {est_width:.2}in wide; keep it <= {TARGET_MEDIA_WIDTH_IN:.1}in."
            ));
        }
        if est_height > MAX_MEDIA_HEIGHT_IN {
            issues.push(format!(
                "{label} is approximately {est_height:.2}in tall; keep it <= \
                 {MAX_MEDIA_HEIGHT_IN:.1}in (70% of page height)."
            ));
        }
        i = cursor;
    }
    (count, issues)
}

/// Every regular file with one of `suffixes` below `dir`, sorted; files that
/// resolve outside `dir` (symlinks) are not part of the inventory.
pub(crate) fn files_under(dir: &Path, suffixes: &[&str]) -> Vec<PathBuf> {
    let Ok(root) = dir.canonicalize() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(path);
            } else if path.is_file()
                && path
                    .extension()
                    .map(|e| e.to_string_lossy().to_ascii_lowercase())
                    .is_some_and(|e| suffixes.contains(&e.as_str()))
            {
                if let Ok(resolved) = path.canonicalize() {
                    if resolved.starts_with(&root) {
                        out.push(resolved);
                    }
                }
            }
        }
    }
    out.sort();
    out
}

/// Resolve a Markdown image target against its source file's directory, then
/// the book root; `None` when it lies outside the book root.
pub(crate) fn resolve_image(target: &str, source: &Path, root: &Path) -> Option<PathBuf> {
    let raw = Path::new(target);
    let candidates: Vec<PathBuf> = if raw.is_absolute() {
        vec![raw.to_path_buf()]
    } else {
        vec![source.parent().unwrap_or(root).join(raw), root.join(raw)]
    };
    let inside: Vec<PathBuf> = candidates
        .into_iter()
        .map(|c| normalise(&c))
        .filter(|c| c.starts_with(root))
        .collect();
    inside
        .iter()
        .find(|c| c.is_file())
        .cloned()
        .or_else(|| inside.into_iter().next())
}

/// Lexically normalise `..` and `.` (the file may not exist).
pub(crate) fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The book's Markdown sources: chapters, then front and back matter.
pub(crate) fn book_sources(root: &Path) -> Vec<PathBuf> {
    let mut sources = files_under(&root.join("chapters"), &["md"]);
    for dir in ["front-matter", "back-matter"] {
        sources.extend(files_under(&root.join(dir), &["md"]));
    }
    sources.sort();
    sources
}

/// agenthicc's `_layout_findings` over every book source.
pub(crate) fn layout_findings(root: &Path, assets: &Path) -> LayoutFindings {
    let mut findings = LayoutFindings::default();
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let assets = assets
        .canonicalize()
        .unwrap_or_else(|_| assets.to_path_buf());
    let sources = book_sources(&root);
    if sources.is_empty() {
        findings.issues.push(
            "no Markdown book sources exist under chapters/ or front/back matter directories"
                .into(),
        );
    }
    for source in &sources {
        let name = file_name(source);
        findings.files.push(
            source
                .strip_prefix(&root)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| source.display().to_string()),
        );
        if name.eq_ignore_ascii_case("contents.md") {
            findings.issues.push(format!(
                "{name} is an agent-authored TOC source; create_build_book generates the TOC."
            ));
        }
        let text = match std::fs::read(source) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                findings
                    .issues
                    .push(format!("{name} could not be read: {e}"));
                continue;
            }
        };
        for (target, attributes) in image_references(&text) {
            if target.contains("://") {
                findings.issues.push(format!(
                    "{name} references a remote image ({target}); save it under the assets directory."
                ));
                continue;
            }
            let Some(candidate) = resolve_image(&target, source, &root) else {
                findings.issues.push(format!(
                    "{name} references an image outside the configured assets directory: {target}"
                ));
                continue;
            };
            if !candidate.starts_with(&assets) {
                findings.issues.push(format!(
                    "{name} references an image outside the configured assets directory: {target}"
                ));
                continue;
            }
            if !candidate.is_file() {
                findings
                    .issues
                    .push(format!("{name} references missing image: {target}"));
                continue;
            }
            let ext = candidate
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if !IMAGE_SUFFIXES.contains(&ext.as_str()) {
                findings.issues.push(format!(
                    "{name} references unsupported image format: {target}"
                ));
                continue;
            }
            findings.image_count += 1;
            findings
                .issues
                .extend(image_issues(&candidate, &attributes));
        }
        let (tables, issues) = table_issues(&name, &text);
        findings.table_count += tables;
        findings.issues.extend(issues);
    }
    findings
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A minimal PNG header of `w` × `h` pixels.
    pub(crate) fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0]);
        b
    }

    #[test]
    fn measurements_convert_to_inches_like_agenthicc() {
        assert_eq!(parse_measurement("2in", true), Some(2.0));
        assert!((parse_measurement("96", true).unwrap() - 1.0).abs() < 1e-9);
        assert!((parse_measurement("72pt", false).unwrap() - 1.0).abs() < 1e-9);
        assert!((parse_measurement("50%", true).unwrap() - 3.25).abs() < 1e-9);
        assert!((parse_measurement("10%", false).unwrap() - 1.15).abs() < 1e-9);
        assert_eq!(parse_measurement("auto", true), None);
    }

    #[test]
    fn image_headers_are_read_without_an_image_library() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        std::fs::write(&p, png(1200, 800)).unwrap();
        assert_eq!(image_dimensions(&p), Some((1200, 800)));
        let g = dir.path().join("a.gif");
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[0x40, 0x01, 0xF0, 0x00]);
        std::fs::write(&g, gif).unwrap();
        assert_eq!(image_dimensions(&g), Some((320, 240)));
        let s = dir.path().join("a.svg");
        std::fs::write(
            &s,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 640 480"></svg>"#,
        )
        .unwrap();
        assert_eq!(image_dimensions(&s), Some((640, 480)));
        let j = dir.path().join("a.jpg");
        let mut jpg = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08];
        jpg.extend_from_slice(&300u16.to_be_bytes());
        jpg.extend_from_slice(&400u16.to_be_bytes());
        jpg.extend_from_slice(&[0; 12]);
        std::fs::write(&j, jpg).unwrap();
        assert_eq!(image_dimensions(&j), Some((400, 300)));
    }

    #[test]
    fn oversized_media_and_tables_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("chapters")).unwrap();
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/wide.png"), png(3000, 1000)).unwrap();
        std::fs::write(root.join("assets/fine.png"), png(600, 400)).unwrap();
        let wide_row = format!("| {} | b |", "x".repeat(200));
        std::fs::write(
            root.join("chapters/01-a.md"),
            format!(
                "# A\n\n![w](../assets/wide.png){{width=7in}}\n\n![f](assets/fine.png){{width=4in}}\n\n\
                 ![m](../assets/missing.png)\n\n| a | b |\n|---|---|\n{wide_row}\n"
            ),
        )
        .unwrap();
        let findings = layout_findings(root, &root.join("assets"));
        assert_eq!(findings.files, ["chapters/01-a.md"]);
        assert_eq!(findings.image_count, 2);
        assert_eq!(findings.table_count, 1);
        let text = findings.issues.join("\n");
        assert!(text.contains("wide.png is 7.00in wide"), "{text}");
        assert!(
            text.contains("missing image: ../assets/missing.png"),
            "{text}"
        );
        assert!(text.contains("01-a.md table 1 is approximately"), "{text}");
        assert!(!text.contains("fine.png"), "{text}");
    }

    #[test]
    fn an_agent_written_contents_file_and_outside_images_are_issues() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("front-matter")).unwrap();
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("front-matter/contents.md"), "# Contents\n").unwrap();
        std::fs::write(
            root.join("front-matter/preface.md"),
            "# P\n![x](../../etc/x.png)\n",
        )
        .unwrap();
        let text = layout_findings(root, &root.join("assets"))
            .issues
            .join("\n");
        assert!(
            text.contains("contents.md is an agent-authored TOC source"),
            "{text}"
        );
        assert!(
            text.contains("outside the configured assets directory"),
            "{text}"
        );
    }
}
