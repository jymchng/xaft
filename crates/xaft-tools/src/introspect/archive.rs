//! The format of the embedded self-introspection corpus.
//!
//! The root crate's build script (`build.rs`) walks xaft's documentation and
//! Rust source and writes them into one blob with [`ArchiveWriter`]; the
//! binary embeds the blob with `include_bytes!` and hands it to
//! [`super::install_embedded_corpus`], which reads it with [`Archive`]. The
//! build script includes this file by `#[path]`, so writer and reader are the
//! same code and cannot drift.
//!
//! Layout (all integers little-endian):
//!
//! ```text
//! MAGIC  "XAFTCRP1"
//! u32    entry count
//! entry* kind u8 · path (u16 len + UTF-8) · title (u16 len + UTF-8)
//!        · lines u32 · raw bytes u32 · compressed bytes u32
//! data   each entry's raw-DEFLATE stream, in entry order
//! ```
//!
//! Each file is compressed on its own so reading one document inflates one
//! document. This file depends on nothing but `std` and `miniz_oxide`, because
//! the build script compiles it too.

use std::path::{Path, PathBuf};

/// The first eight bytes of every archive.
pub const MAGIC: &[u8; 8] = b"XAFTCRP1";

/// agenthicc's `_EXTRA_DOCS`: top-level documents served beside the docs
/// tree, each taken from `docs/<name>` when present, else `<repo>/<name>`.
pub const EXTRA_DOCS: [&str; 3] = ["llms.txt", "llms-full.txt", "README.md"];

/// agenthicc's `_DOC_SUFFIXES`.
pub const DOC_SUFFIXES: [&str; 3] = ["md", "txt", "json"];

/// Which half of the corpus an entry belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A documentation page, keyed relative to `docs/` (`guides/tools.md`)
    /// or a top-level document (`llms.txt`).
    Doc,
    /// A Rust source file, keyed relative to the repository root
    /// (`crates/xaft-tools/src/lib.rs`).
    Source,
}

impl EntryKind {
    fn to_byte(self) -> u8 {
        match self {
            Self::Doc => 0,
            Self::Source => 1,
        }
    }

    fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Doc),
            1 => Some(Self::Source),
            _ => None,
        }
    }
}

/// What the index records about one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Documentation or source.
    pub kind: EntryKind,
    /// The key the tools publish it under (POSIX separators).
    pub path: String,
    /// A document's first `# ` heading, or its file name; empty for source.
    pub title: String,
    /// Line count (`str::lines`).
    pub lines: u32,
    /// Size of the original file in bytes.
    pub bytes: u32,
    compressed: u32,
    offset: usize,
}

/// agenthicc's `_title_of`: the first Markdown `# ` heading within the first
/// 60 lines, else the file name.
pub fn title_of(path: &str, text: &str) -> String {
    for line in text.lines().take(60) {
        let stripped = line.trim();
        if let Some(rest) = stripped.strip_prefix("# ") {
            return rest.trim().to_string();
        }
    }
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// Builds an archive in memory.
#[derive(Debug)]
pub struct ArchiveWriter {
    level: u8,
    entries: Vec<(EntryKind, String, String, u32, u32, Vec<u8>)>,
}

impl Default for ArchiveWriter {
    fn default() -> Self {
        Self::new(6)
    }
}

impl ArchiveWriter {
    /// A writer compressing at DEFLATE `level` (0-10).
    pub fn new(level: u8) -> Self {
        Self {
            level,
            entries: Vec::new(),
        }
    }

    /// Add one file. Later additions with the same kind and path are ignored,
    /// so the first spelling wins.
    pub fn add(&mut self, kind: EntryKind, path: &str, text: &str) {
        if self
            .entries
            .iter()
            .any(|(k, p, ..)| *k == kind && p == path)
        {
            return;
        }
        let title = match kind {
            EntryKind::Doc => title_of(path, text),
            EntryKind::Source => String::new(),
        };
        let lines = u32::try_from(text.lines().count()).unwrap_or(u32::MAX);
        let bytes = u32::try_from(text.len()).unwrap_or(u32::MAX);
        let compressed = miniz_oxide::deflate::compress_to_vec(text.as_bytes(), self.level);
        self.entries
            .push((kind, path.to_string(), title, lines, bytes, compressed));
    }

    /// Number of files added so far.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if nothing has been added.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Serialise the archive.
    pub fn finish(self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for (kind, path, title, lines, bytes, compressed) in &self.entries {
            out.push(kind.to_byte());
            for text in [path, title] {
                let raw = text.as_bytes();
                let len = raw.len().min(u16::MAX as usize);
                out.extend_from_slice(&(len as u16).to_le_bytes());
                out.extend_from_slice(&raw[..len]);
            }
            out.extend_from_slice(&lines.to_le_bytes());
            out.extend_from_slice(&bytes.to_le_bytes());
            out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        }
        for (.., compressed) in &self.entries {
            out.extend_from_slice(compressed);
        }
        out
    }
}

/// Every regular file under `dir`, recursively, skipping hidden entries,
/// `target` directories and symlinks.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "target" {
            continue;
        }
        match entry.file_type() {
            Ok(t) if t.is_dir() => walk(&entry.path(), out),
            Ok(t) if t.is_file() => out.push(entry.path()),
            _ => {}
        }
    }
}

