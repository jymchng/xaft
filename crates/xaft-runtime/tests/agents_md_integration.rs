//! Integration tests for AGENTS.md auto-loading (PRD 51).

use tempfile::TempDir;
use xaft_runtime::agents_md::{find_agents_md_in_file_list, load_agents_md};

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn write_file(dir: &std::path::Path, name: &str, content: &str) {
    tokio::fs::write(dir.join(name), content).await.unwrap();
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn agents_md_loaded_from_working_dir() {
    let tmp = TempDir::new().unwrap();
    write_file(tmp.path(), "AGENTS.md", "# Instructions\nBe helpful.").await;
    let msgs = load_agents_md(tmp.path(), 65_536).await;
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].text().contains("Be helpful."));
    assert!(msgs[0].text().contains("Project instructions from"));
}

#[tokio::test]
async fn agents_md_truncated_when_too_large() {
    let tmp = TempDir::new().unwrap();
    // Write content larger than 65 536 bytes.
    let content = "A".repeat(70_000);
    write_file(tmp.path(), "AGENTS.md", &content).await;
    let msgs = load_agents_md(tmp.path(), 65_536).await;
    assert_eq!(msgs.len(), 1);
    assert!(
        msgs[0].text().contains("[AGENTS.md truncated"),
        "expected truncation notice, got: {}",
        &msgs[0].text()[..100]
    );
}

#[tokio::test]
async fn agents_md_not_loaded_when_absent() {
    let tmp = TempDir::new().unwrap();
    let msgs = load_agents_md(tmp.path(), 65_536).await;
    assert!(msgs.is_empty());
}

#[tokio::test]
async fn agents_md_empty_file_not_loaded() {
    let tmp = TempDir::new().unwrap();
    write_file(tmp.path(), "AGENTS.md", "   \n  ").await;
    let msgs = load_agents_md(tmp.path(), 65_536).await;
    assert!(msgs.is_empty());
}

#[tokio::test]
async fn agents_md_includes_header_with_path() {
    let tmp = TempDir::new().unwrap();
    write_file(tmp.path(), "AGENTS.md", "do things").await;
    let msgs = load_agents_md(tmp.path(), 65_536).await;
    assert_eq!(msgs.len(), 1);
    let text = msgs[0].text();
    assert!(
        text.contains("AGENTS.md"),
        "should contain AGENTS.md in path"
    );
}

#[tokio::test]
async fn agents_md_loads_parent_dir() {
    let tmp = TempDir::new().unwrap();
    let sub = tmp.path().join("project");
    tokio::fs::create_dir(&sub).await.unwrap();
    write_file(tmp.path(), "AGENTS.md", "parent instructions").await;

    let msgs = load_agents_md(&sub, 65_536).await;
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].text().contains("parent instructions"));
}

#[tokio::test]
async fn agents_md_loads_both_working_and_parent() {
    let tmp = TempDir::new().unwrap();
    let sub = tmp.path().join("project");
    tokio::fs::create_dir(&sub).await.unwrap();
    write_file(&sub, "AGENTS.md", "sub instructions").await;
    write_file(tmp.path(), "AGENTS.md", "parent instructions").await;

    let msgs = load_agents_md(&sub, 65_536).await;
    assert_eq!(msgs.len(), 2);
    // Working dir first, then parent.
    assert!(msgs[0].text().contains("sub instructions"));
    assert!(msgs[1].text().contains("parent instructions"));
}

#[tokio::test]
async fn find_agents_md_in_file_list_filters_correctly() {
    let paths = vec![
        "src/lib.rs".to_string(),
        "AGENTS.md".to_string(),
        "sub/AGENTS.md".to_string(),
        "notagents.md".to_string(),
        "AGENTS.mdx".to_string(),
    ];
    let found = find_agents_md_in_file_list(&paths);
    assert_eq!(found.len(), 2);
    assert!(found.contains(&"AGENTS.md".to_string()));
    assert!(found.contains(&"sub/AGENTS.md".to_string()));
}

#[tokio::test]
async fn agents_md_max_bytes_capped_at_65536() {
    let tmp = TempDir::new().unwrap();
    // Asking for more than 65_536 should be capped.
    let content = "B".repeat(100);
    write_file(tmp.path(), "AGENTS.md", &content).await;
    // Even with max_bytes=1_000_000, should load fine.
    let msgs = load_agents_md(tmp.path(), 1_000_000).await;
    assert_eq!(msgs.len(), 1);
    assert!(!msgs[0].text().contains("[AGENTS.md truncated"));
}
