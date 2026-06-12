//! AGENTS.md auto-loading — injects project-level agent instructions as system context.
//!
//! Discovers `AGENTS.md` files in the working directory and its parent,
//! reads and size-caps them, and returns a list of [`Message::system`] entries
//! suitable for prepending to `RunRequest::prior_messages`.

use std::path::{Path, PathBuf};

use agtrs_runtime::transport::Message;

/// Hard maximum: 64 KiB per load call.
const MAX_BYTES: usize = 65_536;

const FILENAME: &str = "AGENTS.md";

// ── Public API ────────────────────────────────────────────────────────────────

/// Discover and load all AGENTS.md files relevant to `working_dir`.
///
/// Returns a list of `Message::system(…)` entries to prepend to
/// `RunRequest::prior_messages`.  The list is ordered root-first (working
/// directory before parent).
///
/// `max_bytes` is capped at [`MAX_BYTES`] regardless of the caller-supplied
/// value.
pub async fn load_agents_md(working_dir: &Path, max_bytes: usize) -> Vec<Message> {
    let max_bytes = max_bytes.min(MAX_BYTES);
    let mut messages = Vec::new();

    // 1. Root AGENTS.md in working_dir.
    let root_path = working_dir.join(FILENAME);
    if let Some(msg) = read_agents_md_file(&root_path, max_bytes).await {
        messages.push(msg);
    }

    // 2. Parent directory (one level up) — useful for monorepo setups.
    if let Some(parent) = working_dir.parent() {
        let parent_path = parent.join(FILENAME);
        if parent_path != root_path {
            if let Some(msg) = read_agents_md_file(&parent_path, max_bytes).await {
                messages.push(msg);
            }
        }
    }

    messages
}

/// Return the paths of all AGENTS.md files that would be loaded for
/// `working_dir`, without actually reading them.
pub fn agents_md_paths(working_dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let root = working_dir.join(FILENAME);
    if root.exists() {
        paths.push(root.clone());
    }
    if let Some(parent) = working_dir.parent() {
        let parent_path = parent.join(FILENAME);
        if parent_path != root && parent_path.exists() {
            paths.push(parent_path);
        }
    }
    paths
}

/// Scan a list of workspace file paths for AGENTS.md entries.
///
/// Returns those paths that end with `/AGENTS.md` or equal `"AGENTS.md"`.
pub fn find_agents_md_in_file_list(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter(|p| p.ends_with("/AGENTS.md") || p.as_str() == "AGENTS.md")
        .cloned()
        .collect()
}

// ── Private helpers ───────────────────────────────────────────────────────────

async fn read_agents_md_file(path: &Path, max_bytes: usize) -> Option<Message> {
    let content = tokio::fs::read_to_string(path).await.ok()?;
    if content.trim().is_empty() {
        return None;
    }
    let (body, truncated) = truncate_utf8(&content, max_bytes);
    let suffix = if truncated {
        "\n\n[AGENTS.md truncated — file exceeded size limit]"
    } else {
        ""
    };
    let header = format!("## Project instructions from {}\n\n", path.display());
    Some(Message::system(format!("{header}{body}{suffix}")))
}

/// Truncate `s` to at most `max_bytes` bytes at a valid UTF-8 boundary.
///
/// Returns `(slice, was_truncated)`.
fn truncate_utf8(s: &str, max_bytes: usize) -> (&str, bool) {
    if s.len() <= max_bytes {
        return (s, false);
    }
    // Walk back from max_bytes to find a valid char boundary.
    let mut boundary = max_bytes;
    while boundary > 0 && !s.is_char_boundary(boundary) {
        boundary -= 1;
    }
    (&s[..boundary], true)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn write(dir: &Path, name: &str, content: &str) {
        tokio::fs::write(dir.join(name), content).await.unwrap();
    }

    #[tokio::test]
    async fn loads_from_working_dir() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "AGENTS.md", "# instructions\nBe helpful.").await;
        let msgs = load_agents_md(tmp.path(), 65_536).await;
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].text().contains("Be helpful."));
    }

    #[tokio::test]
    async fn empty_vec_when_absent() {
        let tmp = TempDir::new().unwrap();
        let msgs = load_agents_md(tmp.path(), 65_536).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn empty_file_not_loaded() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "AGENTS.md", "   \n   ").await;
        let msgs = load_agents_md(tmp.path(), 65_536).await;
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn truncates_large_file() {
        let tmp = TempDir::new().unwrap();
        // Write 70 KB file.
        let content = "A".repeat(70_000);
        write(tmp.path(), "AGENTS.md", &content).await;
        let msgs = load_agents_md(tmp.path(), 65_536).await;
        assert_eq!(msgs.len(), 1);
        let text = msgs[0].text();
        assert!(text.contains("[AGENTS.md truncated"));
    }

    #[tokio::test]
    async fn truncate_utf8_handles_boundary() {
        let s = "hello world";
        let (slice, truncated) = truncate_utf8(s, 5);
        assert_eq!(slice, "hello");
        assert!(truncated);
    }

    #[tokio::test]
    async fn find_agents_md_in_file_list_works() {
        let paths = vec![
            "src/main.rs".to_string(),
            "AGENTS.md".to_string(),
            "sub/AGENTS.md".to_string(),
            "notAGENTS.md".to_string(),
        ];
        let found = find_agents_md_in_file_list(&paths);
        assert_eq!(found, vec!["AGENTS.md", "sub/AGENTS.md"]);
    }

    #[tokio::test]
    async fn loads_parent_dir_agents_md() {
        let tmp = TempDir::new().unwrap();
        // Create a sub-dir to use as working_dir.
        let sub = tmp.path().join("project");
        tokio::fs::create_dir(&sub).await.unwrap();
        // Put AGENTS.md only in the parent (tmp), not in `sub`.
        write(tmp.path(), "AGENTS.md", "parent instructions").await;
        let msgs = load_agents_md(&sub, 65_536).await;
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].text().contains("parent instructions"));
    }

    #[tokio::test]
    async fn loads_both_working_dir_and_parent() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("project");
        tokio::fs::create_dir(&sub).await.unwrap();
        write(&sub, "AGENTS.md", "sub instructions").await;
        write(tmp.path(), "AGENTS.md", "parent instructions").await;
        let msgs = load_agents_md(&sub, 65_536).await;
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].text().contains("sub instructions"));
        assert!(msgs[1].text().contains("parent instructions"));
    }
}
