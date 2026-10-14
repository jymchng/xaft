//! Integration tests for the `[plugins]` policy and the plugin trust module.
//!
//! Parity targets in agenthicc's `config.py::PluginSettings`: `auto_trust`,
//! `auto_install`, `install_target`, `allowed_modules`, `timeout_seconds`,
//! `disabled`, `trust_file`, `audit_file`, `strict_cli_shadow`.
//!
//! The property that decides whether any of this is safe is **content
//! addressing**: trust is recorded for the exact bytes the user agreed to, so
//! editing a trusted plugin revokes it. A trust system keyed on path alone
//! would extend "I reviewed this" to whatever the file was replaced with, which
//! is the same class of defect as trusting a script because of where it lives.
//!
//! The second property is **precedence**: `disabled` is checked first, so
//! `auto_trust` cannot re-enable a plugin the user turned off.

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use xaft_config::{ConfigLoader, PluginConfig, XaftConfig};
use xaft_tools::{PluginTrust, TrustDecision, TrustManifest, TrustRecord, sha256_hex};

fn plugin_paths(tag: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("temp dir");
    let plugin = dir.path().join(format!("{tag}.xaft-plugin"));
    (dir, plugin)
}

fn write(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("write plugin");
}

fn policy(config: &PluginConfig) -> PluginTrust {
    PluginTrust::new(config)
}

// ── Defaults ──────────────────────────────────────────────────────────────────

#[test]
fn defaults_match_agenthicc_exactly() {
    let config = PluginConfig::default();
    assert!(!config.auto_trust);
    assert!(!config.auto_install);
    assert_eq!(config.install_target, "venv");
    assert!(config.allowed_modules.is_empty());
    assert_eq!(config.timeout_seconds, 30.0);
    assert!(config.disabled.is_empty());
    assert_eq!(
        config.trust_file,
        PathBuf::from(".agenthicc/trusted_plugins.json")
    );
    assert_eq!(
        config.audit_file,
        PathBuf::from(".agenthicc/plugin_audit.jsonl")
    );
    assert!(!config.strict_cli_shadow);
}

#[test]
fn the_pre_existing_plugin_fields_are_untouched() {
    // The nine parity fields were folded into `PluginConfig`, which already
    // held four others. Assert those still round-trip, so adding them did not
    // displace anything.
    let config = PluginConfig::default();
    assert!(config.search_paths.is_empty());
    assert!(!config.allow_dynamic);
    assert!(!config.allow_wasm);
    assert!(!config.security.require_signature);
}

// ── Disabled ──────────────────────────────────────────────────────────────────

#[test]
fn a_disabled_plugin_is_skipped_even_with_auto_trust_on() {
    // The precedence that matters. `auto_trust` is documented as the CI escape
    // hatch; if it could override `disabled`, turning it on would silently
    // resurrect every plugin the user had turned off.
    let (_dir, plugin) = plugin_paths("disabled");
    write(&plugin, "code");
    let config = PluginConfig {
        disabled: vec!["disabled".to_string()],
        auto_trust: true,
        ..PluginConfig::default()
    };
    let decision = policy(&config).check(&plugin, b"code", &TrustManifest::default());
    assert_eq!(
        decision,
        TrustDecision::Skip,
        "auto_trust must not re-enable a disabled plugin"
    );
    assert!(!decision.allows_load());
}

#[test]
fn a_disabled_plugin_matches_on_its_file_stem() {
    let (_dir, plugin) = plugin_paths("myplugin");
    write(&plugin, "code");
    let config = PluginConfig {
        disabled: vec!["myplugin".to_string()],
        ..PluginConfig::default()
    };
    assert!(policy(&config).is_disabled(&plugin));
}

#[test]
fn a_disabled_plugin_matches_on_its_full_file_name() {
    let (_dir, plugin) = plugin_paths("myplugin");
    write(&plugin, "code");
    let config = PluginConfig {
        disabled: vec!["myplugin.xaft-plugin".to_string()],
        ..PluginConfig::default()
    };
    assert!(policy(&config).is_disabled(&plugin));
}

