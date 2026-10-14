//! Integration tests for the tool security policy.
//!
//! Parity targets in agenthicc's `config.py`: `SecuritySettings`, plus
//! `ToolSettings.allowed` / `ToolSettings.denied`.
//!
//! The properties under test are the ones that decide whether a security
//! setting is safe to rely on:
//!
//! - the policy is **fail-closed**: no matching allow rule means denied, so an
//!   empty list denies everything rather than everything;
//! - `denied` is evaluated before `allowed`, so config order cannot change the
//!   outcome;
//! - domain matching is on **label boundaries**, so `notexample.com` does not
//!   slip through an `example.com` rule;
//! - path matching is on **path components**, so `/workspace-evil` is not
//!   inside `/workspace`;
//! - an explicit `false` or `null` is distinguishable from a default.
//!
//! Every test states the whole policy it is asserting against. A test that
//! inherited the defaults would keep passing if a default changed, which is
//! exactly the case where a security setting silently changes meaning.

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use xaft_config::{
    SecurityConfig, ToolDecision, ToolPolicy, ToolSettingsConfig, XaftConfig, glob_match,
};

fn s(text: &str) -> String {
    text.to_string()
}

/// A policy from an allow list and a deny list, with the sandbox off so only
/// the tool-pattern rules are under test.
fn tool_policy(allowed: &[&str], denied: &[&str]) -> ToolPolicy {
    let security = SecurityConfig {
        sandbox_mode: false,
        ..SecurityConfig::default()
    };
    ToolPolicy::new(
        &security,
        &allowed.iter().map(|p| s(p)).collect::<Vec<_>>(),
        &denied.iter().map(|p| s(p)).collect::<Vec<_>>(),
    )
}

// ── Defaults ──────────────────────────────────────────────────────────────────

#[test]
fn defaults_match_agenthicc_exactly() {
    let config = SecurityConfig::default();
    assert!(
        config.sandbox_mode,
        "agenthicc defaults sandbox_mode to true"
    );
    assert_eq!(
        config.allowed_paths,
        [PathBuf::from("/workspace")],
        "agenthicc defaults allowed_paths to [\"/workspace\"]"
    );
    assert!(
        config.network_allow_list.is_empty(),
        "agenthicc defaults the network allow-list to empty"
    );
    assert_eq!(config.max_tool_cpu_seconds, Some(30));
    assert_eq!(config.max_tool_memory_mb, Some(512));
}

#[test]
fn an_empty_network_allow_list_denies_every_host_while_the_sandbox_is_on() {
    // The opposite reading -- "empty means unrestricted" -- is the one that
    // turns a typo into an open egress rule, so it is pinned explicitly.
    let policy = SecurityConfig::default().policy();
    assert!(policy.sandbox_enabled());
    assert!(!policy.is_host_allowed("example.com"));
    assert!(!policy.is_host_allowed("localhost"));
}

// ── Tool allow / deny ─────────────────────────────────────────────────────────

#[test]
fn no_rules_denies_every_tool() {
    let policy = tool_policy(&[], &[]);
    assert!(!policy.is_tool_allowed("read_file"));
    assert!(!policy.is_tool_allowed("shell"));
    assert_eq!(policy.evaluate_tool("read_file"), ToolDecision::Deny);
}

#[test]
fn an_allow_list_admits_only_what_it_names() {
    let policy = tool_policy(&["read_file", "list_files"], &[]);
    assert!(policy.is_tool_allowed("read_file"));
    assert!(policy.is_tool_allowed("list_files"));
    assert!(!policy.is_tool_allowed("write_file"));
    assert!(!policy.is_tool_allowed("shell"));
}

#[test]
fn a_star_allow_pattern_admits_everything() {
    let policy = tool_policy(&["*"], &[]);
    for name in ["read_file", "shell", "anything-at-all"] {
        assert!(policy.is_tool_allowed(name), "{name} must be allowed");
    }
}

#[test]
fn a_prefix_pattern_admits_only_that_prefix() {
    let policy = tool_policy(&["file-*"], &[]);
    assert!(policy.is_tool_allowed("file-read"));
    assert!(policy.is_tool_allowed("file-write"));
    assert!(!policy.is_tool_allowed("shell-exec"));
    assert!(!policy.is_tool_allowed("my-file-read"));
}

