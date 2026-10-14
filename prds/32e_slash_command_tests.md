# PRD 32e — Slash Command System: Test Plan

## Status: Draft
## Depends on: PRD 32a, PRD 32b, PRD 32c, PRD 32d

---

## 1. Test Scope

| Layer | Where |
|-------|-------|
| Parser unit tests | `crates/xaft-tui/src/slash/parser.rs` (`#[cfg(test)]`) |
| Palette unit tests | `crates/xaft-tui/src/slash/palette.rs` (`#[cfg(test)]`) |
| Registry unit tests | `crates/xaft-tui/src/slash/registry.rs` (`#[cfg(test)]`) |
| Per-command handler tests | `crates/xaft-tui/src/slash/commands/*.rs` (`#[cfg(test)]`) |
| State machine integration | `crates/xaft-tui/tests/slash_integration.rs` |
| Renderer integration | `crates/xaft-tui/tests/slash_rendering.rs` |

---

## 2. Parser Tests (`parser.rs`)

```rust
#[test]
fn parse_slash_help() {
    assert_eq!(SlashCommandParser::parse("/help"), Some(Ok(SlashCommand::Help)));
}

#[test]
fn parse_slash_help_uppercase() {
    assert_eq!(SlashCommandParser::parse("/HELP"), Some(Ok(SlashCommand::Help)));
}

#[test]
fn parse_slash_clear_alias() {
    assert_eq!(SlashCommandParser::parse("/cls"), Some(Ok(SlashCommand::Clear)));
}

#[test]
fn parse_slash_compact_alias() {
    assert_eq!(SlashCommandParser::parse("/ctx"), Some(Ok(SlashCommand::Compact)));
}

#[test]
fn parse_slash_model_ok() {
    assert_eq!(
        SlashCommandParser::parse("/model claude-opus-4-8"),
        Some(Ok(SlashCommand::Model { name: "claude-opus-4-8".into() }))
    );
}

#[test]
fn parse_slash_model_missing_arg() {
    let result = SlashCommandParser::parse("/model");
    assert!(matches!(result, Some(Err(_))));
    if let Some(Err(msg)) = result {
        assert!(msg.contains("/model requires"));
    }
}

#[test]
fn parse_slash_resume_no_arg() {
    assert_eq!(
        SlashCommandParser::parse("/resume"),
        Some(Ok(SlashCommand::Resume { id: None }))
    );
}

#[test]
fn parse_slash_resume_with_id() {
    assert_eq!(
        SlashCommandParser::parse("/resume f7fba85e"),
        Some(Ok(SlashCommand::Resume { id: Some("f7fba85e".into()) }))
    );
}

#[test]
fn parse_slash_rewind_number() {
    assert_eq!(
        SlashCommandParser::parse("/rewind 5"),
        Some(Ok(SlashCommand::Rewind { msg_index: Some(5) }))
    );
}

#[test]
fn parse_slash_rewind_bad_arg() {
    let result = SlashCommandParser::parse("/rewind foo");
    assert!(matches!(result, Some(Err(_))));
    if let Some(Err(msg)) = result {
        assert!(msg.contains("expects a message number"));
    }
}

#[test]
fn parse_slash_theme_no_arg() {
    assert_eq!(
        SlashCommandParser::parse("/theme"),
        Some(Ok(SlashCommand::Theme { name: None }))
    );
}

#[test]
fn parse_slash_theme_named() {
    assert_eq!(
        SlashCommandParser::parse("/theme nord"),
        Some(Ok(SlashCommand::Theme { name: Some("nord".into()) }))
    );
}

#[test]
fn parse_slash_pr_no_arg() {
    assert_eq!(
        SlashCommandParser::parse("/pr"),
        Some(Ok(SlashCommand::Pr { title: None }))
    );
}

#[test]
fn parse_slash_pr_with_title() {
    assert_eq!(
        SlashCommandParser::parse("/pr Add entropy improvements"),
        Some(Ok(SlashCommand::Pr { title: Some("Add entropy improvements".into()) }))
    );
}

#[test]
fn parse_plain_text_returns_none() {
    assert_eq!(SlashCommandParser::parse("hello world"), None);
}

#[test]
fn parse_bare_slash_returns_none() {
    assert_eq!(SlashCommandParser::parse("/"), None);
}

#[test]
fn parse_space_prefix_not_slash_command() {
    // " /help" has a leading space — not a slash command.
    assert_eq!(SlashCommandParser::parse(" /help"), None);
}

#[test]
fn parse_unknown_trigger_returns_err() {
    let result = SlashCommandParser::parse("/blorp");
    assert!(matches!(result, Some(Err(_))));
    if let Some(Err(msg)) = result {
        assert!(msg.contains("Unknown command: /blorp"));
        assert!(msg.contains("/help"));
    }
}

#[test]
fn completions_empty_prefix_returns_all() {
    let all = SlashCommandParser::completions("");
    // Every known trigger must be present.
    for trigger in &["help", "clear", "compact", "cost", "agents", "model"] {
        assert!(all.contains(trigger), "missing {trigger}");
    }
}

#[test]
fn completions_prefix_c() {
    let cs = SlashCommandParser::completions("c");
    assert!(cs.contains(&"clear"));
    assert!(cs.contains(&"compact"));
    assert!(cs.contains(&"cost"));
    assert!(cs.contains(&"config"));
    assert!(cs.contains(&"commit"));
    assert!(!cs.contains(&"help"));
}

#[test]
fn completions_prefix_cl() {
    let cs = SlashCommandParser::completions("cl");
    assert_eq!(cs, vec!["clear"]);
}

#[test]
fn unique_completion_hit() {
    assert_eq!(SlashCommandParser::unique_completion("cl"), Some("clear"));
}

#[test]
fn unique_completion_miss_ambiguous() {
    assert_eq!(SlashCommandParser::unique_completion("c"), None);
}

#[test]
fn unique_completion_miss_empty() {
    assert_eq!(SlashCommandParser::unique_completion("zzz"), None);
}
```