#[test]
fn a_plugin_not_on_the_list_is_not_disabled() {
    let (_dir, plugin) = plugin_paths("allowed");
    write(&plugin, "code");
    let config = PluginConfig {
        disabled: vec!["something-else".to_string()],
        ..PluginConfig::default()
    };
    assert!(!policy(&config).is_disabled(&plugin));
}

#[test]
fn an_empty_disabled_list_disables_nothing() {
    let (_dir, plugin) = plugin_paths("x");
    assert!(!policy(&PluginConfig::default()).is_disabled(&plugin));
}

// ── Allowed modules ───────────────────────────────────────────────────────────

#[test]
fn an_empty_module_list_allows_nothing() {
    // The fail-closed reading. The opposite -- empty meaning "everything" --
    // would hand every plugin the standard library and the network stack.
    let config = PluginConfig::default();
    let trust = policy(&config);
    assert!(!trust.is_module_allowed("json"));
    assert!(!trust.is_module_allowed("subprocess"));
    assert!(!trust.is_module_allowed(""));
}

#[test]
fn a_named_module_is_allowed_and_others_are_not() {
    let config = PluginConfig {
        allowed_modules: vec!["json".to_string(), "re".to_string()],
        ..PluginConfig::default()
    };
    let trust = policy(&config);
    assert!(trust.is_module_allowed("json"));
    assert!(trust.is_module_allowed("re"));
    assert!(!trust.is_module_allowed("subprocess"));
}

#[test]
fn module_matching_is_exact_not_prefix_or_substring() {
    let config = PluginConfig {
        allowed_modules: vec!["json".to_string()],
        ..PluginConfig::default()
    };
    let trust = policy(&config);
    assert!(!trust.is_module_allowed("json.decoder"));
    assert!(!trust.is_module_allowed("json5"));
    assert!(!trust.is_module_allowed("JSON"));
}

// ── Content-addressed trust ───────────────────────────────────────────────────

#[test]
fn an_unseen_plugin_is_skipped_by_default() {
    let (_dir, plugin) = plugin_paths("unseen");
    write(&plugin, "print(1)");
    let decision =
        policy(&PluginConfig::default()).check(&plugin, b"print(1)", &TrustManifest::default());
    assert_eq!(decision, TrustDecision::Skip);
    assert!(!decision.allows_load());
}

#[test]
fn a_plugin_matching_its_recorded_hash_is_trusted_once() {
    let (_dir, plugin) = plugin_paths("recorded");
    let contents = b"print(1)";
    write(&plugin, "print(1)");

    let mut manifest = TrustManifest::default();
    manifest.trusted.insert(
        plugin.display().to_string(),
        TrustRecord {
            sha256: sha256_hex(contents),
            ..TrustRecord::default()
        },
    );

    let decision = policy(&PluginConfig::default()).check(&plugin, contents, &manifest);
    assert_eq!(decision, TrustDecision::TrustOnce);
    assert!(decision.allows_load());
}

#[test]
fn editing_a_trusted_plugin_revokes_its_trust() {
    // The property the whole trust file rests on.
    let (_dir, plugin) = plugin_paths("edited");
    write(&plugin, "print(1)");
    let mut manifest = TrustManifest::default();
    manifest.trusted.insert(
        plugin.display().to_string(),
        TrustRecord {
            sha256: sha256_hex(b"print(1)"),
            ..TrustRecord::default()
        },
    );

    // The file on disk now says something else.
    let decision =
        policy(&PluginConfig::default()).check(&plugin, b"import os; os.system('x')", &manifest);
    assert_eq!(
        decision,
        TrustDecision::Skip,
        "trust must not survive a content change"
    );
}