fn relative(base: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(base).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

fn has_suffix(path: &Path, suffixes: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| suffixes.contains(&e))
}

/// Pack an xaft checkout rooted at `root`: the top-level documents, the
/// `docs/` tree (`.md`, `.txt`, `.json`, keyed relative to `docs/`), and every
/// `.rs` file under `crates/` and `src/` (keyed relative to `root`), each set
/// in path order. Unreadable or non-UTF-8 files are skipped.
pub fn pack_checkout(root: &Path, level: u8) -> ArchiveWriter {
    let mut writer = ArchiveWriter::new(level);
    let docs = root.join("docs");
    for name in EXTRA_DOCS {
        for candidate in [docs.join(name), root.join(name)] {
            if let Ok(text) = std::fs::read_to_string(&candidate) {
                writer.add(EntryKind::Doc, name, &text);
                break;
            }
        }
    }
    let mut files = Vec::new();
    walk(&docs, &mut files);
    files.sort();
    for file in files {
        let Some(key) = relative(&docs, &file) else {
            continue;
        };
        if !has_suffix(&file, &DOC_SUFFIXES) || EXTRA_DOCS.contains(&key.as_str()) {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&file) {
            writer.add(EntryKind::Doc, &key, &text);
        }
    }
    let mut sources = Vec::new();
    walk(&root.join("crates"), &mut sources);
    walk(&root.join("src"), &mut sources);
    sources.sort();
    for file in sources {
        if !has_suffix(&file, &["rs"]) {
            continue;
        }
        let (Some(key), Ok(text)) = (relative(root, &file), std::fs::read_to_string(&file)) else {
            continue;
        };
        writer.add(EntryKind::Source, &key, &text);
    }
    writer
}

/// A parsed archive index over borrowed bytes.
#[derive(Debug, Clone)]
pub struct Archive {
    entries: Vec<Entry>,
    data_start: usize,
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| "corpus archive is truncated".to_string())?;
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn text(&mut self) -> Result<String, String> {
        let len = self.u16()? as usize;
        String::from_utf8(self.take(len)?.to_vec())
            .map_err(|_| "corpus archive holds a non-UTF-8 name".to_string())
    }
}

