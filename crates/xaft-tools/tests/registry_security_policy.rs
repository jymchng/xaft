//! Integration tests for `ToolRegistryBuilder`'s security-policy filter.
//!
//! The point of these tests is reachability: `tools.allowed`, `tools.denied`
//! and `[security]` are only parity if something *uses* them. The
//! `config-key-audit` / `parity-row-reachability` pair scores a declared-but-
//! never-read field as ❌ for exactly this reason, so these tests drive the
//! builder and assert on the registry it hands back.
//!
//! The behaviour that matters is that a denied tool is **not registered at
//! all**, rather than registered and refusing at call time. An agent that is
//! offered a tool it cannot use spends a turn discovering that, and an offered
//! destructive tool is an invitation the model may well take.

use std::path::PathBuf;

use xaft_config::{SecurityConfig, ToolPolicy, XaftConfig};
use xaft_tools::registry::ToolRegistryBuilder;

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xaft-policy-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// A policy that allows everything, so a test can vary one rule at a time.
fn open_policy(allowed: &[&str], denied: &[&str]) -> ToolPolicy {
    let config = XaftConfig {
        security: SecurityConfig {
            sandbox_mode: false,
            ..SecurityConfig::default()
        },
        tools: xaft_config::ToolSettingsConfig {
            allowed: allowed.iter().map(|s| s.to_string()).collect(),
            denied: denied.iter().map(|s| s.to_string()).collect(),
            ..xaft_config::ToolSettingsConfig::default()
        },
        ..XaftConfig::default()
    };
    ToolPolicy::from_config(&config)
}

fn reader(tag: &str, policy: ToolPolicy) -> xaft_tools::registry::ToolRegistry {
    ToolRegistryBuilder::new(temp_root(tag))
        .with_security_policy(policy)
        .without_git()
        .build_reader()
        .expect("reader registry builds")
}

#[test]
fn without_a_policy_the_registry_is_unfiltered() {
    // The default has to leave an existing caller's registry alone, otherwise
    // adding this feature would silently empty everyone's tool list.
    let registry = ToolRegistryBuilder::new(temp_root("unfiltered"))
        .without_git()
        .build_reader()
        .expect("reader registry builds");
    let names = registry.names();
    assert!(names.contains(&"read_file".to_string()), "{names:?}");
    assert!(names.contains(&"list_files".to_string()), "{names:?}");
    assert!(names.contains(&"grep".to_string()), "{names:?}");
}

#[test]
fn a_permissive_policy_keeps_the_reader_tools() {
    let registry = reader("permissive", open_policy(&["*"], &[]));
    let names = registry.names();
    assert!(names.contains(&"read_file".to_string()));
    assert!(names.contains(&"list_files".to_string()));
    assert!(names.contains(&"grep".to_string()));
}

#[test]
fn an_empty_allow_list_yields_an_empty_registry() {
    // Fail-closed, observed at the one place it matters. A config with no
    // allow rules must not hand the agent every tool in the registry.
    let registry = reader("empty-allow", open_policy(&[], &[]));
    assert!(
        registry.is_empty(),
        "an empty allow-list must register nothing, got {:?}",
        registry.names()
    );
}

#[test]
fn a_narrow_allow_list_yields_only_those_tools() {
    let registry = reader("narrow", open_policy(&["read_file"], &[]));
    assert_eq!(registry.names(), ["read_file"]);
}

#[test]
fn a_multi_tool_allow_list_keeps_registration_order() {
    // The order is the registry's, not the allow-list's: `build_reader`
    // registers list_files before read_file before grep, and the filter walks
    // that order. Asserted against registration order rather than the order the
    // patterns happen to be written in, because those are different things and
    // conflating them would make this test pass for the wrong reason.
    let registry = reader("multi", open_policy(&["read_file", "list_files", "grep"], &[]));
    assert_eq!(registry.names(), ["list_files", "read_file", "grep"]);
}