#[test]
fn a_record_entry_for_a_different_path_does_not_grant_trust() {
    // Trust is keyed by path *and* hash, so a record for another plugin must
    // not authorize this one even if the bytes happen to match.
    let (_dir, plugin) = plugin_paths("this-one");
    write(&plugin, "code");
    let mut manifest = TrustManifest::default();
    manifest.trusted.insert(
        "/somewhere/else.xaft-plugin".to_string(),
        TrustRecord {
            sha256: sha256_hex(b"code"),
            ..TrustRecord::default()
        },
    );
    let decision = policy(&PluginConfig::default()).check(&plugin, b"code", &manifest);
    assert_eq!(decision, TrustDecision::Skip);
}

// ── auto_trust ────────────────────────────────────────────────────────────────

#[test]
fn auto_trust_admits_an_unseen_plugin_and_records_it() {
    let (_dir, plugin) = plugin_paths("autotrust");
    write(&plugin, "code");
    let config = PluginConfig {
        auto_trust: true,
        ..PluginConfig::default()
    };
    let trust = policy(&config);
    let mut manifest = TrustManifest::default();

    let decision = trust.check(&plugin, b"code", &manifest);
    assert_eq!(decision, TrustDecision::AlwaysTrust);
    assert!(decision.allows_load());

    let record = trust
        .record_for(b"code", decision, "2026-01-01T00:00:00Z")
        .expect("always_trust is recorded");
    assert_eq!(record.sha256, sha256_hex(b"code"));
    assert_eq!(record.decision, "always_trust");

    assert!(trust.apply(&mut manifest, &plugin, record));
    assert_eq!(manifest.version, 1);
    assert!(
        manifest
            .trusted
            .get(&plugin.display().to_string())
            .is_some_and(|r| r.sha256 == sha256_hex(b"code"))
    );

    // A second check now short-circuits on the recorded hash.
    assert_eq!(
        trust.check(&plugin, b"code", &manifest),
        TrustDecision::TrustOnce
    );
}

#[test]
fn a_skip_is_never_recorded() {
    let (_dir, plugin) = plugin_paths("skip");
    write(&plugin, "code");
    let trust = policy(&PluginConfig::default());
    assert!(
        trust
            .record_for(b"code", TrustDecision::Skip, "2026-01-01T00:00:00Z")
            .is_none(),
        "a plain skip must not rewrite the trust file"
    );
}

#[test]
fn auto_trust_records_a_different_hash_after_an_edit() {
    let (_dir, plugin) = plugin_paths("edited-autotrust");
    write(&plugin, "v2");
    let trust = policy(&PluginConfig {
        auto_trust: true,
        ..PluginConfig::default()
    });
    let mut manifest = TrustManifest::default();
    manifest.trusted.insert(
        plugin.display().to_string(),
        TrustRecord {
            sha256: sha256_hex(b"v1"),
            ..TrustRecord::default()
        },
    );

    assert_eq!(
        trust.check(&plugin, b"v2", &manifest),
        TrustDecision::AlwaysTrust
    );
    let record = trust
        .record_for(b"v2", TrustDecision::AlwaysTrust, "2026-01-01T00:00:00Z")
        .unwrap();
    assert!(trust.apply(&mut manifest, &plugin, record));
    assert_eq!(
        manifest.trusted[&plugin.display().to_string()].sha256,
        sha256_hex(b"v2")
    );
}

#[test]
fn applying_an_unchanged_record_reports_no_change() {
    let (_dir, plugin) = plugin_paths("unchanged");
    write(&plugin, "code");
    let trust = policy(&PluginConfig::default());
    let record = TrustRecord {
        sha256: sha256_hex(b"code"),
        trusted_at: "2026-01-01T00:00:00Z".to_string(),
        decision: "always_trust".to_string(),
    };
    let mut manifest = TrustManifest::default();
    assert!(trust.apply(&mut manifest, &plugin, record.clone()));
    assert!(
        !trust.apply(&mut manifest, &plugin, record),
        "an identical record must not dirty the file"
    );
}

// ── Audit journal ─────────────────────────────────────────────────────────────