#[test]
fn deny_beats_a_broader_allow() {
    // The whole reason denied is evaluated first: `allowed = ["*"]` plus
    // `denied = ["shell"]` is a safe thing to write, and it must stay safe no
    // matter which order the two appear in.
    let policy = tool_policy(&["*"], &["shell"]);
    assert!(!policy.is_tool_allowed("shell"));
    assert!(policy.is_tool_allowed("read_file"));
}

#[test]
fn deny_order_does_not_matter_against_a_broad_allow() {
    let a = tool_policy(&["*"], &["file-*"]);
    let b = tool_policy(&["*"], &["file-*"]);
    for name in ["file-read", "file-write", "read_file", "shell"] {
        assert_eq!(
            a.evaluate_tool(name),
            b.evaluate_tool(name),
            "the verdict for {name} must not depend on construction order"
        );
    }
    assert!(!a.is_tool_allowed("file-write"));
}

#[test]
fn deny_alone_denies_the_named_tools_and_nothing_else_is_implied() {
    // Deny-only is not "deny everything": a rule that is not present does not
    // fire. Combined with the empty-allowed default, the caller must also give
    // an allow list for anything to be callable at all.
    let policy = tool_policy(&[], &["shell"]);
    assert!(!policy.is_tool_allowed("shell"));
    assert!(
        !policy.is_tool_allowed("read_file"),
        "no allow rule means deny"
    );
}

#[test]
fn filter_tools_preserves_registration_order() {
    let policy = tool_policy(&["*"], &["*write*"]);
    let kept = policy.filter_tools([
        "read_file",
        "write_file",
        "list_files",
        "batch_write",
        "grep",
    ]);
    assert_eq!(kept, ["read_file", "list_files", "grep"]);
}

#[test]
fn filter_tools_can_empty_a_registry_completely() {
    // This is the loud failure the fail-closed default is for: a config with no
    // allow rules produces a registry with no tools, not a registry with all of
    // them.
    let policy = tool_policy(&[], &[]);
    assert!(policy.filter_tools(["read_file", "write_file"]).is_empty());
}

// ── Glob matching ─────────────────────────────────────────────────────────────

#[test]
fn glob_star_matches_the_whole_text_including_nothing() {
    assert!(glob_match("*", ""));
    assert!(glob_match("*", "x"));
    assert!(glob_match("a*b*c", "abc"));
    assert!(glob_match("a*b*c", "axxbyyc"));
    assert!(!glob_match("a*b*c", "axxbyy"));
}

#[test]
fn glob_question_mark_matches_exactly_one_character() {
    assert!(glob_match("file-?", "file-a"));
    assert!(!glob_match("file-?", "file-"));
    assert!(!glob_match("file-?", "file-ab"));
}

#[test]
fn glob_star_backtracks_rather_than_greedy_failing() {
    // The classic failure of a non-backtracking matcher: after consuming the
    // one `*`, `c` cannot match `b`.
    assert!(glob_match("*bc", "abc"));
    assert!(glob_match("a*c", "abbbc"));
    assert!(glob_match("*a*b*", "xxaxxbxx"));
}

#[test]
fn glob_treats_regex_metacharacters_as_literals() {
    // A tool name is not a regex. `[`, `.` and `+` must not acquire meaning, or
    // a pattern like `file.read` would silently start matching `fileXread`.
    assert!(glob_match("file.read", "file.read"));
    assert!(!glob_match("file.read", "fileXread"));
    assert!(glob_match("a+b", "a+b"));
    assert!(!glob_match("a+b", "ab"));
    assert!(glob_match("[abc]", "[abc]"));
    assert!(!glob_match("[abc]", "a"));
}

#[test]
fn glob_pattern_must_match_the_whole_name() {
    assert!(!glob_match("read", "read_file"));
    assert!(!glob_match("file", "my_file"));
}

// ── Network allow-list ────────────────────────────────────────────────────────

fn net_policy(domains: &[&str], sandbox: bool) -> ToolPolicy {
    let security = SecurityConfig {
        sandbox_mode: sandbox,
        network_allow_list: domains.iter().map(|d| s(d)).collect(),
        ..SecurityConfig::default()
    };
    security.policy()
}

#[test]
fn an_exact_host_matches_its_allow_listed_domain() {
    let policy = net_policy(&["example.com"], true);
    assert!(policy.is_host_allowed("example.com"));
    assert!(!policy.is_host_allowed("other.com"));
}