#[test]
fn a_denied_tool_is_not_registered_at_all() {
    let registry = reader("denied", open_policy(&["*"], &["read_file"]));
    let names = registry.names();
    assert!(
        !names.contains(&"read_file".to_string()),
        "a denied tool must not be offered to the agent: {names:?}"
    );
    assert!(names.contains(&"list_files".to_string()), "{names:?}");
}

#[test]
fn deny_removes_a_whole_glob_family() {
    let registry = reader("deny-family", open_policy(&["*"], &["*write*"]));
    let names = registry.names();
    assert!(!names.iter().any(|n| n.contains("write")), "{names:?}");
    assert!(names.contains(&"read_file".to_string()));
}

#[test]
fn filtering_preserves_registration_order() {
    // Order is how the agent sees its tool list; a policy filter must not
    // reshuffle it into something the model reads differently.
    let permissive = reader("order-a", open_policy(&["*"], &[]));
    let restricted = reader("order-b", open_policy(&["*"], &["grep"]));

    let expected: Vec<String> = permissive
        .names()
        .into_iter()
        .filter(|n| n != "grep")
        .collect();
    assert_eq!(restricted.names(), expected);
}

#[test]
fn the_filter_applies_to_the_coder_registry_too() {
    // Deny the write family by its real shape -- `write_file`, `batch_write`,
    // `patch_file`, `append_to_file` -- rather than a `file-*` prefix that no
    // tool in this registry uses.
    let registry = ToolRegistryBuilder::new(temp_root("coder"))
        .with_write()
        .without_git()
        .with_security_policy(open_policy(&["*"], &["*write*", "patch_file"]))
        .build_coder()
        .expect("coder registry builds");

    let names = registry.names();
    assert!(!names.is_empty(), "readers must survive");
    assert!(
        !names.iter().any(|n| n.contains("write")),
        "the denied write family must be absent: {names:?}"
    );
    assert!(
        !names.contains(&"patch_file".to_string()),
        "the exactly-denied tool must be absent: {names:?}"
    );
    assert!(names.contains(&"read_file".to_string()), "{names:?}");
}

#[test]
fn a_denied_read_tool_leaves_the_write_tools_alone() {
    // Deny is per-pattern, not a mode: refusing one reader must not take the
    // writers with it.
    let registry = ToolRegistryBuilder::new(temp_root("targeted"))
        .with_write()
        .without_git()
        .with_security_policy(open_policy(&["*"], &["read_file"]))
        .build_coder()
        .expect("coder registry builds");

    let names = registry.names();
    assert!(!names.contains(&"read_file".to_string()), "{names:?}");
    assert!(names.contains(&"write_file".to_string()), "{names:?}");
}

#[test]
fn a_tool_kept_by_the_policy_is_still_callable() {
    // The filter must not corrupt the entries it keeps: a kept tool has to come
    // back out of `get`, not merely appear in `names`.
    let registry = reader("callable", open_policy(&["read_file"], &[]));
    assert!(registry.get("read_file").is_some());
    assert!(registry.get("write_file").is_none());
    assert_eq!(registry.len(), 1);
}

// ── The builder's config-driven entry points ─────────────────────────────────
//
// These are the functions a real caller uses, so they are what makes the
// `[security]` / `[tools]` keys reachable from outside xaft-config. Each one is
// driven from a parsed config file, not a hand-built struct, so a rename of a
// config key cannot silently leave the consumer reading a field that no longer
// exists.

use std::fs;
use xaft_config::ConfigLoader;

fn load_config(tag: &str, toml_text: &str) -> XaftConfig {
    let dir = temp_root(tag);
    let path = dir.join("xaft.toml");
    fs::write(&path, toml_text).expect("write config");
    ConfigLoader::load_file(&path).expect("config loads")
}

