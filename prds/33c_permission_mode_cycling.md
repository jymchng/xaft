# PRD 33c — Permission Mode Runtime Cycling (Shift+Tab)

## Status: Draft
## Depends on: PRD 09, PRD 10, PRD 27/F2, PRD 47 (permissions command)

---

## 1. Problem

xaft's approval gate is binary: either `TuiApprovalGate` (every
confirmation-required tool prompts the user) or `AutoApproveGate`
(everything auto-approved). The mode is set at startup and cannot be
changed without restarting.

From the Claude Code output the user pasted:

```
⏵⏵ bypass permissions on  (shift+tab to cycle)
```

Claude Code exposes three permission modes that cycle with `Shift+Tab`:

| Mode | Indicator | Behavior |
|------|-----------|----------|
| Normal | `⏸` | All confirmations required |
| Auto-approve | `⏵` | Low-risk tools auto-approved |
| Bypass | `⏵⏵` | All tools bypass confirmation |

Being able to switch modes mid-run is critical for productivity: a user
who started with full approvals may want to quickly enable auto-approve
for a repetitive file-edit loop, then re-enable manual approval before
a git push.

---

## 2. Goals

1. Three permission modes cycle in order with `Shift+Tab`:
   **Normal → AutoApprove → Bypass → Normal → …**
2. The current mode is displayed in the status bar / prompt line at all
   times.
3. Changing the mode takes effect immediately for the **next** tool call.
   In-flight tool confirmations already shown are not retroactively
   affected.
4. `Bypass` mode is intentionally one step past `AutoApprove` so it
   requires a deliberate second press — reducing accidental activation.
5. The mode resets to `Normal` at the end of each session.
6. A default mode can be set in config (e.g. `[tui] default_permission_mode
   = "auto-approve"`) so power users don't have to press `Shift+Tab` on
   every launch.

---

## 3. Non-Goals

- Persisting the runtime mode selection across sessions when no default
  is configured.
- Fine-grained per-tool overrides (that is the `PermissionRegistry` in
  PRD 47; this PRD is about the macro mode switch).
- Removing the existing `auto_approve` flag on `RunRequest` (that
  sets the initial mode for headless/CI runs).
- Bypassing the `dangerously_skip_permissions` flag path (that remains
  a separate launch-time escape hatch).

---

## 4. Design

### 4.1 `PermissionMode` enum

Defined in `crates/xaft-tui/src/approval.rs` (alongside `ApprovalContext`,
`RiskLevel`, etc.):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    #[default]
    Normal,      // All confirmations required per RiskLevel
    AutoApprove, // Low-risk and medium-risk auto-approved
    Bypass,      // All tools bypass confirmation
}

impl PermissionMode {
    pub fn cycle(self) -> Self {
        match self {
            Self::Normal      => Self::AutoApprove,
            Self::AutoApprove => Self::Bypass,
            Self::Bypass      => Self::Normal,
        }
    }

    pub fn indicator(self) -> &'static str {
        match self {
            Self::Normal      => "⏸",
            Self::AutoApprove => "⏵",
            Self::Bypass      => "⏵⏵",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal      => "permissions on",
            Self::AutoApprove => "auto-approve",
            Self::Bypass      => "bypass permissions",
        }
    }

    pub fn from_run_request(auto_approve: bool, skip_permissions: bool) -> Self {
        if skip_permissions { Self::Bypass }
        else if auto_approve { Self::AutoApprove }
        else { Self::Normal }
    }
}
```

### 4.2 `AppState` field

```rust
pub permission_mode: PermissionMode,
```

Initialised in `AppState::new`:
```rust
permission_mode: PermissionMode::from_run_request(
    run_request.auto_approve,
    run_request.dangerously_skip_permissions,
),
```

If a `[tui] default_permission_mode` is set in config, it overrides
the `Normal` default (but not an explicitly-passed `--auto-approve` flag).

### 4.3 `Shift+Tab` handler

In `AppState::handle_key_event`:

```rust
KeyCode::BackTab => {  // crossterm BackTab = Shift+Tab
    let new_mode = self.permission_mode.cycle();
    self.permission_mode = new_mode;
    self.signal_bus.emit(XaftPermissionModeChanged { mode: new_mode });
    // Refresh prompt so the indicator updates immediately.
    self.mutations.push(RenderMutation::UpdatePrompt(build_prompt(self)));
    InputAction::NoOp
}
```

**Interaction with Vim/Emacs mode (PRD 43)**: `BackTab` does not appear
in any vim normal/emacs binding tables, so it always reaches the mode
cycling handler regardless of `InputMode`.

### 4.4 `XaftPermissionModeChanged` signal

```rust
/// Emitted whenever the permission mode changes (Shift+Tab or programmatic).
#[derive(Debug, Clone)]
pub struct XaftPermissionModeChanged {
    pub mode:       PermissionMode,
    pub changed_by: PermissionModeSource,
}

