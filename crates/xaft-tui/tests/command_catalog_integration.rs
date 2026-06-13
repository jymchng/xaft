//! Integration tests for `CommandCatalog` (PRD-60 Feature A).
//!
//! These tests build a `CommandCatalog` from the real `COMMAND_TABLE` and verify
//! grouping, search, and dynamic registration behaviour.

use std::time::Instant;

use xaft_tui::trigger::catalog::{CommandCatalog, CommandEntry, CommandGroup, CommandSource};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn dynamic_entry(name: &str, group: CommandGroup) -> CommandEntry {
    CommandEntry {
        name: name.to_string(),
        aliases: Vec::new(),
        description: format!("Dynamic: {name}"),
        args_hint: None,
        group,
        source: CommandSource::Dynamic {
            registered_at: Instant::now(),
        },
    }
}

fn skill_entry(name: &str, skill: &str) -> CommandEntry {
    CommandEntry {
        name: name.to_string(),
        aliases: Vec::new(),
        description: format!("Skill command: {name}"),
        args_hint: None,
        group: CommandGroup::Skills,
        source: CommandSource::Skill {
            skill_name: skill.to_string(),
        },
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn catalog_built_from_command_table_has_expected_groups() {
    let catalog = CommandCatalog::from_command_table();

    // Navigation group
    for name in &["clear", "compact", "resume", "rewind", "quit"] {
        let entry = catalog
            .get(name)
            .unwrap_or_else(|| panic!("missing /{name}"));
        assert_eq!(
            entry.group,
            CommandGroup::Navigation,
            "/{name} must be in Navigation group"
        );
    }

    // Agent group
    for name in &["agents", "model", "cost"] {
        let entry = catalog
            .get(name)
            .unwrap_or_else(|| panic!("missing /{name}"));
        assert_eq!(
            entry.group,
            CommandGroup::Agent,
            "/{name} must be in Agent group"
        );
    }

    // Git group
    for name in &["diff", "commit", "pr"] {
        let entry = catalog
            .get(name)
            .unwrap_or_else(|| panic!("missing /{name}"));
        assert_eq!(
            entry.group,
            CommandGroup::Git,
            "/{name} must be in Git group"
        );
    }

    // Tools group
    for name in &["mcp", "permissions", "doctor", "init"] {
        let entry = catalog
            .get(name)
            .unwrap_or_else(|| panic!("missing /{name}"));
        assert_eq!(
            entry.group,
            CommandGroup::Tools,
            "/{name} must be in Tools group"
        );
    }

    // Other group
    for name in &[
        "help", "config", "theme", "vim", "emacs", "login", "logout", "memory",
    ] {
        let entry = catalog
            .get(name)
            .unwrap_or_else(|| panic!("missing /{name}"));
        assert_eq!(
            entry.group,
            CommandGroup::Other,
            "/{name} must be in Other group"
        );
    }
}

#[test]
fn catalog_grouped_search_with_prefix_co() {
    let catalog = CommandCatalog::from_command_table();
    let groups = catalog.grouped_search("co");

    // All returned entries must start with "co".
    for (_, entries) in &groups {
        for e in entries {
            assert!(
                e.name.starts_with("co") || e.aliases.iter().any(|a| a.starts_with("co")),
                "entry '{}' does not match prefix 'co'",
                e.name
            );
        }
    }

    // Must include compact, config, commit, cost.
    let all_names: Vec<&str> = groups
        .iter()
        .flat_map(|(_, v)| v.iter().map(|e| e.name.as_str()))
        .collect();
    for expected in &["compact", "config", "commit", "cost"] {
        assert!(
            all_names.contains(expected),
            "grouped_search('co') must contain '{expected}', got: {all_names:?}"
        );
    }

    // Must NOT include commands that don't start with "co".
    assert!(
        !all_names.contains(&"help"),
        "grouped_search('co') must not contain 'help'"
    );
}

#[test]
fn catalog_recently_used_appears_in_recent_group() {
    let mut catalog = CommandCatalog::from_command_table();
    catalog.record_used("compact");
    catalog.record_used("cost");

    let groups = catalog.grouped_search("");
    assert_eq!(
        groups[0].0, "Recent",
        "first group must be 'Recent' when there are recently-used commands"
    );

    let recent_names: Vec<&str> = groups[0].1.iter().map(|e| e.name.as_str()).collect();
    assert!(
        recent_names.contains(&"compact"),
        "compact must be in Recent"
    );
    assert!(recent_names.contains(&"cost"), "cost must be in Recent");
}

#[test]
fn catalog_register_dynamic_appears_in_dynamic_group() {
    let mut catalog = CommandCatalog::from_command_table();
    let entry = dynamic_entry("deploy", CommandGroup::Dynamic);
    catalog.register_dynamic(entry);

    // Must appear in search.
    let results = catalog.search("dep");
    assert!(
        results.iter().any(|e| e.name == "deploy"),
        "search('dep') must find 'deploy'"
    );

    // Must appear in grouped_search under "Dynamic".
    let groups = catalog.grouped_search("");
    let dynamic_group = groups
        .iter()
        .find(|(label, _)| *label == "Dynamic")
        .expect("Dynamic group must appear");
    assert!(
        dynamic_group.1.iter().any(|e| e.name == "deploy"),
        "'deploy' must be in Dynamic group"
    );
}

#[test]
fn catalog_skill_entry_appears_in_skills_group() {
    let mut catalog = CommandCatalog::from_command_table();
    catalog.register_dynamic(skill_entry("run-tests", "test-runner"));

    let groups = catalog.grouped_search("");
    let skills_group = groups
        .iter()
        .find(|(label, _)| *label == "Skills")
        .expect("Skills group must appear");
    assert!(
        skills_group.1.iter().any(|e| e.name == "run-tests"),
        "'run-tests' must appear in Skills group"
    );
}

#[test]
fn catalog_skills_sort_before_dynamic_in_palette() {
    let mut catalog = CommandCatalog::from_command_table();
    catalog.register_dynamic(dynamic_entry("my-dyn", CommandGroup::Dynamic));
    catalog.register_dynamic(skill_entry("my-skill-cmd", "my-skill"));

    let groups = catalog.grouped_search("");
    let skills_pos = groups.iter().position(|(l, _)| *l == "Skills");
    let dynamic_pos = groups.iter().position(|(l, _)| *l == "Dynamic");

    if let (Some(s), Some(d)) = (skills_pos, dynamic_pos) {
        assert!(s < d, "Skills group must appear before Dynamic group");
    }
}

#[test]
fn catalog_alias_lookup_works() {
    let catalog = CommandCatalog::from_command_table();
    // "ctx" is an alias for "compact".
    let entry = catalog.get("ctx").expect("alias 'ctx' must be found");
    assert_eq!(
        entry.name, "compact",
        "alias 'ctx' must resolve to 'compact'"
    );
}

#[test]
fn catalog_register_dynamic_replaces_builtin_last_writer_wins() {
    let mut catalog = CommandCatalog::from_command_table();
    let count_before = catalog.len();

    // Replace /help with a skill version.
    let replacement = CommandEntry {
        name: "help".to_string(),
        aliases: Vec::new(),
        description: "Skill-enhanced help".to_string(),
        args_hint: Some("[topic]".to_string()),
        group: CommandGroup::Skills,
        source: CommandSource::Skill {
            skill_name: "help-enhancer".to_string(),
        },
    };
    catalog.register_dynamic(replacement);

    // Count must not grow — it's a replacement.
    assert_eq!(
        catalog.len(),
        count_before,
        "register_dynamic must replace in-place (count {count_before} must be unchanged)"
    );

    // Description must reflect the new entry.
    let updated = catalog.get("help").expect("help must still exist");
    assert_eq!(updated.description, "Skill-enhanced help");
    assert_eq!(updated.group, CommandGroup::Skills);
}