impl Archive {
    /// Parse the index of `bytes`, checking every entry lies inside it.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let mut cur = Cursor { bytes, pos: 0 };
        if cur.take(MAGIC.len())? != MAGIC {
            return Err("not an xaft corpus archive (bad magic)".to_string());
        }
        let count = cur.u32()? as usize;
        let mut entries = Vec::with_capacity(count.min(100_000));
        let mut offset = 0usize;
        for _ in 0..count {
            let kind = EntryKind::from_byte(cur.take(1)?[0])
                .ok_or_else(|| "corpus archive holds an unknown entry kind".to_string())?;
            let path = cur.text()?;
            let title = cur.text()?;
            let lines = cur.u32()?;
            let raw = cur.u32()?;
            let compressed = cur.u32()?;
            entries.push(Entry {
                kind,
                path,
                title,
                lines,
                bytes: raw,
                compressed,
                offset,
            });
            offset = offset
                .checked_add(compressed as usize)
                .ok_or_else(|| "corpus archive is too large".to_string())?;
        }
        let data_start = cur.pos;
        if data_start.checked_add(offset) != Some(bytes.len()) {
            return Err("corpus archive data does not match its index".to_string());
        }
        Ok(Self {
            entries,
            data_start,
        })
    }

    /// Every entry, in archive order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Inflate `entry`'s text out of `bytes` (the same bytes that were parsed).
    pub fn text(&self, bytes: &[u8], entry: &Entry) -> Result<String, String> {
        let start = self.data_start + entry.offset;
        let blob = bytes
            .get(start..start + entry.compressed as usize)
            .ok_or_else(|| format!("{} lies outside the corpus archive", entry.path))?;
        let raw = miniz_oxide::inflate::decompress_to_vec_with_limit(blob, entry.bytes as usize)
            .map_err(|e| format!("{} could not be inflated: {e:?}", entry.path))?;
        String::from_utf8(raw).map_err(|_| format!("{} is not UTF-8", entry.path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_text_titles_and_counts() {
        let mut w = ArchiveWriter::new(6);
        w.add(EntryKind::Doc, "guides/tools.md", "intro\n# Tools\nbody\n");
        w.add(EntryKind::Doc, "tool-registry.txt", "a\nb");
        w.add(EntryKind::Source, "crates/x/src/lib.rs", "pub fn f() {}\n");
        w.add(
            EntryKind::Source,
            "crates/x/src/lib.rs",
            "ignored duplicate",
        );
        assert_eq!(w.len(), 3);
        let bytes = w.finish();
        let archive = Archive::parse(&bytes).unwrap();
        let e = archive.entries();
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].title, "Tools");
        assert_eq!(e[1].title, "tool-registry.txt", "no heading: the file name");
        assert_eq!((e[0].lines, e[0].bytes), (3, 19));
        assert_eq!(e[2].kind, EntryKind::Source);
        assert_eq!(archive.text(&bytes, &e[2]).unwrap(), "pub fn f() {}\n");
        assert_eq!(archive.text(&bytes, &e[1]).unwrap(), "a\nb");
    }

    #[test]
    fn empty_files_survive_the_round_trip() {
        let mut w = ArchiveWriter::new(6);
        w.add(EntryKind::Doc, "empty.md", "");
        let bytes = w.finish();
        let a = Archive::parse(&bytes).unwrap();
        assert_eq!(a.text(&bytes, &a.entries()[0]).unwrap(), "");
        assert_eq!(a.entries()[0].title, "empty.md");
    }

    #[test]
    fn packing_a_checkout_keys_docs_and_sources_like_agenthicc() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let write = |rel: &str, text: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        };
        write("README.md", "# repo readme\n");
        write("llms.txt", "# llms\n");
        write("docs/README.md", "# docs readme\n");
        write("docs/guides/b.md", "# B\n");
        write("docs/a.txt", "a\n");
        write("docs/image.png", "not a doc");
        write("crates/x/src/lib.rs", "pub fn f() {}\n");
        write("crates/x/target/debug/build.rs", "fn skipped() {}\n");
        write("src/main.rs", "fn main() {}\n");
        write("crates/x/.hidden/h.rs", "fn hidden() {}\n");
        let bytes = pack_checkout(root, 1).finish();
        let archive = Archive::parse(&bytes).unwrap();
        let keys: Vec<(EntryKind, &str)> = archive
            .entries()
            .iter()
            .map(|e| (e.kind, e.path.as_str()))
            .collect();
        assert_eq!(
            keys,
            [
                (EntryKind::Doc, "llms.txt"),
                (EntryKind::Doc, "README.md"),
                (EntryKind::Doc, "a.txt"),
                (EntryKind::Doc, "guides/b.md"),
                (EntryKind::Source, "crates/x/src/lib.rs"),
                (EntryKind::Source, "src/main.rs"),
            ]
        );
        let readme = &archive.entries()[1];
        assert_eq!(
            readme.title, "docs readme",
            "docs/README.md wins, as in agenthicc"
        );
    }

    #[test]
    fn damaged_archives_are_rejected_not_trusted() {
        assert!(Archive::parse(b"nope").is_err());
        let mut w = ArchiveWriter::default();
        w.add(EntryKind::Doc, "a.md", "# A\n");
        let mut bytes = w.finish();
        assert!(Archive::parse(&bytes[..bytes.len() - 1]).is_err());
        bytes.push(0);
        assert!(
            Archive::parse(&bytes).is_err(),
            "trailing bytes are not ignored"
        );
        let mut bad_kind = ArchiveWriter::default();
        bad_kind.add(EntryKind::Doc, "a.md", "x");
        let mut b = bad_kind.finish();
        b[12] = 9;
        assert!(Archive::parse(&b).is_err());
    }
}