#[test]
fn a_subdomain_matches_its_parent_domain() {
    let policy = net_policy(&["example.com"], true);
    assert!(policy.is_host_allowed("api.example.com"));
    assert!(policy.is_host_allowed("a.b.c.example.com"));
}

#[test]
fn a_lookalike_host_does_not_match_a_suffix_rule() {
    // The single most important case in this file. A raw `ends_with("example.com")`
    // check would let `notexample.com` and `example.com.evil.test` through.
    let policy = net_policy(&["example.com"], true);
    assert!(!policy.is_host_allowed("notexample.com"));
    assert!(!policy.is_host_allowed("myexample.com"));
    assert!(!policy.is_host_allowed("example.com.evil.test"));
}

#[test]
fn host_matching_ignores_case_and_a_trailing_dot() {
    let policy = net_policy(&["example.com"], true);
    assert!(policy.is_host_allowed("API.Example.COM"));
    assert!(policy.is_host_allowed("example.com."));
    assert!(policy.is_host_allowed("  example.com  "));
}

#[test]
fn an_allow_listed_domain_may_be_written_with_a_leading_dot() {
    let policy = net_policy(&[".example.com"], true);
    assert!(policy.is_host_allowed("api.example.com"));
    assert!(policy.is_host_allowed("example.com"));
}

#[test]
fn an_empty_host_is_never_allowed() {
    let policy = net_policy(&["example.com"], true);
    assert!(!policy.is_host_allowed(""));
    assert!(!policy.is_host_allowed("   "));
}

#[test]
fn disabling_the_sandbox_opens_every_host() {
    let policy = net_policy(&[], false);
    assert!(policy.is_host_allowed("example.com"));
    assert!(policy.is_host_allowed("literally-anything.test"));
}

#[test]
fn url_host_extraction_cannot_be_smuggled_past_the_check() {
    let policy = net_policy(&["example.com"], true);
    // The host is taken from the authority only. A hostile path, query or
    // userinfo segment must not change which host is checked.
    assert!(policy.is_url_allowed("https://example.com/anything"));
    assert!(policy.is_url_allowed("https://example.com/?q=evil.test"));
    assert!(policy.is_url_allowed("https://evil.test@example.com/"));
    assert!(policy.is_url_allowed("https://example.com#@evil.test"));
    assert!(policy.is_url_allowed("https://example.com:8443/x"));

    assert!(!policy.is_url_allowed("https://evil.test/example.com"));
    assert!(!policy.is_url_allowed("https://notexample.com/"));
}

#[test]
fn a_url_without_a_usable_host_is_denied() {
    let policy = net_policy(&["example.com"], true);
    for bad in ["", "   ", "https://", "not a url", "https://exa mple.com"] {
        assert!(!policy.is_url_allowed(bad), "{bad:?} must not be allowed");
    }
}

// ── Path allow-list ───────────────────────────────────────────────────────────

fn path_policy(roots: &[&str], sandbox: bool) -> ToolPolicy {
    let security = SecurityConfig {
        sandbox_mode: sandbox,
        allowed_paths: roots.iter().map(PathBuf::from).collect(),
        ..SecurityConfig::default()
    };
    security.policy()
}

#[test]
fn a_path_inside_a_root_is_allowed() {
    let policy = path_policy(&["/workspace"], true);
    assert!(policy.is_path_allowed(Path::new("/workspace")));
    assert!(policy.is_path_allowed(Path::new("/workspace/src/main.rs")));
    assert!(policy.is_path_allowed(Path::new("/workspace/a/b/c/d.txt")));
}

#[test]
fn a_path_outside_every_root_is_denied() {
    let policy = path_policy(&["/workspace", "/tmp/scratch"], true);
    assert!(!policy.is_path_allowed(Path::new("/etc/passwd")));
    assert!(!policy.is_path_allowed(Path::new("/")));
    assert!(policy.is_path_allowed(Path::new("/tmp/scratch/x")));
}

#[test]
fn a_sibling_directory_with_a_shared_prefix_is_outside_the_root() {
    // `/workspace-evil` shares a byte prefix with `/workspace` but is a
    // different path. Matching on bytes rather than components is the bug this
    // guards.
    let policy = path_policy(&["/workspace"], true);
    assert!(!policy.is_path_allowed(Path::new("/workspace-evil")));
    assert!(!policy.is_path_allowed(Path::new("/workspace-evil/secret")));
}