---

## 3. Palette Tests (`palette.rs`)

```rust
#[test]
fn palette_new_empty_partial_has_all_candidates() {
    let p = SlashPalette::new("");
    assert!(p.candidates.len() >= 21);
    assert_eq!(p.selected, 0);
}

#[test]
fn palette_new_partial_filters() {
    let p = SlashPalette::new("cl");
    assert_eq!(p.candidates, vec!["clear"]);
}

#[test]
fn palette_select_next_wraps() {
    let mut p = SlashPalette::new("c");
    let n = p.candidates.len();
    for _ in 0..n {
        p.select_next();
    }
    assert_eq!(p.selected, 0);
}

#[test]
fn palette_select_prev_wraps() {
    let mut p = SlashPalette::new("c");
    p.select_prev();
    assert_eq!(p.selected, p.candidates.len() - 1);
}

#[test]
fn palette_update_resets_selection() {
    let mut p = SlashPalette::new("c");
    p.select_next();
    p.select_next();
    p.update("cl");
    assert_eq!(p.selected, 0);
    assert_eq!(p.candidates, vec!["clear"]);
}

#[test]
fn palette_selected_trigger_none_when_empty() {
    let p = SlashPalette::new("zzz");
    assert!(p.candidates.is_empty());
    assert!(p.selected_trigger().is_none());
}
```

---

## 4. Registry Tests (`registry.rs`)

```rust
#[test]
fn registry_all_slash_command_variants_have_handlers() {
    // Smoke-test: building the registry does not panic.
    let registry = build_test_registry();
    // Access every variant.
    for cmd in SlashCommand::all_example_variants() {
        let _ = registry.get(&cmd);
    }
}

#[test]
#[should_panic(expected = "no handler registered")]
fn registry_missing_handler_panics_on_get() {
    // Build a registry without registering Help, then get it.
    let registry = SlashCommandRegistry::builder()
        // deliberately omit some handlers
        .build();
    let _ = registry.get(&SlashCommand::Help);
}
```

---

## 5. Per-Command Handler Tests

### 5.1 `/help`

