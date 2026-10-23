//! `list_xaft_docs`, `read_xaft_doc`, `search_xaft_docs` — agenthicc's
//! `list_docs`, `read_doc` and `search_docs` over the embedded corpus.

use serde_json::{Value, json};

use super::rust_items::clip;
use super::{
    Corpus, Entry, EntryKind, MAX_DOC_LINES, MAX_LINE_CHARS, MAX_SEARCH_RESULTS, failure, matcher,
    repr, search_result,
};

use super::archive::EXTRA_DOCS;

/// agenthicc's `docs_unavailable_result`, for a build without a corpus.
pub fn unavailable(reason: &str) -> Value {
    failure(
        format!("No xaft documentation is available in this installation: {reason}."),
        "Use an xaft binary built from a source checkout (its build script embeds docs/, \
         llms.txt and llms-full.txt), or use inspect_xaft_source to read the code directly.",
    )
}

fn as_json(e: &Entry) -> Value {
    json!({"path": e.path, "title": e.title, "lines": e.lines, "bytes": e.bytes})
}

/// Every document, filtered by `section` prefix: the top-level documents
/// first, then the docs tree in path order.
pub fn list<'a>(corpus: &'a Corpus, section: &str) -> Vec<&'a Entry> {
    let wanted = section.trim().trim_matches('/');
    let mut extras: Vec<&Entry> = EXTRA_DOCS
        .iter()
        .filter_map(|name| corpus.find(EntryKind::Doc, name))
        .filter(|e| wanted.is_empty() || e.path.starts_with(wanted))
        .collect();
    let mut rest: Vec<&Entry> = corpus
        .docs()
        .filter(|e| !EXTRA_DOCS.contains(&e.path.as_str()))
        .filter(|e| wanted.is_empty() || e.path.starts_with(wanted))
        .collect();
    rest.sort_by(|a, b| a.path.cmp(&b.path));
    extras.extend(rest);
    extras
}

/// The `list_xaft_docs` result.
pub fn list_result(corpus: &Corpus, section: &str) -> Value {
    if corpus.docs().next().is_none() {
        return unavailable("the embedded corpus holds no documents");
    }
    let entries = list(corpus, section);
    json!({
        "ok": true,
        "section": section,
        "count": entries.len(),
        "documents": entries.iter().map(|e| as_json(e)).collect::<Vec<_>>(),
    })
}

/// Resolve a document key. `docs/guides/x.md` is accepted for `guides/x.md`.
fn find<'a>(corpus: &'a Corpus, path: &str) -> Option<&'a Entry> {
    let key = path.trim().replace('\\', "/");
    let key = key.trim_start_matches('/');
    if key.is_empty() {
        return None;
    }
    corpus.find(EntryKind::Doc, key).or_else(|| {
        key.strip_prefix("docs/")
            .and_then(|k| corpus.find(EntryKind::Doc, k))
    })
}

/// The `read_xaft_doc` result: a bounded window of one document.
pub fn read(corpus: &Corpus, path: &str, start_line: i64, max_lines: i64) -> Value {
    if corpus.docs().next().is_none() {
        return unavailable("the embedded corpus holds no documents");
    }
    let Some(entry) = find(corpus, path) else {
        return failure(
            format!("No xaft document is published at {}.", repr(path)),
            "Call list_xaft_docs() to see the available paths.",
        );
    };
    let text = match corpus.text(entry) {
        Ok(t) => t,
        Err(_) => {
            return failure(
                format!("{path} could not be read as text."),
                "Pick a different document from list_xaft_docs().",
            );
        }
    };
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let window = max_lines.clamp(1, MAX_DOC_LINES as i64) as usize;
    let start = start_line.max(1) as usize;
    let selected: Vec<&str> = lines.iter().skip(start - 1).take(window).copied().collect();
    let end = start + selected.len() - 1;
    json!({
        "ok": true,
        "path": path,
        "title": entry.title,
        "total_lines": total,
        "start_line": start,
        "end_line": end.max(start - 1),
        "truncated": end < total,
        "next_start_line": if end < total { end + 1 } else { 0 },
        "content": selected.join("\n"),
    })
}