#[test]
fn an_audit_record_captures_the_decision_and_the_hash() {
    let (_dir, plugin) = plugin_paths("audited");
    write(&plugin, "code");
    let trust = policy(&PluginConfig::default());
    let record = trust.audit_record(
        &plugin,
        b"code",
        TrustDecision::Skip,
        "2026-01-01T00:00:00Z",
    );
    assert_eq!(record.decision, "skip");
    assert_eq!(record.sha256, sha256_hex(b"code"));
    assert_eq!(record.at, "2026-01-01T00:00:00Z");
    assert!(record.plugin.ends_with("audited.xaft-plugin"));
}

#[test]
fn audit_lines_are_one_json_object_each() {
    // JSONL, so a reader can stream a long history without parsing it whole.
    let trust = policy(&PluginConfig::default());
    let mut text = String::new();
    for i in 0..3 {
        let record = trust.audit_record(
            Path::new("/p/x.xaft-plugin"),
            b"code",
            TrustDecision::Skip,
            "2026-01-01T00:00:00Z",
        );
        text.push_str(&serde_json::to_string(&record).expect("serialize"));
        text.push('\n');
        let _ = i;
    }
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    for line in lines {
        serde_json::from_str::<xaft_tools::AuditRecord>(line).expect("each line parses alone");
    }
}

#[test]
fn decision_names_are_stable() {
    assert_eq!(TrustDecision::TrustOnce.as_str(), "trust_once");
    assert_eq!(TrustDecision::AlwaysTrust.as_str(), "always_trust");
    assert_eq!(TrustDecision::Skip.as_str(), "skip");
}

// ── Trust file parsing ────────────────────────────────────────────────────────

#[test]
fn a_corrupt_trust_file_reads_as_nothing_trusted() {
    // Fail-closed, not fail-crash: "nothing is trusted" is the correct state
    // for a file xaft cannot parse, and erroring out would leave the user
    // unable to load anything without hand-editing JSON.
    assert!(TrustManifest::parse("not json at all").trusted.is_empty());
    assert!(TrustManifest::parse("").trusted.is_empty());
    assert!(TrustManifest::parse("[]").trusted.is_empty());
}

#[test]
fn a_well_formed_trust_file_round_trips() {
    let manifest = TrustManifest {
        version: 1,
        trusted: std::collections::BTreeMap::from([(
            "/p/x.xaft-plugin".to_string(),
            TrustRecord {
                sha256: sha256_hex(b"code"),
                trusted_at: "2026-01-01T00:00:00Z".to_string(),
                decision: "always_trust".to_string(),
            },
        )]),
    };
    let text = serde_json::to_string(&manifest).expect("serialize");
    let reparsed = TrustManifest::parse(&text);
    assert_eq!(reparsed.version, 1);
    assert_eq!(reparsed.trusted.len(), 1);
    assert_eq!(
        reparsed.trusted["/p/x.xaft-plugin"].sha256,
        sha256_hex(b"code")
    );
}