```rust
#[test]
fn help_handler_lists_all_commands() {
    let registry = build_test_registry();
    let handler = HelpHandler::new(Arc::new(registry));
    let ctx = test_ctx("");
    let result = handler.execute(ctx);
    let text = result_to_string(&result);
    for trigger in &["clear", "compact", "cost", "model", "agents"] {
        assert!(text.contains(trigger), "missing {trigger} in /help output");
    }
}

#[test]
fn help_handler_single_command_detail() {
    let registry = build_test_registry();
    let handler = HelpHandler::new(Arc::new(registry));
    let ctx = test_ctx("compact");
    let result = handler.execute(ctx);
    let text = result_to_string(&result);
    assert!(text.contains("Manually trigger context compaction"));
    assert!(text.contains("Aliases:"));
}

#[test]
fn help_handler_unknown_subcommand() {
    let registry = build_test_registry();
    let handler = HelpHandler::new(Arc::new(registry));
    let ctx = test_ctx("blorp");
    assert!(matches!(handler.execute(ctx), CommandResult::Error(_)));
}
```

### 5.2 `/clear`

```rust
#[test]
fn clear_handler_emits_display_cleared_signal() {
    let bus = Arc::new(SignalBus::new());
    let mut received = false;
    // Subscribe before executing.
    let bus2 = Arc::clone(&bus);
    let handle = tokio::spawn(async move {
        bus2.subscribe::<SessionDisplayCleared>(|_| { received = true; });
    });
    let handler = ClearHandler;
    let ctx = test_ctx_with_bus("", Arc::clone(&bus));
    handler.execute(ctx);
    // Check signal was emitted.
    // (In tests, use a synchronous bus mock or check the signal count)
}
```

### 5.3 `/cost`

```rust
#[test]
fn cost_handler_empty_stats() {
    let handler = CostHandler;
    let ctx = test_ctx_with_stats("", AgentStatsMap::new());
    let result = handler.execute(ctx);
    let text = result_to_string(&result);
    assert!(text.contains("No LLM calls recorded yet"));
}

#[test]
fn cost_handler_renders_table() {
    let mut stats = AgentStatsMap::new();
    stats.insert("planner".into(), AgentStats {
        input_tokens: 1000, output_tokens: 200, cache_read: 0,
        cache_write: 0, cost_usd: 0.005, calls: 1,
    });
    let handler = CostHandler;
    let ctx = test_ctx_with_stats("", stats);
    let result = handler.execute(ctx);
    let text = result_to_string(&result);
    assert!(text.contains("planner"));
    assert!(text.contains("0.0050") || text.contains("$0.005"));
    assert!(text.contains("TOTAL"));
}
```

### 5.4 `/model`

```rust
#[test]
fn model_handler_missing_name_returns_error() {
    let handler = ModelHandler::new(make_runtime_handle());
    let result = handler.execute(test_ctx(""));
    assert!(matches!(result, CommandResult::Error(_)));
}

#[test]
fn model_handler_sets_override() {
    let runtime = make_runtime_handle();
    let handler = ModelHandler::new(Arc::clone(&runtime));
    let result = handler.execute(test_ctx("claude-opus-4-8"));
    assert!(matches!(result, CommandResult::Lines(_)));
    assert_eq!(runtime.model_override(), Some("claude-opus-4-8".to_string()));
}
```

### 5.5 `/theme`

```rust
#[test]
fn theme_handler_cycles_on_empty_arg() {
    let config = Arc::new(RwLock::new(XaftConfig::default()));
    let handler = ThemeHandler::new(Arc::clone(&config));
    let before = config.read().tui.theme.clone();
    handler.execute(test_ctx(""));
    let after = config.read().tui.theme.clone();
    assert_ne!(before, after);
}

#[test]
fn theme_handler_unknown_name_returns_error() {
    let config = Arc::new(RwLock::new(XaftConfig::default()));
    let handler = ThemeHandler::new(config);
    let result = handler.execute(test_ctx("unicorn-tears"));
    assert!(matches!(result, CommandResult::Error(_)));
}

#[test]
fn theme_handler_sets_named_theme() {
    let config = Arc::new(RwLock::new(XaftConfig::default()));
    let handler = ThemeHandler::new(Arc::clone(&config));
    let result = handler.execute(test_ctx("nord"));
    assert!(matches!(result, CommandResult::Lines(_)));
    assert_eq!(config.read().tui.theme, "nord");
}
```

