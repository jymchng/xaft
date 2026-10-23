//! Behavioural coverage for the `slash` module's public surface.
//!
//! `slash/mod.rs` holds the command enum, the result type every handler
//! returns, the config-layer labels and the per-agent stats map. None of it is
//! reachable from a dispatch test that only exercises the commands a user
//! actually types, so the following were untested:
//!
//!   * `SlashCommand::all_discriminants` — the registry-validation input;
//!   * `SlashCommand::trigger_name` for every variant — the table the parser
//!     and `/help` both display;
//!   * `CommandResult`'s hand-written `Debug` and `Clone` impls, including the
//!     deliberate panic on the non-`Clone` `OpenMenu` variant;
//!   * `ConfigLayer::label`;
//!   * `AgentStatsMap::get_mut` / `len` and `insert`'s overwrite path.

use std::io;

use crossterm::event::KeyEvent;
use xaft_tui::{
    AgentStats, AgentStatsMap, CommandResult, ConfigLayer, ConfigRow, ConfigSection,
    ConfigValueKind, LineKind, MenuResult, MenuWidget, SlashCommand, StyledLine,
};

/// A do-nothing menu widget, used only to construct the `OpenMenu` variant so
/// its `Debug` and `Clone` behaviour can be asserted.
struct InertWidget;

impl MenuWidget for InertWidget {
    fn render(
        &self,
        _out: &mut dyn io::Write,
        _size: (u16, u16),
        _prev_rows: usize,
    ) -> io::Result<usize> {
        Ok(0)
    }

    fn handle_key(&mut self, _key: KeyEvent) -> MenuResult {
        MenuResult::Continue
    }

    fn title(&self) -> &str {
        "inert"
    }
}

fn example_section() -> ConfigSection {
    ConfigSection {
        name: "core".to_string(),
        rows: vec![ConfigRow {
            key: "core.log_level".to_string(),
            display_value: "info".to_string(),
            value_kind: ConfigValueKind::Str,
            source_layer: ConfigLayer::Default,
            is_overridden: false,
        }],
    }
}

// ── SlashCommand ────────────────────────────────────────────────────────────

#[test]
fn every_example_variant_has_a_unique_discriminant() {
    let examples = SlashCommand::all_example_variants();
    let discriminants = SlashCommand::all_discriminants();

    assert_eq!(
        discriminants.len(),
        examples.len(),
        "all_discriminants must yield one entry per example variant"
    );

    let mut unique = std::collections::HashSet::new();
    for d in &discriminants {
        assert!(
            unique.insert(*d),
            "all_example_variants repeated a variant, so the registry \
             validation input has a duplicate discriminant"
        );
    }
}

#[test]
fn every_discriminant_is_resolvable_by_trigger_name() {
    // `all_discriminants` is fed to the registry, which then looks each
    // discriminant up. A variant whose `trigger_name` arm was missing would
    // not compile, so this asserts the names are stable, non-empty and unique.
    let names: Vec<&str> = SlashCommand::all_example_variants()
        .iter()
        .map(SlashCommand::trigger_name)
        .collect();

    let mut unique = std::collections::HashSet::new();
    for name in &names {
        assert!(!name.is_empty(), "a trigger name must not be empty");
        assert!(
            !name.starts_with('/'),
            "trigger_name must omit the leading slash, got {name:?}"
        );
        assert!(
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "trigger names are lowercase ASCII, got {name:?}"
        );
        assert!(
            unique.insert(*name),
            "two variants share the trigger name {name:?}"
        );
    }
}

#[test]
fn trigger_names_cover_every_arm_of_the_match() {
    // Spelling the expected list out means adding a variant to the enum
    // without a `trigger_name` arm fails the count check, and renaming one
    // fails the membership check.
    let expected = [
        "help",
        "clear",
        "compact",
        "config",
        "cost",
        "init",
        "agents",
        "mcp",
        "resume",
        "rewind",
        "permissions",
        "model",
        "vim",
        "emacs",
        "theme",
        "login",
        "logout",
        "doctor",
        "memory",
        "diff",
        "commit",
        "pr",
        "quit",
        "mode",
        "cancel",
        "commands",
        "tools",
        "status",
        "expand",
        "history",
        "models",
        "skills",
        "workflows",
        "workflow",
        "replay",
        "ps",
        "stop",
        "loop",
        "loops",
        "startup",
        "workers",
    ];

    let names: Vec<&str> = SlashCommand::all_example_variants()
        .iter()
        .map(SlashCommand::trigger_name)
        .collect();

    assert_eq!(
        names.len(),
        expected.len(),
        "the example variant list grew or shrank; update `expected`"
    );
    for want in expected {
        assert!(
            names.contains(&want),
            "no variant reports trigger_name {want:?}; got {names:?}"
        );
    }
}

#[test]
fn trigger_name_is_independent_of_the_payload() {
    // The payload-carrying variants must report the bare trigger, not the
    // argument: `/theme dark` and `/theme` are the same command.
    assert_eq!(SlashCommand::Theme { name: None }.trigger_name(), "theme");
    assert_eq!(
        SlashCommand::Theme {
            name: Some("dark".into())
        }
        .trigger_name(),
        "theme"
    );
    assert_eq!(SlashCommand::Replay { id: None }.trigger_name(), "replay");
    assert_eq!(
        SlashCommand::Replay {
            id: Some("abc".into())
        }
        .trigger_name(),
        "replay"
    );
    assert_eq!(
        SlashCommand::Expand { target: None }.trigger_name(),
        "expand"
    );
    assert_eq!(SlashCommand::Mode { name: None }.trigger_name(), "mode");
}

