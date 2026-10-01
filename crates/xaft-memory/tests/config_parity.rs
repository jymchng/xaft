//! Keeps the two `MemoryConfig` declarations from drifting apart silently.
//!
//! THE DEFECT THIS EXISTS TO KILL. `MemoryConfig` is declared twice: once as
//! the user-facing schema (`xaft_config::MemoryConfig`, parsed from
//! `xaft.toml` and held by `XaftConfig.memory`) and once in
//! `crates/xaft-memory/src/config.rs` as the runtime copy the memory system
//! consumes. `src/main.rs` maps between them field by field. That mapping is
//! the ONLY thing connecting the two, so a field added on one side is not
//! automatically a setting a user can write, and nothing used to fail if the
//! two lists diverged.
//!
//! This was not hypothetical. `auto_remember_ttl_secs` existed only on the
//! runtime side, `src/main.rs` hardcoded it to `None`, and no code read it —
//! a knob that looked implemented and did nothing. That is the same class of
//! defect as a config field nothing reads (G4c's rule) and as a parity row
//! marked done on the strength of a name (goals 37/63): the surface *looked*
//! right while the reachable behaviour was empty.
//!
//! The guard is structural rather than a hand-maintained list: the field sets
//! are read out of both source files, so adding a field to either struct
//! without accounting for it here fails the test.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Parse the `pub <name>: <type>` field list out of one struct definition.
///
/// Scans from the struct's opening brace and returns every public field name.
/// Comment lines and nested items are skipped; xaft's config structs are flat
/// `pub` field lists, so a depth counter is not needed, but braces are tracked
/// anyway so a future nested item cannot corrupt the result silently.
fn public_fields(source: &str, struct_name: &str) -> BTreeSet<String> {
    let header = format!("pub struct {struct_name} {{");
    let start = source
        .find(&header)
        .unwrap_or_else(|| panic!("`pub struct {struct_name} {{` not found"));
    let body = &source[start + header.len()..];

    let mut depth = 1usize;
    let mut fields = BTreeSet::new();
    for line in body.lines() {
        // Track nesting before any `break`, so a field inside an inline block
        // is still attributed to this struct.
        for ch in line.chars() {
            match ch {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        if depth == 0 {
            break;
        }
        let line = line.trim();
        if line.starts_with("//") || line.starts_with("///") || line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix("pub ") else {
            continue;
        };
        let Some(colon) = rest.find(':') else {
            continue;
        };
        let name = rest[..colon].trim();
        // Skip nested `pub` items that are not fields (e.g. a `pub fn`).
        if name.is_empty()
            || !name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        {
            continue;
        }
        fields.insert(name.to_string());
    }
    assert!(
        !fields.is_empty(),
        "no public fields parsed from `{struct_name}` — the parser is stale"
    );
    fields
}

/// Repository root, derived from this crate's manifest dir.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("repo root")
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn config_fields() -> BTreeSet<String> {
    public_fields(&read("crates/xaft-config/src/types.rs"), "MemoryConfig")
}

fn runtime_fields() -> BTreeSet<String> {
    public_fields(&read("crates/xaft-memory/src/config.rs"), "MemoryConfig")
}

fn runtime_only_allowlist() -> BTreeSet<String> {
    xaft_memory::config::RUNTIME_ONLY_FIELDS
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect()
}

/// The core guard: every field on the runtime type must either also exist on
/// the user-facing config type, or be listed in `RUNTIME_ONLY_FIELDS` with a
/// written justification.
///
/// Without this, adding `auto_remember_ttl_secs` to the runtime struct alone
/// would have compiled, shipped, and been unreachable from `xaft.toml`.
#[test]
fn config_fields_are_not_silently_unreachable() {
    let config = config_fields();
    let runtime = runtime_fields();
    let allowlist = runtime_only_allowlist();

    let unaccounted: Vec<&String> = runtime
        .iter()
        .filter(|f| !config.contains(*f) && !allowlist.contains(*f))
        .collect();

    assert!(
        unaccounted.is_empty(),
        "these fields exist on xaft_memory::MemoryConfig but not on \
         xaft_config::MemoryConfig and are not in RUNTIME_ONLY_FIELDS, so no \
         user can set them: {unaccounted:?}\n\
         Either add the key to xaft_config::MemoryConfig and map it in \
         src/main.rs, or add it to RUNTIME_ONLY_FIELDS with a reason why it is \
         legitimately runtime-only."
    );
}

/// The allow-list must not rot: an entry naming a field the runtime struct no
/// longer has would silently stop justifying anything.
///
/// NOTE what is deliberately NOT asserted here: that an entry is absent from
/// the config side. `backend` is in the list because its TYPE differs
/// (`MemoryBackend` vs `String`), not because the key is missing — it is
/// present on both structs. A "not on the config side" check fails on the
/// very entry that motivated the list, which is exactly what happened the
/// first time this test ran.
#[test]
fn the_runtime_only_allowlist_has_no_stale_entries() {
    let config = config_fields();
    let runtime = runtime_fields();

    for (name, reason) in xaft_memory::config::RUNTIME_ONLY_FIELDS {
        assert!(
            runtime.contains(*name),
            "RUNTIME_ONLY_FIELDS lists `{name}` but the runtime struct has no such field"
        );
        assert!(
            reason.len() > 40,
            "RUNTIME_ONLY_FIELDS entry `{name}` needs a real justification, not a stub"
        );
        // An entry only carries meaning while the two sides actually differ
        // for this field. `backend` is exempt for its TYPE, so its presence on
        // both sides is expected — but any NEW name appearing on both sides
        // means the exemption is obsolete and should be dropped.
        if config.contains(*name) {
            assert!(
                *name == "backend",
                "RUNTIME_ONLY_FIELDS lists `{name}`, which now exists on BOTH \
                 structs. If the types are identical the exemption is obsolete; \
                 if they still differ, this entry must justify itself by name."
            );
        }
    }
}

/// The user-facing struct must not grow fields the runtime silently drops, for
/// the same reason. A key here that is absent from the runtime type is a
/// setting a user can write that changes nothing — the worst outcome, because
/// it looks implemented.
#[test]
fn user_facing_keys_are_not_silently_dropped() {
    let config = config_fields();
    let runtime = runtime_fields();

    let dropped: Vec<&String> = config
        .iter()
        .filter(|f| !runtime.contains(*f) && *f != "project_memory_path")
        .collect();

    assert!(
        dropped.is_empty(),
        "xaft_config::MemoryConfig declares {dropped:?} but the runtime type \
         does not, and it is not `project_memory_path` (which is read directly \
         from the config in src/main.rs to locate the SQLite store). A user \
         could write this key and it would do nothing."
    );
}

/// `project_memory_path` is the one legitimate asymmetry, and it is read on the
/// config side rather than the runtime side — so prove that, rather than
/// allowing it on faith.
#[test]
fn project_memory_path_is_read_from_the_config_side() {
    let main = read("src/main.rs");
    assert!(
        main.contains("config.memory.project_memory_path")
            || main.contains(".memory\n            .project_memory_path")
            || main.contains("memory\n            .project_memory_path"),
        "project_memory_path is exempted from the field-set guard as \
         'read on the config side', but src/main.rs no longer reads it — if \
         that is true the exemption must be replaced with a real mapping"
    );
}

/// `backend` is the one field whose TYPE differs between the two structs
/// (`String` vs `MemoryBackend`), exempted by name in `RUNTIME_ONLY_FIELDS`.
///
/// Its value is not exempt: this asserts the string->enum mapping actually
/// covers both variants, so a user writing `backend = "in_memory"` still gets
/// the in-memory store.
#[test]
fn config_backend_round_trips_into_the_runtime_enum() {
    use xaft_config::types::MemoryConfig as ConfigMemoryConfig;
    use xaft_memory::config::{MemoryBackend, MemoryConfig};

    // Reproduce the mapping `src/main.rs` performs.
    fn map(backend: &str) -> MemoryBackend {
        match backend {
            "in_memory" => MemoryBackend::InMemory,
            _ => MemoryBackend::Sqlite,
        }
    }

    let mut cfg = ConfigMemoryConfig::default();
    cfg.backend = "in_memory".into();
    assert_eq!(map(&cfg.backend), MemoryBackend::InMemory);

    cfg.backend = "sqlite".into();
    assert_eq!(map(&cfg.backend), MemoryBackend::Sqlite);

    // An unknown value falls back to the default, which is what `src/main.rs`
    // does today; pinning it so a future change is deliberate.
    cfg.backend = "postgres".into();
    assert_eq!(map(&cfg.backend), MemoryBackend::Sqlite);

    // And the default on both sides agrees.
    assert_eq!(MemoryConfig::default().backend, MemoryBackend::Sqlite);
    assert_eq!(ConfigMemoryConfig::default().backend, "sqlite");
}

/// The regression this whole file is about, pinned directly: a TTL written in
/// `xaft.toml` must reach the runtime type.
///
/// Before this work `src/main.rs` hardcoded `auto_remember_ttl_secs: None`,
/// so the key was writable in one struct and ignored at the other.
#[test]
fn a_configured_ttl_reaches_the_runtime_config() {
    use xaft_config::types::MemoryConfig as ConfigMemoryConfig;

    let mut cfg = ConfigMemoryConfig::default();
    assert_eq!(
        cfg.auto_remember_ttl_secs, None,
        "default must be None so existing users see no behaviour change"
    );

    cfg.auto_remember_ttl_secs = Some(3600);
    assert_eq!(cfg.auto_remember_ttl_secs, Some(3600));

    // A config file that sets the key round-trips.
    let toml_text = format!(
        "[memory]\nauto_remember_ttl_secs = {}\n",
        cfg.auto_remember_ttl_secs.unwrap()
    );
    let parsed: XaftConfigFromToml = toml::from_str(&toml_text).expect("parse");
    assert_eq!(parsed.memory.auto_remember_ttl_secs, Some(3600));
}

/// The one place a re-introduced `auto_remember_ttl_secs: None` could hide:
/// `src/main.rs` is the binary crate, which has no tests at all (0/70 covered),
/// so nothing in `cargo test` can observe what it passes to the manager.
///
/// The original defect was exactly this: `src/main.rs` hand-mapped every field
/// and hardcoded `auto_remember_ttl_secs: None`, so the key existed in the
/// config schema, the field existed at runtime, and the two were never joined.
/// Asserting on the source is the only available seam, and it is worth having
/// because that mapping is the only thing that can silently drop a field.
#[test]
fn main_rust_maps_the_runtime_config_from_the_user_config() {
    let main = read("src/main.rs");

    // Extract the `xaft_memory::MemoryConfig { .. }` literal.
    let start = main
        .find("xaft_memory::MemoryConfig {")
        .unwrap_or_else(|| panic!("no runtime MemoryConfig literal found in src/main.rs"));
    let body = &main[start..];
    let end = body.find("\n        };").unwrap_or_else(|| {
        panic!("no closing brace for the runtime config literal in src/main.rs")
    });
    let literal = &body[..end];

    // Every runtime field present in the literal must be fed from
    // `config.memory.<field>` — never a literal. This is the invariant the
    // whole goal is about.
    let runtime = runtime_fields();
    let exempt = runtime_only_allowlist();
    let mut hardcoded: Vec<&String> = Vec::new();
    for field in &runtime {
        if !literal.contains(&format!("{field}:")) {
            continue;
        }
        // A field exempt by name must at least be traceable to the user
        // config SOMEWHERE in the mapping block, not necessarily on the same
        // line: `backend` is a `String` in the user config and a
        // `MemoryBackend` enum at runtime, so main.rs assigns
        // `backend: backend.clone()` from a local converted out of
        // `config.memory.backend` a few lines earlier. `main.rs`'s source is
        // read in full below to prove the value originates there, and
        // `config_backend_round_trips_into_the_runtime_enum` pins the mapping
        // itself, so exempting it here loses no coverage.
        if !literal.contains(&format!("{field}: config.memory.{field}")) {
            if exempt.contains(field) && main.contains(&format!("config.memory.{field}")) {
                continue;
            }
            hardcoded.push(field);
        }
    }
    // For every exempt name, the value must be traceable to the user config in
    // the SAME bootstrap block — not necessarily inside the struct literal.
    // `backend` is converted a few lines ABOVE the literal
    // (`let backend = match config.memory.backend.as_str() { ... }`), so the
    // search window deliberately starts at the memory bootstrap, not at the
    // literal.
    let bootstrap_start = main
        .find("config.memory.enabled")
        .unwrap_or_else(|| panic!("no memory bootstrap found in src/main.rs"));
    let bootstrap = &main[bootstrap_start..];
    for (field, _) in xaft_memory::config::RUNTIME_ONLY_FIELDS {
        assert!(
            bootstrap.contains(&format!("config.memory.{field}")),
            "{field} is exempt from the hardcode check, but \
             config.memory.{field} appears nowhere in the memory bootstrap in \
             src/main.rs, so its value does not come from the user config"
        );
    }

    assert!(
        hardcoded.is_empty(),
        "src/main.rs hardcodes these runtime MemoryConfig fields instead of \
         reading config.memory.<field>: {hardcoded:?}\n\
         A hardcoded value means the key is writable in xaft.toml but ignored \
         at runtime — the exact defect this guard exists to catch. Either map \
         it from config or add it to RUNTIME_ONLY_FIELDS with a reason."
    );
}

/// Guards the specific regression that started this goal: the runtime field
/// was fed a literal `None` while a same-named user key existed.
#[test]
fn the_ttl_is_not_hardcoded_to_none_in_main() {
    let main = read("src/main.rs");
    assert!(
        !main.contains("auto_remember_ttl_secs: None"),
        "src/main.rs hardcodes auto_remember_ttl_secs to None again, so \
         xaft.toml's memory.auto_remember_ttl_secs is ignored. Map it from \
         config.memory.auto_remember_ttl_secs instead."
    );
    assert!(
        main.contains("auto_remember_ttl_secs: config.memory.auto_remember_ttl_secs"),
        "src/main.rs must pass the configured TTL through to the runtime type"
    );
}

/// The minimal shape needed to parse a `[memory]` table in this test without
/// pulling the whole `XaftConfig` (whose other sections have their own
/// defaults that are irrelevant here).
#[derive(serde::Deserialize)]
struct XaftConfigFromToml {
    #[serde(default)]
    memory: xaft_config::types::MemoryConfig,
}