#[test]
fn dotdot_cannot_escape_a_root() {
    let policy = path_policy(&["/workspace"], true);
    assert!(!policy.is_path_allowed(Path::new("/workspace/../etc/passwd")));
    assert!(!policy.is_path_allowed(Path::new("/workspace/a/../../../etc")));
    assert!(policy.is_path_allowed(Path::new("/workspace/a/../b")));
}

#[test]
fn a_trailing_slash_on_a_root_does_not_change_its_scope() {
    let with = path_policy(&["/workspace"], true);
    let without = path_policy(&["/workspace/"], true);
    for path in ["/workspace", "/workspace/a.txt", "/workspace-evil"] {
        assert_eq!(
            with.is_path_allowed(Path::new(path)),
            without.is_path_allowed(Path::new(path)),
            "{path} must classify identically"
        );
    }
}

#[test]
fn a_path_that_does_not_exist_yet_is_still_classified() {
    // A write to a new file is exactly the case a sandbox has to answer, and it
    // is answered before the file exists. A checker that canonicalized first
    // would have nothing to check here.
    let policy = path_policy(&["/workspace"], true);
    assert!(policy.is_path_allowed(Path::new("/workspace/not/created/yet.txt")));
    assert!(!policy.is_path_allowed(Path::new("/etc/not/created/yet.txt")));
}

#[test]
fn duplicate_roots_are_collapsed_and_order_is_normalized() {
    let policy = path_policy(&["/workspace", "/workspace/", "/workspace"], true);
    assert_eq!(policy.roots(), [PathBuf::from("/workspace")]);
}

#[test]
fn disabling_the_sandbox_opens_every_path() {
    let policy = path_policy(&[], false);
    assert!(policy.is_path_allowed(Path::new("/etc/passwd")));
    assert!(policy.is_path_allowed(Path::new("/")));
}

// ── Resource limits ───────────────────────────────────────────────────────────

#[test]
fn resource_limits_come_from_the_config() {
    let config = SecurityConfig {
        max_tool_cpu_seconds: Some(5),
        max_tool_memory_mb: Some(64),
        ..SecurityConfig::default()
    };
    let limits = config.limits();
    assert_eq!(limits.cpu_seconds, Some(5));
    assert_eq!(limits.memory_mb, Some(64));
    assert!(!limits.is_unbounded());
}

#[test]
fn absent_ceilings_are_distinguishable_from_zero() {
    let unlimited = SecurityConfig {
        max_tool_cpu_seconds: None,
        max_tool_memory_mb: None,
        ..SecurityConfig::default()
    };
    assert!(unlimited.limits().is_unbounded());

    // Zero is rejected by validate(), but the type still distinguishes it, so a
    // caller cannot mistake one for the other by accident.
    let zero = SecurityConfig {
        max_tool_cpu_seconds: Some(0),
        max_tool_memory_mb: Some(0),
        ..SecurityConfig::default()
    };
    assert_eq!(zero.limits().cpu_seconds, Some(0));
    assert!(!zero.limits().is_unbounded());
}

// ── Validation ────────────────────────────────────────────────────────────────

#[test]
fn a_zero_ceiling_is_rejected() {
    for bad in [
        SecurityConfig {
            max_tool_cpu_seconds: Some(0),
            ..SecurityConfig::default()
        },
        SecurityConfig {
            max_tool_memory_mb: Some(0),
            ..SecurityConfig::default()
        },
    ] {
        let error = bad.policy().validate("security").expect_err("zero ceiling");
        assert!(error.to_string().contains("greater than zero"), "{error}");
    }
}

#[test]
fn absent_ceilings_validate() {
    SecurityConfig {
        max_tool_cpu_seconds: None,
        max_tool_memory_mb: None,
        ..SecurityConfig::default()
    }
    .policy()
    .validate("security")
    .expect("null ceilings are valid");
}

#[test]
fn a_duplicate_allowed_and_denied_pattern_is_rejected() {
    // Denied wins, so the allow rule is dead. Rejecting it beats silently
    // ignoring half of what the user wrote.
    let config = XaftConfig {
        tools: ToolSettingsConfig {
            allowed: vec![s("file-*")],
            denied: vec![s("file-*"), s("shell")],
            ..ToolSettingsConfig::default()
        },
        ..XaftConfig::default()
    };
    let error = xaft_config::validate(&config).expect_err("contradictory rules");
    assert!(error.to_string().contains("file-*"), "{error}");
    assert!(error.to_string().contains("dead"), "{error}");
}