/// The `search_xaft_docs` result: matching lines with their paths.
pub fn search(corpus: &Corpus, query: &str, max_results: i64, section: &str) -> Value {
    if corpus.docs().next().is_none() {
        return unavailable("the embedded corpus holds no documents");
    }
    if query.trim().is_empty() {
        return failure(
            "A non-empty query is required.",
            "Call search_xaft_docs('capability gate') with the phrase to find.",
        );
    }
    let is_match = matcher(query);
    let limit = max_results.clamp(1, MAX_SEARCH_RESULTS as i64) as usize;
    let mut hits = Vec::new();
    let mut scanned = 0;
    for entry in list(corpus, section) {
        let Ok(text) = corpus.text(entry) else {
            continue;
        };
        scanned += 1;
        for (number, line) in text.lines().enumerate() {
            if is_match(line) {
                hits.push(json!({
                    "path": entry.path,
                    "line": number + 1,
                    "text": clip(line.trim(), MAX_LINE_CHARS),
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
    fn listing_puts_top_level_documents_first_and_filters_by_section() {
        let c = corpus();
        let all = list_result(&c, "");
        let paths: Vec<&str> = all["documents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["path"].as_str().unwrap())
            .collect();
        assert_eq!(
            paths,
            [
                "llms.txt",
                "README.md",
                "guides/tools.md",
                "guides/workflows.md",
                "reference/cli.md"
            ]
        );
        assert_eq!(all["count"], json!(5));
        assert_eq!(all["documents"][2]["title"], json!("Tools"));
        assert_eq!(all["documents"][4]["title"], json!("cli.md"));
        let guides = list_result(&c, "/guides/");
        assert_eq!(guides["count"], json!(2));
        assert_eq!(guides["section"], json!("/guides/"));
    }

    #[test]
    fn reading_is_windowed_like_agenthicc() {
        let c = corpus();
        let r = read(&c, "guides/tools.md", 2, 2);
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["content"], json!("\nThe capability gate filters tools."));
        assert_eq!(
            (r["start_line"].clone(), r["end_line"].clone()),
            (json!(2), json!(3))
        );
        assert_eq!(r["truncated"], json!(true));
        assert_eq!(r["next_start_line"], json!(4));
        assert_eq!(r["total_lines"], json!(4));

        let tail = read(&c, "docs/guides/tools.md", 4, 400);
        assert_eq!(tail["truncated"], json!(false));
        assert_eq!(tail["next_start_line"], json!(0));
        let past = read(&c, "guides/tools.md", 99, 10);
        assert_eq!(
            (past["end_line"].clone(), past["content"].clone()),
            (json!(98), json!(""))
        );

        let missing = read(&c, "../../etc/passwd", 1, 10);
        assert_eq!(missing["ok"], json!(false));
        assert_eq!(
            missing["error"],
            json!("No xaft document is published at '../../etc/passwd'.")
        );
        assert!(missing["fix"].as_str().unwrap().contains("list_xaft_docs"));
    }

    #[test]
    fn search_is_case_insensitive_regex_with_a_literal_fallback() {
        let c = corpus();
        let r = search(&c, "capability gate", 40, "");
        assert_eq!(r["match_count"], json!(2));
        assert_eq!(r["files_scanned"], json!(5));
        assert_eq!(
            r["matches"][0],
            json!({"path": "guides/tools.md", "line": 3, "text": "The capability gate filters tools."})
        );
        let capped = search(&c, "capability", 1, "");
        assert_eq!(
            (capped["match_count"].clone(), capped["truncated"].clone()),
            (json!(1), json!(true))
        );
        let scoped = search(&c, "gate", 40, "reference");
        assert_eq!(scoped["files_scanned"], json!(1));
        let literal = search(&c, "Phases advance (", 40, "");
        assert_eq!(
            literal["match_count"],
            json!(0),
            "an invalid regex is a literal"
        );
        assert_eq!(search(&c, "  ", 40, "")["ok"], json!(false));
    }

    #[test]
    fn a_corpus_without_documents_says_so() {
        let mut w = super::super::ArchiveWriter::new(1);
        w.add(EntryKind::Source, "src/main.rs", "fn main() {}");
        let c = Corpus::from_vec(w.finish()).unwrap();
        let r = list_result(&c, "");
        assert_eq!(r["ok"], json!(false));
        assert!(
            r["error"]
                .as_str()
                .unwrap()
                .starts_with("No xaft documentation")
        );
    }
}