pub enum PermissionModeSource {
    UserKeypress,   // Shift+Tab
    RunRequest,     // set at launch
    Config,         // default_permission_mode from config
}
```

### 4.5 Updated `TuiApprovalGate`

`TuiApprovalGate` gains `mode: Arc<RwLock<PermissionMode>>`:

```rust
pub struct TuiApprovalGate {
    queue:           Arc<ApprovalQueue>,
    mode:            Arc<RwLock<PermissionMode>>,
    /// Subscription handle — kept alive to receive mode-change signals.
    _mode_sub:       SignalSubscription,
}

impl TuiApprovalGate {
    pub fn new(queue: Arc<ApprovalQueue>, signal_bus: &SignalBus, initial_mode: PermissionMode) -> Self {
        let mode = Arc::new(RwLock::new(initial_mode));
        let mode_clone = Arc::clone(&mode);
        let sub = signal_bus.subscribe::<XaftPermissionModeChanged>(move |ev| {
            *mode_clone.write().unwrap() = ev.mode;
        });
        Self { queue, mode, _mode_sub: sub }
    }
}

impl ApprovalGate for TuiApprovalGate {
    async fn request_approval(&self, ctx: ApprovalContext) -> ApprovalDecision {
        let mode = *self.mode.read().unwrap();
        match mode {
            PermissionMode::Bypass => ApprovalDecision::Approved,
            PermissionMode::AutoApprove => {
                // PRD 47 PermissionRegistry takes precedence for session overrides.
                // Here we apply the macro threshold.
                if ctx.risk_level <= RiskLevel::Medium {
                    ApprovalDecision::Approved
                } else {
                    self.prompt_user(ctx).await
                }
            }
            PermissionMode::Normal => self.prompt_user(ctx).await,
        }
    }
}
```

**Ordering with in-flight approvals**: `request_approval` reads the mode
lock at the moment of the call. If a tool call is already waiting at the
prompt (`prompt_user` is blocking on the `ApprovalQueue`), mode changes
do not retroactively resolve it. The user still sees and must answer the
in-flight prompt. This is the documented behaviour (§2 Goal 3).

**Interaction with `PermissionRegistry` (PRD 47)**:
`PermissionRegistry::rule_for` is consulted first. If a tool has a
session `deny` override, it is denied regardless of `PermissionMode`.
If it has an `allow` override, it is approved regardless of mode.
Only tools without a session override fall through to the
`TuiApprovalGate::request_approval` logic above.

### 4.6 `AutoApproveGate` unification

`AutoApproveGate` (the existing struct) is replaced by `TuiApprovalGate`
initialised with `PermissionMode::Bypass`. The `auto_approve` launch flag
now calls:
```rust
TuiApprovalGate::new(queue, signal_bus, PermissionMode::AutoApprove)
```
rather than constructing a separate gate type. `AutoApproveGate` is
deprecated and removed.

### 4.7 Prompt line display

`PromptState` gains:
```rust
pub permission_mode: PermissionMode,
```

`build_prompt()` copies `state.permission_mode`.

`IncrementalRenderer::draw_prompt_block` renders the mode chip as the
leftmost element of the hint line below the input border:

```
⏵⏵ bypass permissions  (shift+tab to cycle)  ·  model: claude-sonnet-4-6
```

The chip updates via `RenderMutation::UpdatePrompt` — same mechanism as
the rest of the prompt, no special case.

### 4.8 Config support

`TuiConfig` gains:
```rust
/// Default permission mode at session start.
/// Valid values: "normal" | "auto-approve" | "bypass"
/// Overridden by --auto-approve and --dangerously-skip-permissions flags.
pub default_permission_mode: Option<PermissionMode>,
```

Default: `None` (resolves to `Normal` unless flags say otherwise).

---

## 5. Interaction Matrix

| Scenario | Resolution |
|----------|------------|
| `--auto-approve` flag + `Shift+Tab` (one press) | Starts `AutoApprove`, cycles to `Bypass` |
| `--dangerously-skip-permissions` + `Shift+Tab` | Starts `Bypass`, cycles to `Normal` |
| Mode change while tool is at approval prompt | Prompt remains; new mode applies to next tool |
| Mode change while `Bypass`, tool denied by `PermissionRegistry` | Deny wins (registry takes precedence) |
| `/permissions deny bash_exec` then `Bypass` mode | `bash_exec` still denied (registry overrides mode) |

---

## 6. Files

| File | Change |
|------|--------|
| `crates/xaft-tui/src/approval.rs` | Add `PermissionMode` enum, `PermissionModeSource` |
| `crates/xaft-tui/src/approval_gate.rs` | Refactor `TuiApprovalGate` to hold mode; remove `AutoApproveGate` |
| `crates/xaft-tui/src/state.rs` | `permission_mode` field, `Shift+Tab` handler, emit signal |
| `crates/xaft-agent/src/signals.rs` | `XaftPermissionModeChanged` |
| `crates/xaft-tui/src/prompt.rs` | `permission_mode` in `PromptState`, render chip in `build_prompt` |
| `crates/xaft-config/src/types.rs` | `TuiConfig::default_permission_mode` |
| `crates/xaft-tui/tests/permission_mode_integration.rs` | New integration tests |

---

## 7. Acceptance Criteria

| # | Criterion |
|---|-----------|
| AC1 | `Shift+Tab` cycles through Normal → AutoApprove → Bypass → Normal. |
| AC2 | The mode indicator (`⏸` / `⏵` / `⏵⏵`) and label update in the prompt immediately. |
| AC3 | In `Bypass` mode, a `bash_exec` tool call is approved without prompting the user. |
| AC4 | In `Normal` mode, a `bash_exec` tool call shows the approval prompt. |
| AC5 | In `AutoApprove` mode, a `read_file` (low-risk) is auto-approved; `bash_exec` (high-risk) still prompts. |
| AC6 | `XaftPermissionModeChanged` signal is emitted on each mode change with `changed_by = UserKeypress`. |
| AC7 | Mode resets to `Normal` (or `default_permission_mode`) at session start. |
| AC8 | `--auto-approve` flag starts in `AutoApprove` mode. |
| AC9 | A tool denied by `PermissionRegistry` session override is denied even in `Bypass` mode. |
| AC10 | An in-flight approval prompt is not retroactively resolved when mode changes. |

---

## 8. Tests

```rust
#[test]
fn permission_mode_cycles_correctly() {
    assert_eq!(PermissionMode::Normal.cycle(), PermissionMode::AutoApprove);
    assert_eq!(PermissionMode::AutoApprove.cycle(), PermissionMode::Bypass);
    assert_eq!(PermissionMode::Bypass.cycle(), PermissionMode::Normal);
}