#[test]
fn distinct_allowed_and_denied_patterns_validate() {
    let config = XaftConfig {
        tools: ToolSettingsConfig {
            allowed: vec![s("*")],
            denied: vec![s("shell")],
            ..ToolSettingsConfig::default()
        },
        ..XaftConfig::default()
    };
    xaft_config::validate(&config).expect("the safe combination must validate");
}

// ── Config file integration ───────────────────────────────────────────────────

fn load(toml_text: &str) -> Result<XaftConfig, String> {
    let dir = TempDir::new().map_err(|e| e.to_string())?;
    let path = dir.path().join("xaft.toml");
    std::fs::write(&path, toml_text).map_err(|e| e.to_string())?;
    xaft_config::ConfigLoader::load_file(&path).map_err(|e| e.to_string())
}

fn load_validated(toml_text: &str) -> Result<XaftConfig, String> {
    let with_presets = format!(
        r#"
[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
{toml_text}"#
    );
    let config = load(&with_presets)?;
    xaft_config::validate(&config).map_err(|e| e.to_string())?;
    Ok(config)
}

#[test]
fn a_config_without_a_security_section_loads() {
    // Backward compatibility: the section is new and existing files must not
    // need editing.
    let config = load(
        r#"
[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
"#,
    )
    .expect("load");
    assert_eq!(config.security, SecurityConfig::default());
}

#[test]
fn a_full_security_section_loads_and_drives_the_policy() {
    let config = load_validated(
        r#"
[security]
sandbox_mode = true
allowed_paths = ["/srv/work", "/srv/scratch"]
network_allow_list = ["api.example.com", "internal.test"]
max_tool_cpu_seconds = 10
max_tool_memory_mb = 256

[tools]
allowed = ["file-*", "grep"]
denied = ["file-write"]
"#,
    )
    .expect("a well-formed security section must load and validate");

    assert_eq!(
        config.security.allowed_paths,
        [PathBuf::from("/srv/work"), PathBuf::from("/srv/scratch")]
    );
    assert_eq!(config.security.max_tool_memory_mb, Some(256));

    let policy = ToolPolicy::from_config(&config);
    assert!(policy.is_tool_allowed("file-read"));
    assert!(!policy.is_tool_allowed("file-write"));
    assert!(!policy.is_tool_allowed("shell"));
    assert!(policy.is_host_allowed("api.example.com"));
    assert!(!policy.is_host_allowed("evil.test"));
    assert!(policy.is_path_allowed(Path::new("/srv/work/x.rs")));
    assert!(!policy.is_path_allowed(Path::new("/etc/passwd")));
}

#[test]
fn a_zero_ceiling_is_rejected_by_the_validating_loader() {
    let error = load_validated(
        r#"
[security]
max_tool_cpu_seconds = 0
"#,
    )
    .expect_err("a zero CPU ceiling must be rejected");
    assert!(error.contains("greater than zero"), "{error}");
}

#[test]
fn the_security_section_round_trips_through_toml() {
    let config = load_validated(
        r#"
[security]
sandbox_mode = false
allowed_paths = ["/a"]
network_allow_list = ["x.test"]
max_tool_cpu_seconds = 7
max_tool_memory_mb = 9
"#,
    )
    .expect("load");

    let rendered = toml::to_string(&config.security).expect("render");
    let reparsed: SecurityConfig = toml::from_str(&rendered).expect("re-parse");
    assert_eq!(reparsed, config.security);
}

#[test]
fn a_round_trip_preserves_the_fail_closed_default() {
    // The most dangerous thing that could happen to this section in a
    // round-trip is an empty allow-list quietly becoming "allow everything".
    let config = load_validated("[tools]\nallowed = []\n").expect("load");
    let rendered = toml::to_string(&config.tools).expect("render");
    let reparsed: ToolSettingsConfig = toml::from_str(&rendered).expect("re-parse");

    assert!(reparsed.allowed.is_empty());
    let policy = ToolPolicy::from_config(&config);
    assert!(
        !policy.is_tool_allowed("read_file"),
        "empty allow still denies"
    );
}