// ── CommandResult ───────────────────────────────────────────────────────────

#[test]
fn command_result_debug_names_every_variant() {
    let cases: Vec<(CommandResult, &str)> = vec![
        (CommandResult::Lines(vec!["one".into()]), "Lines"),
        (
            CommandResult::StyledLines(vec![StyledLine::new("hi", LineKind::System)]),
            "StyledLines",
        ),
        (CommandResult::Handled, "Handled"),
        (CommandResult::Error("boom".into()), "Error"),
        (
            CommandResult::ConfigDisplay(vec![example_section()]),
            "ConfigDisplay",
        ),
        (
            CommandResult::OpenMenu(Box::new(InertWidget)),
            "OpenMenu(<dyn MenuWidget>)",
        ),
    ];

    for (result, want) in cases {
        let rendered = format!("{result:?}");
        assert!(
            rendered.starts_with(want),
            "expected the debug form to start with {want:?}, got {rendered:?}"
        );
    }
}

#[test]
fn command_result_clones_every_cloneable_variant() {
    let lines = CommandResult::Lines(vec!["a".into()]);
    match lines.clone() {
        CommandResult::Lines(v) => assert_eq!(v, vec!["a".to_string()]),
        other => panic!("clone changed the variant: {other:?}"),
    }

    let styled = CommandResult::StyledLines(vec![StyledLine::new("a", LineKind::System)]);
    match styled.clone() {
        CommandResult::StyledLines(v) => assert_eq!(v.len(), 1),
        other => panic!("clone changed the variant: {other:?}"),
    }

    assert!(matches!(
        CommandResult::Handled.clone(),
        CommandResult::Handled
    ));

    match CommandResult::Error("e".into()).clone() {
        CommandResult::Error(s) => assert_eq!(s, "e"),
        other => panic!("clone changed the variant: {other:?}"),
    }

    match CommandResult::ConfigDisplay(vec![example_section()]).clone() {
        CommandResult::ConfigDisplay(v) => {
            assert_eq!(v[0].name, "core");
            assert_eq!(v[0].rows[0].key, "core.log_level");
        }
        other => panic!("clone changed the variant: {other:?}"),
    }
}

#[test]
#[should_panic(expected = "CommandResult::OpenMenu is not Clone")]
fn cloning_an_open_menu_result_panics_loudly() {
    // `Box<dyn MenuWidget>` cannot be cloned. Panicking is deliberate — the
    // alternative is a silent `None`, which would drop the menu the caller
    // asked for.
    let result = CommandResult::OpenMenu(Box::new(InertWidget));
    let _ = result.clone();
}

// ── ConfigLayer ─────────────────────────────────────────────────────────────

#[test]
fn every_config_layer_has_a_distinct_label() {
    let layers = [
        ConfigLayer::CliFlag,
        ConfigLayer::EnvVar,
        ConfigLayer::Session,
        ConfigLayer::Project,
        ConfigLayer::User,
        ConfigLayer::Default,
    ];

    let labels: Vec<&str> = layers.iter().map(ConfigLayer::label).collect();
    assert_eq!(
        labels,
        vec!["cli", "env", "session", "project", "user", "default"]
    );

    let mut unique = std::collections::HashSet::new();
    for label in &labels {
        assert!(unique.insert(*label), "duplicate layer label {label:?}");
    }
}

// ── AgentStatsMap ───────────────────────────────────────────────────────────

fn stats(calls: u32) -> AgentStats {
    AgentStats {
        calls,
        input_tokens: u64::from(calls) * 10,
        ..AgentStats::default()
    }
}

#[test]
fn a_new_stats_map_is_empty_and_has_no_length() {
    let map = AgentStatsMap::new();
    assert!(map.is_empty());
    assert_eq!(map.len(), 0);
    assert_eq!(map.iter().count(), 0);
}

#[test]
fn insert_appends_new_names_and_replaces_existing_ones() {
    let mut map = AgentStatsMap::new();
    map.insert("coder".into(), stats(1));
    map.insert("planner".into(), stats(2));
    assert_eq!(map.len(), 2);

    // Insertion order is preserved, which is what the `/cost` listing relies on.
    let order: Vec<&str> = map.iter().map(|(name, _)| name).collect();
    assert_eq!(order, vec!["coder", "planner"]);

    // Re-inserting an existing name REPLACES rather than duplicating, so the
    // map length must not grow.
    map.insert("coder".into(), stats(9));
    assert_eq!(map.len(), 2, "re-inserting a name must not append a row");

    let (_, coder) = map.iter().find(|(n, _)| *n == "coder").unwrap();
    assert_eq!(coder.calls, 9, "the replacement value must win");
    assert_eq!(coder.input_tokens, 90);
}

#[test]
fn get_mut_finds_a_known_name_and_rejects_an_unknown_one() {
    let mut map = AgentStatsMap::new();
    map.insert("coder".into(), stats(1));

    let slot = map.get_mut("coder").expect("known name must resolve");
    slot.calls += 4;
    slot.cost_usd = 0.25;

    assert!(
        map.get_mut("planner").is_none(),
        "unknown name must not resolve"
    );
    // `get_mut` must hand out the live entry, not a copy.
    let (_, coder) = map.iter().find(|(n, _)| *n == "coder").unwrap();
    assert_eq!(coder.calls, 5);
    assert_eq!(coder.cost_usd, 0.25);
}