#[tokio::test]
async fn shift_tab_cycles_permission_mode() {
    // Build AppState, press BackTab three times.
    // Assert mode returns to Normal.
}

#[tokio::test]
async fn bypass_mode_auto_approves_high_risk_tool() {
    // Set mode to Bypass. Request approval for bash_exec (High risk).
    // Assert ApprovalDecision::Approved without user prompt.
}

#[tokio::test]
async fn auto_approve_mode_prompts_for_high_risk() {
    // Set mode to AutoApprove. Request approval for bash_exec (High risk).
    // Assert approval queue receives the request (i.e., user is prompted).
}

#[tokio::test]
async fn auto_approve_mode_approves_low_risk_without_prompt() {
    // Set mode to AutoApprove. Request approval for read_file (Low risk).
    // Assert ApprovalDecision::Approved without user prompt.
}

#[tokio::test]
async fn permission_mode_changed_signal_emitted() {
    // Subscribe to XaftPermissionModeChanged.
    // Press BackTab. Assert signal received with mode = AutoApprove.
}

#[tokio::test]
async fn registry_deny_overrides_bypass_mode() {
    // PermissionRegistry: bash_exec → Deny (session override).
    // TuiApprovalGate: mode = Bypass.
    // Request approval for bash_exec.
    // Assert ApprovalDecision::Denied.
}

#[tokio::test]
async fn auto_approve_flag_starts_in_auto_approve_mode() {
    let state = AppState::for_testing(RunRequest { auto_approve: true, .. });
    assert_eq!(state.permission_mode, PermissionMode::AutoApprove);
}
```