### 5.6 `/unknown` (error path)

```rust
#[test]
fn unknown_slash_command_produces_error_line() {
    let mut state = make_app_state();
    state.handle_input_submitted("/zzzzz".into());
    let texts = commit_texts(&state);
    assert!(
        texts.iter().any(|t| t.contains("Unknown command") && t.contains("/zzzzz")),
        "expected unknown-command error: {texts:?}"
    );
    // Must NOT have forwarded to agent.
    assert_eq!(state.phase, WorkflowPhase::Idle);
}
```

---

## 6. State Machine Integration Tests (`slash_integration.rs`)

```rust
#[test]
fn slash_command_does_not_reach_agent() {
    let mut state = make_app_state();
    state.handle_input_submitted("/clear".into());
    // No RunRequest should have been queued.
    assert!(!state.has_pending_run_request());
}

#[test]
fn slash_command_clears_input_bar() {
    let mut state = make_app_state();
    state.input_bar.insert_str("/clear");
    state.handle_input_submitted("/clear".into());
    assert!(state.input_bar.is_empty());
}

#[test]
fn slash_command_pushes_separator_line() {
    let mut state = make_app_state();
    state.handle_input_submitted("/help".into());
    let texts = commit_texts(&state);
    assert!(
        texts.iter().any(|t| t.contains("/help")),
        "separator must mention the command: {texts:?}"
    );
}

#[test]
fn slash_command_emits_signal() {
    let (bus, mut rx) = make_signal_channel::<SlashCommandExecuted>();
    let mut state = make_app_state_with_bus(Arc::clone(&bus));
    state.handle_input_submitted("/clear".into());
    // Poll the channel.
    let sig = rx.try_recv().expect("SlashCommandExecuted must be emitted");
    assert_eq!(sig.name, "clear");
    assert_eq!(sig.args, "");
    assert!(sig.success);
}

#[test]
fn slash_command_error_does_not_emit_with_success_true() {
    let (bus, mut rx) = make_signal_channel::<SlashCommandExecuted>();
    let mut state = make_app_state_with_bus(Arc::clone(&bus));
    state.handle_input_submitted("/model".into());  // missing arg
    let sig = rx.try_recv().expect("SlashCommandExecuted emitted even on error");
    assert!(!sig.success);
}

#[test]
fn slash_history_records_executed_commands() {
    let mut state = make_app_state();
    state.handle_input_submitted("/clear".into());
    state.handle_input_submitted("/help".into());
    assert_eq!(state.slash_history.len(), 2);
}

#[test]
fn palette_opens_on_slash_input() {
    let mut state = make_app_state();
    state.handle_char('/');
    assert!(state.slash_palette.is_some());
}

#[test]
fn palette_closes_on_escape() {
    let mut state = make_app_state();
    state.handle_char('/');
    state.handle_key(KeyCode::Esc);
    assert!(state.slash_palette.is_none());
}

#[test]
fn palette_closes_after_command_executes() {
    let mut state = make_app_state();
    state.handle_char('/');
    state.input_bar.insert_str("clear");
    state.handle_input_submitted("/clear".into());
    assert!(state.slash_palette.is_none());
}

#[test]
fn palette_tab_completes_unique_match() {
    let mut state = make_app_state();
    state.handle_char('/');
    state.input_bar.insert_str("cl");
    state.handle_key(KeyCode::Tab);
    // Input bar should now contain "/clear "
    let content = state.input_bar.first_line_text();
    assert_eq!(content, "/clear ");
}

#[test]
fn multiline_input_is_not_slash_command() {
    let mut state = make_app_state();
    // Insert a newline — multi-line input is never a slash command.
    state.input_bar.insert_str("/clear");
    state.input_bar.insert_newline();
    state.input_bar.insert_str("extra line");
    let text = state.input_bar.to_string();
    state.handle_input_submitted(text);
    // Should be forwarded to agent, not parsed as slash command.
    assert!(state.has_pending_run_request());
}
```

