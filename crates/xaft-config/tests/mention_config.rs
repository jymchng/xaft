//! Integration tests for the F3 @-mention configuration.
//!
//! Tests cover:
//! - Default values per PRD 30b §12.5
//! - TOML parsing for `[mention]` section
//! - `escape_policy` enum parsing
//! - `escape_allowlist` glob validation
//! - Migration of pre-F3 configs (no `[mention]` section)

use tempfile::TempDir;

use xaft_config::{ConfigLoader, EscapePolicy, MentionConfig, XaftConfig};

fn write_config(dir: &TempDir, filename: &str, content: &str) -> std::path::PathBuf {
    let path = dir.path().join(filename);
    std::fs::write(&path, content).expect("write config");
    path
}

#[test]
fn mention_config_default_is_confirm() {
    let c = MentionConfig::default();
    assert_eq!(c.escape_policy, EscapePolicy::Confirm);
    assert!(c.escape_allowlist.is_empty());
    assert_eq!(c.max_inline_lines, 2_000);
    assert_eq!(c.max_inline_bytes, 50_000);
    assert_eq!(c.resolver_max_file_bytes, 100_000_000); // 100 MB (decimal)
    assert!(!c.dedupe);
}

#[test]
fn escape_policy_default_value_is_confirm() {
    assert_eq!(EscapePolicy::default(), EscapePolicy::Confirm);
}

#[test]
fn escape_policy_parses_from_toml_string() {
    let dir = TempDir::new().unwrap();
    write_config(
        &dir,
        "xaft.toml",
        r#"
[mention]
escape_policy = "always"
"#,
    );
    let cfg = ConfigLoader::load_file(&dir.path().join("xaft.toml")).expect("load config");
    assert_eq!(cfg.mention.escape_policy, EscapePolicy::Always);
}

#[test]
fn escape_policy_parses_never() {
    let dir = TempDir::new().unwrap();
    write_config(
        &dir,
        "xaft.toml",
        r#"
[mention]
escape_policy = "never"
"#,
    );
    let cfg = ConfigLoader::load_file(&dir.path().join("xaft.toml")).expect("load config");
    assert_eq!(cfg.mention.escape_policy, EscapePolicy::Never);
}

#[test]
fn escape_policy_unknown_value_errors_at_load() {
    let dir = TempDir::new().unwrap();
    write_config(
        &dir,
        "xaft.toml",
        r#"
[mention]
escape_policy = "maybe"
"#,
    );
    let result = ConfigLoader::load_file(&dir.path().join("xaft.toml"));
    assert!(result.is_err(), "expected error for unknown escape_policy");
}

#[test]
fn escape_allowlist_parses_glob_patterns() {
    let dir = TempDir::new().unwrap();
    write_config(
        &dir,
        "xaft.toml",
        r#"
[mention]
escape_allowlist = ["**/notes/*.md", "~/work/**", "/etc/hosts"]
"#,
    );
    let cfg = ConfigLoader::load_file(&dir.path().join("xaft.toml")).expect("load config");
    assert_eq!(cfg.mention.escape_allowlist.len(), 3);
    assert_eq!(cfg.mention.escape_allowlist[0], "**/notes/*.md");
    assert_eq!(cfg.mention.escape_allowlist[1], "~/work/**");
    assert_eq!(cfg.mention.escape_allowlist[2], "/etc/hosts");
}

#[test]
fn escape_allowlist_empty_is_valid() {
    let dir = TempDir::new().unwrap();
    write_config(
        &dir,
        "xaft.toml",
        r#"
[mention]
escape_allowlist = []
"#,
    );
    let cfg = ConfigLoader::load_file(&dir.path().join("xaft.toml")).expect("load config");
    assert!(cfg.mention.escape_allowlist.is_empty());
}

#[test]
fn mention_config_appears_in_xaft_config_default() {
    let cfg = XaftConfig::default();
    // Smoke check: the field exists and is the default MentionConfig.
    assert_eq!(
        cfg.mention.escape_policy,
        MentionConfig::default().escape_policy
    );
    assert_eq!(
        cfg.mention.max_inline_lines,
        MentionConfig::default().max_inline_lines
    );
}

#[test]
fn mention_caps_round_trip_through_toml() {
    let dir = TempDir::new().unwrap();
    write_config(
        &dir,
        "xaft.toml",
        r#"
[mention]
max_inline_lines = 500
max_inline_bytes = 10000
image_max_bytes = 2000000
resolver_max_file_bytes = 5000000
dedupe = true
escape_policy = "confirm"
escape_allowlist = ["**/*.txt"]
"#,
    );
    let cfg = ConfigLoader::load_file(&dir.path().join("xaft.toml")).expect("load config");
    assert_eq!(cfg.mention.max_inline_lines, 500);
    assert_eq!(cfg.mention.max_inline_bytes, 10_000);
    assert_eq!(cfg.mention.image_max_bytes, 2_000_000);
    assert_eq!(cfg.mention.resolver_max_file_bytes, 5_000_000);
    assert!(cfg.mention.dedupe);
    assert_eq!(cfg.mention.escape_policy, EscapePolicy::Confirm);
    assert_eq!(cfg.mention.escape_allowlist, vec!["**/*.txt"]);
}

#[test]
fn pre_f3_config_without_mention_section_loads_with_defaults() {
    // A v0.1 config file that predates F3 has no [mention] section.
    // The loader must backfill the default MentionConfig so the
    // migration path is invisible to the user.
    let dir = TempDir::new().unwrap();
    write_config(
        &dir,
        "xaft.toml",
        r#"
[core]
data_dir = "/tmp/xaft"
"#,
    );
    let cfg = ConfigLoader::load_file(&dir.path().join("xaft.toml")).expect("load pre-F3 config");
    assert_eq!(
        cfg.mention.escape_policy,
        MentionConfig::default().escape_policy
    );
}

#[test]
fn escape_policy_serde_round_trip() {
    use serde_json;
    for (s, expected) in [
        (r#""confirm""#, EscapePolicy::Confirm),
        (r#""always""#, EscapePolicy::Always),
        (r#""never""#, EscapePolicy::Never),
    ] {
        let parsed: EscapePolicy = serde_json::from_str(s).expect("parse");
        assert_eq!(parsed, expected, "from {s}");
        let ser = serde_json::to_string(&parsed).expect("ser");
        assert_eq!(ser, s, "round-trip {s}");
    }
}