#[test]
fn a_trust_file_missing_optional_keys_still_parses() {
    let parsed = TrustManifest::parse(r#"{"trusted":{"/p/x":{}}}"#);
    assert!(parsed.trusted.contains_key("/p/x"));
    assert_eq!(parsed.version, 0);
}

// ── Hashing ───────────────────────────────────────────────────────────────────

#[test]
fn the_digest_is_the_standard_sha256() {
    // Pinned against the published vector so the trust file stays compatible
    // with anything else that computes the same hash over these files.
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn a_single_byte_change_changes_the_digest() {
    assert_ne!(sha256_hex(b"code"), sha256_hex(b"codf"));
    assert_ne!(sha256_hex(b"code\n"), sha256_hex(b"code"));
}

// ── Config file integration ───────────────────────────────────────────────────

fn load(toml_text: &str) -> Result<XaftConfig, String> {
    let dir = TempDir::new().map_err(|e| e.to_string())?;
    let path = dir.path().join("xaft.toml");
    std::fs::write(&path, toml_text).map_err(|e| e.to_string())?;
    ConfigLoader::load_file(&path).map_err(|e| e.to_string())
}

#[test]
fn a_config_without_a_plugins_section_loads() {
    let config = load("[core]\nlog_level = \"info\"\n").expect("load");
    assert_eq!(config.plugins, PluginConfig::default());
}

#[test]
fn a_full_plugins_section_loads_and_drives_the_policy() {
    let config = load(
        r#"
[plugins]
auto_trust = true
auto_install = false
install_target = "system"
allowed_modules = ["json"]
timeout_seconds = 2.5
disabled = ["risky"]
trust_file = ".xaft/trusted.json"
audit_file = ".xaft/audit.jsonl"
strict_cli_shadow = true
"#,
    )
    .expect("load");

    assert!(config.plugins.auto_trust);
    assert_eq!(config.plugins.install_target, "system");
    assert_eq!(config.plugins.timeout_seconds, 2.5);
    assert!(config.plugins.strict_cli_shadow);
    assert_eq!(
        config.plugins.trust_file,
        PathBuf::from(".xaft/trusted.json")
    );

    let trust = PluginTrust::new(&config.plugins);
    assert_eq!(trust.timeout_seconds(), 2.5);
    assert_eq!(trust.trust_file(), Path::new(".xaft/trusted.json"));
    assert_eq!(trust.audit_file(), Path::new(".xaft/audit.jsonl"));
    assert!(trust.is_module_allowed("json"));
    assert!(trust.is_disabled(Path::new("/p/risky.xaft-plugin")));
}

#[test]
fn the_plugins_section_round_trips_through_toml() {
    let config = load(
        r#"
[plugins]
auto_trust = true
install_target = "system"
allowed_modules = ["json"]
timeout_seconds = 2.5
disabled = ["risky"]
trust_file = ".xaft/trusted.json"
audit_file = ".xaft/audit.jsonl"
strict_cli_shadow = true
"#,
    )
    .expect("load");

    let rendered = toml::to_string(&config.plugins).expect("render");
    let reparsed: PluginConfig = toml::from_str(&rendered).expect("re-parse");
    assert_eq!(reparsed.auto_trust, config.plugins.auto_trust);
    assert_eq!(reparsed.install_target, "system");
    assert_eq!(reparsed.allowed_modules, ["json"]);
    assert_eq!(reparsed.timeout_seconds, 2.5);
    assert_eq!(reparsed.disabled, ["risky"]);
    assert!(reparsed.strict_cli_shadow);
}

#[test]
fn unset_parity_keys_are_not_written_back_out() {
    // A round-tripped config must not claim defaults the user never set.
    let config = load("[core]\nlog_level = \"info\"\n").expect("load");
    let rendered = toml::to_string(&config.plugins).expect("render");
    for absent in [
        "auto_trust",
        "auto_install",
        "install_target",
        "allowed_modules",
        "timeout_seconds",
        "disabled",
        "trust_file",
        "audit_file",
        "strict_cli_shadow",
    ] {
        assert!(
            !rendered.contains(absent),
            "{absent} was never set and must not be written back: {rendered}"
        );
    }
}

#[test]
fn the_pre_existing_plugin_keys_still_round_trip_alongside_the_new_ones() {
    let config = load(
        r#"
[plugins]
allow_dynamic = true
search_paths = ["/opt/plugins"]

[plugins.security]
require_signature = true
unsigned_capabilities = ["read"]
"#,
    )
    .expect("load");

    assert!(config.plugins.allow_dynamic);
    assert_eq!(config.plugins.search_paths, [PathBuf::from("/opt/plugins")]);
    assert!(config.plugins.security.require_signature);
    assert_eq!(config.plugins.security.unsigned_capabilities, ["read"]);

    let rendered = toml::to_string(&config.plugins).expect("render");
    assert!(rendered.contains("allow_dynamic"), "{rendered}");
    assert!(rendered.contains("require_signature"), "{rendered}");
}