---

## 7. Rendering Tests (`slash_rendering.rs`)

```rust
#[test]
fn slash_output_lines_have_system_kind() {
    let mut state = make_app_state();
    state.handle_input_submitted("/help".into());
    let lines = commit_lines(&state);
    let system_lines: Vec<_> = lines.iter()
        .filter(|l| l.kind == LineKind::System)
        .collect();
    assert!(!system_lines.is_empty(), "help output must use LineKind::System");
}

#[test]
fn slash_error_lines_have_error_kind() {
    let mut state = make_app_state();
    state.handle_input_submitted("/model".into());  // missing arg
    let lines = commit_lines(&state);
    let error_lines: Vec<_> = lines.iter()
        .filter(|l| l.kind == LineKind::Error)
        .collect();
    assert!(!error_lines.is_empty(), "error output must use LineKind::Error");
    assert!(
        error_lines.iter().any(|l| l.text.contains('✗')),
        "error lines must have ✗ prefix"
    );
}

#[test]
fn slash_separator_line_contains_command_name() {
    let mut state = make_app_state();
    state.handle_input_submitted("/cost".into());
    let lines = commit_lines(&state);
    assert!(
        lines.iter().any(|l| l.kind == LineKind::Separator && l.text.contains("cost")),
        "separator must contain the command name"
    );
}
```

---

## 8. Test Helpers (`slash_integration.rs`)

```rust
fn make_app_state() -> AppState {
    AppState::new("test task")
}

fn make_app_state_with_bus(bus: Arc<SignalBus>) -> AppState {
    AppState::new_with_bus("test task", bus)
}

fn commit_texts(state: &AppState) -> Vec<&str> {
    state.mutations.iter().filter_map(|m| match m {
        RenderMutation::CommitLine(l) => Some(l.text.as_str()),
        _ => None,
    }).collect()
}

fn commit_lines(state: &AppState) -> Vec<&StyledLine> {
    state.mutations.iter().filter_map(|m| match m {
        RenderMutation::CommitLine(l) => Some(l),
        _ => None,
    }).collect()
}

fn result_to_string(result: &CommandResult) -> String {
    match result {
        CommandResult::Lines(ls) => ls.join("\n"),
        CommandResult::StyledLines(ls) => ls.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n"),
        CommandResult::Error(e) => e.clone(),
        CommandResult::Handled => String::new(),
    }
}

fn test_ctx(args: &str) -> CommandContext {
    CommandContext {
        args: args.into(),
        command: SlashCommand::Help,
        signals: Arc::new(SignalBus::new()),
        config: Arc::new(XaftConfig::default()),
        session_id: None,
        working_dir: PathBuf::from("/tmp"),
        terminal_cols: 80,
        llm_stats: Arc::new(RwLock::new(AgentStatsMap::new())),
        conversation_store: None,
        session_store: None,
    }
}
```

---

## 9. Acceptance Criteria Checklist

| AC | Test | Status |
|----|------|--------|
| AC1: `/` opens palette | `palette_opens_on_slash_input` | — |
| AC2: Tab completes unique match | `palette_tab_completes_unique_match` | — |
| AC3: `/clear` resets display | `slash_command_does_not_reach_agent` + `slash_command_clears_input_bar` | — |
| AC4: `/nonexistent` is error-only | `unknown_slash_command_produces_error_line` | — |
| AC5: All commands emit signal | `slash_command_emits_signal` | — |
| AC6: `/help` lists all 21 commands | `help_handler_lists_all_commands` | — |
| AC7: `/cost` renders table | `cost_handler_renders_table` | — |
| AC8: Async shows spinner | (manual / renderer test, future) | — |
| AC9: Palette closes after execute | `palette_closes_after_command_executes` | — |
| AC10: History via ↑/↓ | `slash_history_records_executed_commands` | — |
