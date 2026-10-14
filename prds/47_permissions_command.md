# PRD 47 — `/permissions` Command: Live Tool Permission Editor

## Status: Draft
## Depends on: PRD 32a–32e, PRD 34 Feature B (inline editing pattern)

---

## 1. Problem

`PermissionsHandler` shows a static four-row table of hardcoded built-in
defaults. It does not read from `XaftConfig`, does not reflect user-
configured rules, and cannot be used to change anything.

Additionally, the approval gate (`ApprovalGate`) has runtime permission
state — tools that have been auto-approved or denied in the current session —
that is not visible anywhere.

---

## 2. Goals

1. `/permissions` — show a merged table of all tool permissions: built-in
   defaults overlaid by config-file rules overlaid by session-level overrides.
2. Each row shows tool name, rule (`allow` / `confirm` / `deny`), and
   source layer.
3. `/permissions allow <tool>` / `/permissions deny <tool>` /
   `/permissions confirm <tool>` — set a session-level override immediately.
4. Session overrides are lost when the session ends; to persist a rule the
   user must add it to their `xaft.toml`.

### Example: `/permissions`

```
TOOL            RULE      SOURCE
────────────────────────────────────────────────
bash_exec       confirm   built-in
write_file      allow     built-in
read_file       allow     built-in
delete_file     deny      built-in
list_dir        allow     project .xaft.toml
run_tests       allow     session override
```

---

## 3. Non-Goals

- Editing `xaft.toml` directly from the TUI (use `/config` for that).
- Per-path rules (coarse per-tool granularity only in v1).
- Regex-based tool matching.

---

## 4. Technical Design

### 4.1 `PermissionRule` and `PermissionSource`

Define in `crates/xaft-config/src/permissions.rs` (or expand the existing
concept from `XaftConfig`):

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionRule { Allow, Confirm, Deny }

#[derive(Clone, Debug)]
pub enum PermissionSource {
    BuiltIn,
    UserConfig,
    ProjectConfig,
    SessionOverride,
}

#[derive(Clone, Debug)]
pub struct ToolPermission {
    pub tool: String,
    pub rule: PermissionRule,
    pub source: PermissionSource,
}
```

### 4.2 `PermissionRegistry`

```rust
pub struct PermissionRegistry {
    built_in: Vec<ToolPermission>,
    config: Vec<ToolPermission>,         // from XaftConfig (merged user + project)
    session: Arc<Mutex<Vec<ToolPermission>>>,  // runtime overrides
}

impl PermissionRegistry {
    pub fn effective(&self) -> Vec<ToolPermission>;
    pub fn set_session(&self, tool: &str, rule: PermissionRule);
    pub fn rule_for(&self, tool: &str) -> PermissionRule;
}
```

`ApprovalGate` uses `PermissionRegistry::rule_for` to decide whether a
tool call needs confirmation. This replaces any existing ad-hoc permission
lookup.

### 4.3 `CommandContext` addition

```rust
pub permissions: Option<Arc<PermissionRegistry>>,
```

### 4.4 `PermissionsHandler::execute`

No-arg path: call `registry.effective()`, sort by source priority, render
`CliTable::new(["TOOL", "RULE", "SOURCE"])`.

With sub-command (`allow` / `deny` / `confirm` + `<tool>`):
```rust
let (verb, tool_name) = parse_permission_arg(&ctx.args)?;
let rule = match verb { "allow" => Allow, "deny" => Deny, "confirm" => Confirm, _ => error };
registry.set_session(tool_name, rule);
CommandResult::Lines(vec![format!("Session override: {tool_name} → {verb}")])
```

### 4.5 `XaftConfig` `permissions` field

```toml
[permissions]
bash_exec  = "confirm"
delete_file = "deny"
write_file = "allow"
```

`XaftConfig.permissions: HashMap<String, PermissionRule>` (default empty).
`PermissionRegistry::new(config)` populates the `config` tier from this map.

### 4.6 Built-in defaults

Defined as a `const` slice in `PermissionRegistry`:
```rust
const BUILT_IN: &[(&str, PermissionRule)] = &[
    ("bash_exec",    PermissionRule::Confirm),
    ("write_file",   PermissionRule::Allow),
    ("read_file",    PermissionRule::Allow),
    ("delete_file",  PermissionRule::Deny),
    ("list_dir",     PermissionRule::Allow),
    ("search_files", PermissionRule::Allow),
    ("git_commit",   PermissionRule::Confirm),
    ("git_push",     PermissionRule::Confirm),
];
```

---

## 5. Implementation Steps

1. Define `PermissionRule`, `PermissionSource`, `ToolPermission` in
   `xaft-config/src/permissions.rs`.
2. Implement `PermissionRegistry` with built-in defaults + config + session
   tiers.
3. Add `permissions: HashMap<String, String>` to `XaftConfig`; deserialise
   into `PermissionRule`.
4. Wire `Arc<PermissionRegistry>` through `TuiApp` → `AppState` →
   `build_command_context`.
5. Update `ApprovalGate` to delegate to `PermissionRegistry::rule_for`.
6. Update `PermissionsHandler` (list + set sub-commands).
7. Tests (see §6).

---

## 6. Tests

```
test permission_registry_built_in_defaults
test permission_registry_config_overrides_built_in
test permission_registry_session_overrides_config
test permissions_handler_list_shows_effective_rules
test permissions_handler_allow_sets_session_override
test permissions_handler_deny_sets_session_override
test permissions_handler_unknown_tool_still_sets_override
test permissions_handler_invalid_verb_returns_error
test approval_gate_uses_permission_registry
```