#[test]
fn the_builder_reads_allowed_denied_and_the_sandbox_switch_from_a_config_file() {
    let config = load_config(
        "from-file",
        r#"
[security]
sandbox_mode = true
allowed_paths = ["/srv/work"]
network_allow_list = ["api.example.com"]

[tools]
allowed = ["file-*", "grep"]
denied = ["file-write"]
"#,
    );

    use xaft_tools::registry::ToolRegistryBuilder;

    assert!(ToolRegistryBuilder::sandbox_enabled(&config));

    let policy = ToolRegistryBuilder::policy_from_config(&config);
    assert!(policy.is_tool_allowed("file-read"));
    assert!(!policy.is_tool_allowed("file-write"), "denied wins");
    assert!(!policy.is_tool_allowed("shell"), "unlisted is denied");

    assert_eq!(
        ToolRegistryBuilder::allowed_roots(&config),
        [PathBuf::from("/srv/work")]
    );
    assert_eq!(
        ToolRegistryBuilder::network_allow_list(&config),
        &vec!["api.example.com".to_string()]
    );
    assert!(ToolRegistryBuilder::is_host_allowed(&config, "api.example.com"));
    assert!(!ToolRegistryBuilder::is_host_allowed(&config, "notexample.com"));
}

#[test]
fn the_builder_reads_the_resource_ceilings_from_a_config_file() {
    let config = load_config(
        "limits",
        r#"
[security]
max_tool_cpu_seconds = 3
max_tool_memory_mb = 128
"#,
    );
    use xaft_tools::registry::ToolRegistryBuilder;

    let limits = ToolRegistryBuilder::resource_limits(&config);
    assert_eq!(limits.cpu_seconds, Some(3));
    assert_eq!(limits.memory_mb, Some(128));
    assert!(!limits.is_unbounded());
}

#[test]
fn a_zero_ceiling_is_parseable_but_rejected() {
    // TOML cannot spell `null`, so "no ceiling" and "the default" both come
    // from omitting the key -- which is a real limitation of the format, not of
    // this config. Zero is the only way to *ask* for no headroom, and
    // `validate` refuses it: a ceiling of zero would also kill a tool that does
    // nothing at all.
    let config = load_config("zero-limit", "[security]\nmax_tool_cpu_seconds = 0\n");
    use xaft_tools::registry::ToolRegistryBuilder;

    assert_eq!(
        ToolRegistryBuilder::resource_limits(&config).cpu_seconds,
        Some(0)
    );
    let error = xaft_config::validate(&config).expect_err("zero ceiling is rejected");
    assert!(error.to_string().contains("greater than zero"), "{error}");
}

#[test]
fn a_present_section_and_an_absent_one_agree_on_the_default_ceilings() {
    // The two spellings of "default" must not diverge: no `[security]` table at
    // all, and a `[security]` table that simply omits the ceilings, both have to
    // land on agenthicc's 30 / 512.
    let omitted = load_config("no-section", "[core]\nlog_level = \"info\"\n");
    let present = load_config(
        "empty-section",
        "[security]\nallowed_paths = [\"/srv/work\"]\n",
    );
    use xaft_tools::registry::ToolRegistryBuilder;

    for config in [&omitted, &present] {
        let limits = ToolRegistryBuilder::resource_limits(config);
        assert_eq!(limits.cpu_seconds, Some(30));
        assert_eq!(limits.memory_mb, Some(512));
        assert!(!limits.is_unbounded());
    }
}

#[test]
fn the_builder_checks_paths_against_the_configured_roots() {
    let config = load_config(
        "paths",
        r#"
[security]
allowed_paths = ["/srv/work"]
"#,
    );
    use xaft_tools::registry::ToolRegistryBuilder;

    assert!(ToolRegistryBuilder::is_path_allowed(&config, std::path::Path::new("/srv/work/a.rs")));
    assert!(!ToolRegistryBuilder::is_path_allowed(
        &config,
        std::path::Path::new("/srv/work-evil/a.rs")
    ));
}

#[test]
fn disabling_the_sandbox_in_a_config_opens_the_scope() {
    let config = load_config(
        "nosandbox",
        r#"
[security]
sandbox_mode = false
"#,
    );
    use xaft_tools::registry::ToolRegistryBuilder;

    assert!(!ToolRegistryBuilder::sandbox_enabled(&config));
    assert!(ToolRegistryBuilder::is_host_allowed(&config, "anything.test"));
    assert!(ToolRegistryBuilder::is_path_allowed(&config, std::path::Path::new("/etc/passwd")));
}
