# PRD: Approval & Safety Systems

> xaft — Autonomous Coding CLI built on agtrs
> Document: `safety/01_approval_safety.md`
> Version: 0.1.0-draft

---

## 1. Overview

xaft operates as a semi-autonomous agent: it must reason, plan, and act on the
user's codebase, but **every destructive or high-impact action must pass through
a layered approval pipeline** before execution. This document specifies the
ApprovalGate trait, the `requires_confirmation` attribute, risk-level
determination heuristics, the interactive approval UX flow, PlanMode
enforcement, the defense-in-depth guardrail stack, and user budget controls.

---

## 2. ApprovalGate Trait

The `ApprovalGate` is the central abstraction in `agtrs` that intercepts tool
invocations and decides whether to allow, deny, or defer them. Every tool the
agent can call is routed through an `ApprovalGate` implementation before the
underlying function executes.

### 2.1 Trait Definition

```rust
/// Core approval gate that every tool invocation must pass through.
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    /// Evaluate a pending tool call and return a disposition.
    ///
    /// This method MUST be safe to call concurrently from multiple
    /// agent tasks. Implementations must be stateless or use interior
    /// mutability (e.g., `Arc<Mutex<_>>`).
    async fn evaluate(&self, call: &ToolCall) -> GateDisposition;

    /// Called after a tool execution completes, allowing the gate
    /// to update internal risk models or rate counters.
    async fn post_exec(&self, call: &ToolCall, result: &ToolResult) {
        // default: no-op
    }

    /// Return the gate's identity for logging and audit trails.
    fn name(&self) -> &str;
}

/// Possible outcomes of gate evaluation.
#[derive(Debug, Clone)]
pub enum GateDisposition {
    /// Allow the tool call to proceed immediately.
    Allow,

    /// Deny the call; return a structured error to the agent.
    Deny { reason: String },

    /// Defer to the user for interactive confirmation.
    Confirm {
        prompt: ConfirmationPrompt,
        /// If the user declines, this fallback action is taken.
        on_deny: DenyFallback,
    },

    /// Allow but with modifications to the call arguments.
    Mutate {
        modified_call: ToolCall,
        mutation_reason: String,
    },
}

/// What happens when a user denies a confirmation prompt.
#[derive(Debug, Clone)]
pub enum DenyFallback {
    /// Return an error to the agent so it can re-plan.
    ReturnError(String),
    /// Silently skip the call and return an empty result.
    Skip,
    /// Abort the entire task.
    AbortTask,
}
```

### 2.2 Gate Composition

Multiple gates are composed into a **gate chain**. Each gate in the chain
evaluates the call in order. The first non-`Allow` disposition wins. If all
gates return `Allow`, the call proceeds.

```
┌─────────────┐   ┌──────────────────┐   ┌───────────────┐   ┌──────────────┐
│ RiskLevelGate│──▶│ ConfirmationGate │──▶│ BudgetGate    │──▶│ GuardrailGate│──▶ EXECUTE
└─────────────┘   └──────────────────┘   └───────────────┘   └──────────────┘
  classify risk     require user confirm   check user budget    input/output
  Allow/Deny/Confirm allow/deny/confirm    allow/deny           guardrails
```

```rust
pub struct ChainedApprovalGate {
    gates: Vec<Arc<dyn ApprovalGate>>,
}

#[async_trait]
impl ApprovalGate for ChainedApprovalGate {
    async fn evaluate(&self, call: &ToolCall) -> GateDisposition {
        for gate in &self.gates {
            match gate.evaluate(call).await {
                GateDisposition::Allow => continue,
                other => return other, // first non-Allow wins
            }
        }
        GateDisposition::Allow // all gates approved
    }

    async fn post_exec(&self, call: &ToolCall, result: &ToolResult) {
        for gate in &self.gates {
            gate.post_exec(call, result).await;
        }
    }

    fn name(&self) -> &str { "chained" }
}
```

---

## 3. `requires_confirmation` Attribute

Tool functions can be annotated with `#[requires_confirmation]` to declare that
they **must** pass through interactive user approval before execution. This is
processed at compile time by the `agtrs` proc-macro crate.

### 3.1 Attribute Syntax

```rust
/// Delete a file from the workspace.
#[requires_confirmation(risk = "high", reason = "Irreversible file deletion")]
#[tool(name = "delete_file", description = "Delete a file by path")]
async fn delete_file(ctx: &ToolContext, path: String) -> Result<ToolOutput, ToolError> {
    let sanitized = ctx.workspace.sanitize_path(&path)?;
    tokio::fs::remove_file(&sanitized).await?;
    Ok(ToolOutput::text(format!("Deleted {}", sanitized.display())))
}

/// Read a file — no confirmation needed.
#[tool(name = "read_file", description = "Read file contents")]
async fn read_file(ctx: &ToolContext, path: String) -> Result<ToolOutput, ToolError> {
    // Low-risk: auto-allowed
    let sanitized = ctx.workspace.sanitize_path(&path)?;
    let content = tokio::fs::read_to_string(&sanitized).await?;
    Ok(ToolOutput::text(content))
}
```

### 3.2 Macro Expansion

The `#[requires_confirmation]` macro wraps the tool function:

```rust
// Pseudo-expansion of #[requires_confirmation(risk = "high", reason = "...")]
async fn delete_file(ctx: &ToolContext, path: String) -> Result<ToolOutput, ToolError> {
    let call = ToolCall::new("delete_file", vec![("path", &path)]);
    match ctx.approval_gate.evaluate(&call).await {
        GateDisposition::Allow => { /* proceed to real impl */ },
        GateDisposition::Deny { reason } => {
            return Err(ToolError::Denied { reason });
        },
        GateDisposition::Confirm { prompt, on_deny } => {
            let user_response = ctx.ux.prompt_confirmation(prompt).await?;
            if !user_response.approved {
                return match on_deny {
                    DenyFallback::ReturnError(msg) => Err(ToolError::Denied { reason: msg }),
                    DenyFallback::Skip => Ok(ToolOutput::text("Skipped by user.")),
                    DenyFallback::AbortTask => Err(ToolError::AbortTask),
                };
            }
        },
        GateDisposition::Mutate { modified_call, .. } => {
            // Re-extract arguments from modified_call
        },
    }
    // ── original function body ──
    let sanitized = ctx.workspace.sanitize_path(&path)?;
    tokio::fs::remove_file(&sanitized).await?;
    Ok(ToolOutput::text(format!("Deleted {}", sanitized.display())))
}
```

---

## 4. Risk Level Determination

Every tool call is classified into one of four risk levels. This classification
determines which gates apply and whether interactive confirmation is required.

### 4.1 Risk Matrix

```
┌──────────────────────────────────────────────────────────────────────┐
│                        RISK LEVEL MATRIX                            │
├─────────┬────────────────────────────────────────────────────────────┤
│ Level   │ Examples                                                  │
├─────────┼────────────────────────────────────────────────────────────┤
│ none    │ read_file, search, list_directory                        │
│ low     │ write_file (inside workspace), git_add, npm_install       │
│ medium  │ shell_exec (read-only commands), git_commit, create_file  │
│ high    │ delete_file, shell_exec (write commands), git_push,       │
│         │ net_request (POST/PUT/DELETE), env_var_set                │
│ critical│ shell_exec (rm -rf /, sudo), credential_write,            │
│         │ net_request (external prod URL), factory_reset            │
└─────────┴────────────────────────────────────────────────────────────┘
```

### 4.2 RiskClassifier

```rust
pub struct RiskClassifier {
    /// Static risk table: tool_name -> default_risk
    static_risks: HashMap<String, RiskLevel>,
    /// Dynamic heuristics that can elevate risk at runtime
    heuristics: Vec<Box<dyn RiskHeuristic>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskLevel {
    None = 0,
    Low = 1,
    Medium = 2,
    High = 3,
    Critical = 4,
}

impl RiskClassifier {
    pub fn classify(&self, call: &ToolCall) -> RiskLevel {
        let base = self.static_risks
            .get(&call.tool_name)
            .copied()
            .unwrap_or(RiskLevel::High); // default: high for unknown tools

        // Apply heuristics — any heuristic can elevate (never lower) risk
        let mut final_risk = base;
        for heuristic in &self.heuristics {
            let elevated = heuristic.evaluate(call);
            final_risk = final_risk.max(elevated);
        }
        final_risk
    }
}

/// Heuristic: elevate risk for shell commands containing dangerous patterns.
pub struct DangerousCommandHeuristic;

impl RiskHeuristic for DangerousCommandHeuristic {
    fn evaluate(&self, call: &ToolCall) -> RiskLevel {
        if call.tool_name != "shell_exec" { return RiskLevel::None; }
        let cmd = call.arg("command").unwrap_or("");
        let dangerous = ["rm -rf", "sudo", "chmod 777", "> /dev/sda",
                         "mkfs", "dd if=", ":(){ :|:& };:"];
        if dangerous.iter().any(|d| cmd.contains(d)) {
            RiskLevel::Critical
        } else if cmd.contains("rm ") || cmd.contains("mv ") {
            RiskLevel::High
        } else {
            RiskLevel::None
        }
    }
}

/// Heuristic: elevate risk for network requests to non-allowlisted domains.
pub struct NetworkDomainHeuristic {
    pub allowlist: Vec<String>,
}

impl RiskHeuristic for NetworkDomainHeuristic {
    fn evaluate(&self, call: &ToolCall) -> RiskLevel {
        if call.tool_name != "net_request" { return RiskLevel::None; }
        let url = call.arg("url").unwrap_or("");
        if self.allowlist.iter().any(|d| url.contains(d)) {
            RiskLevel::Low
        } else {
            RiskLevel::High
        }
    }
}
```

### 4.3 Risk-to-Gate Mapping

```
RiskLevel    →  ConfirmationGate    BudgetGate     GuardrailGate
─────────────────────────────────────────────────────────────────
none         →  auto-allow          skip           input only
low          →  auto-allow (log)    check          input + output
medium       →  prompt_if_plan_mode check          input + output
high         →  always prompt       check + warn   input + output
critical     →  always prompt +     hard block     full stack
                show diff + require
                explicit YES
```

---

## 5. Approval UX Flow

When a tool call requires interactive confirmation, xaft presents a rich
terminal UI for the user to review, approve, modify, or deny the action.

### 5.1 Flow Diagram

```
Agent calls tool
       │
       ▼
┌──────────────┐
│ RiskClassifier│
└──────┬───────┘
       │
       ▼
  risk_level?
       │
  ┌────┼────────────┬──────────────┐
  │    │            │              │
none  low        medium         high/critical
  │    │            │              │
  ▼    ▼            ▼              ▼
auto  auto     ┌─────────┐   ┌──────────────┐
exec  exec     │PlanMode?│   │ Show diff /  │
  │    │       └────┬────┘   │ full context │
  │    │        ┌───┴───┐    └──────┬───────┘
  │    │       Yes      No          │
  │    │        │        │          ▼
  │    │        ▼        ▼    ┌───────────────┐
  │    │   ┌─────────┐ auto │  User Prompt   │
  │    │   │  Prompt  │ exec │  [y/n/e/d]    │
  │    │   │  [y/n]  │      │  y=yes n=no   │
  │    │   └────┬────┘      │  e=edit d=diff│
  │    │        │            └───────┬───────┘
  │    │   ┌────┴────┐           ┌───┴───┐
  │    │  Yes       No          Yes     No/Edit
  │    │   │         │           │       │
  ▼    ▼   ▼         ▼           ▼       ▼
EXEC EXEC EXEC    ReturnError  EXEC  Mutate/Abort
```

### 5.2 ConfirmationPrompt

```rust
pub struct ConfirmationPrompt {
    /// The tool being called
    pub tool_name: String,
    /// Human-readable description of what will happen
    pub description: String,
    /// The risk level that triggered confirmation
    pub risk_level: RiskLevel,
    /// Optional diff preview (for file edits)
    pub diff_preview: Option<String>,
    /// Optional command preview (for shell_exec)
    pub command_preview: Option<String>,
    /// Available responses
    pub options: Vec<PromptOption>,
}

pub enum PromptOption {
    /// Approve the action
    Yes,
    /// Deny the action
    No,
    /// Edit the arguments before approving
    Edit,
    /// Show a full diff preview
    Diff,
    /// Approve all subsequent calls of this type (auto-escalate trust)
    YesAll,
    /// Abort the entire task
    Abort,
}
```

### 5.3 Terminal Rendering

```
╔══════════════════════════════════════════════════════════════╗
║  ⚠️  CONFIRMATION REQUIRED  [risk: HIGH]                    ║
╠══════════════════════════════════════════════════════════════╣
║                                                              ║
║  Tool:    delete_file                                        ║
║  Reason:  Irreversible file deletion                         ║
║  Target:  src/main.rs                                        ║
║                                                              ║
║  ─── diff ─────────────────────────────────────────────────  ║
║  - use xaft::prelude::*;                                    ║
║  -                                                          ║
║  - fn main() {                                              ║
║  -     println!("hello");                                   ║
║  - }                                                        ║
║  ─── end diff ────────────────────────────────────────────  ║
║                                                              ║
║  [y] Yes  [n] No  [e] Edit  [d] Diff  [A] Yes-to-all       ║
║  [x] Abort task                                             ║
╚══════════════════════════════════════════════════════════════╝
```

### 5.4 Non-Interactive Mode

When running with `--yes` or in CI, the approval gate uses a configurable
policy:

```rust
pub enum NonInteractivePolicy {
    /// Auto-approve all actions (dangerous, CI only)
    AutoApprove,
    /// Auto-deny all actions requiring confirmation
    AutoDeny,
    /// Only allow actions up to a max risk level
    AutoApproveBelow { max_risk: RiskLevel },
    /// Fail the task if any confirmation would be needed
    FailOnConfirm,
}
```

---

## 6. PlanMode Enforcement

PlanMode is a special operating mode where the agent **only plans** but never
executes. It is enforced through the approval gate stack.

### 6.1 PlanMode Gate

```rust
pub struct PlanModeGate {
    /// Whether PlanMode is active
    active: AtomicBool,
}

#[async_trait]
impl ApprovalGate for PlanModeGate {
    async fn evaluate(&self, call: &ToolCall) -> GateDisposition {
        if !self.active.load(Ordering::Relaxed) {
            return GateDisposition::Allow;
        }

        // In PlanMode, only read-only and planning tools are allowed
        match call.tool_name.as_str() {
            "read_file" | "search" | "list_directory" | "plan" | "think" => {
                GateDisposition::Allow
            }
            _ => {
                GateDisposition::Deny {
                    reason: format!(
                        "PlanMode is active: '{}' is a write operation. \
                         Use `xaft apply` to execute the plan.",
                        call.tool_name
                    ),
                }
            }
        }
    }

    fn name(&self) -> &str { "plan_mode" }
}
```

### 6.2 Plan → Apply Workflow

```
┌──────────┐    ┌───────────┐    ┌──────────┐
│  xaft    │    │  xaft     │    │  xaft    │
│  plan    │───▶│  review   │───▶│  apply   │
│          │    │  (user)   │    │          │
└──────────┘    └───────────┘    └──────────┘
  PlanMode ON     User edits       PlanMode OFF
  Read-only       the plan         Executes plan
  No mutations    Approves/        with normal
  Generates       denies steps     approval flow
  plan.json       per-step
```

### 6.3 PlanArtifact

```rust
#[derive(Serialize, Deserialize)]
pub struct PlanArtifact {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub steps: Vec<PlanStep>,
    pub estimated_risk: RiskLevel,
    pub estimated_tool_calls: usize,
    pub estimated_duration: Duration,
}

#[derive(Serialize, Deserialize)]
pub struct PlanStep {
    pub index: usize,
    pub tool: String,
    pub args: serde_json::Value,
    pub risk: RiskLevel,
    pub rationale: String,
    pub rollback_hint: Option<String>,
}
```

---

## 7. Guardrails: Defense-in-Depth

Guardrails operate at the **content** level — they inspect the *inputs to* and
*outputs from* tools and the LLM itself. They form a secondary defense layer
behind the approval gates.

### 7.1 Guardrail Trait Hierarchy

```
                    ┌─────────────┐
                    │  Guardrail  │  (base trait)
                    └──────┬──────┘
                           │
              ┌────────────┼────────────┐
              │            │            │
    ┌─────────▼──┐  ┌──────▼──────┐  ┌─▼────────────┐
    │InputGuard- │  │OutputGuard- │  │ToolCallGuard-│
    │  rail      │  │  rail       │  │  rail         │
    └────────────┘  └─────────────┘  └───────────────┘
    LLM prompts      LLM responses    tool call/result
    before send       after receive    inspection
```

### 7.2 Guardrail Trait Definitions

```rust
#[async_trait]
pub trait InputGuardrail: Send + Sync {
    /// Inspect and optionally modify the prompt before sending to LLM.
    async fn check_input(&self, prompt: &mut LlmPrompt) -> GuardrailAction;
}

#[async_trait]
pub trait OutputGuardrail: Send + Sync {
    /// Inspect the LLM output before the agent processes it.
    async fn check_output(&self, output: &LlmOutput) -> GuardrailAction;
}

#[async_trait]
pub trait ToolCallGuardrail: Send + Sync {
    /// Inspect a tool call before execution.
    async fn check_tool_call(&self, call: &ToolCall) -> GuardrailAction;

    /// Inspect a tool result before returning to agent.
    async fn check_tool_result(&self, result: &ToolResult) -> GuardrailAction;
}

#[derive(Debug)]
pub enum GuardrailAction {
    /// Allow the content through unchanged
    Allow,
    /// Allow but with modifications
    Modify { content: String, reason: String },
    /// Block the content
    Block { reason: String },
    /// Block and warn the user
    BlockAndWarn { reason: String, severity: Severity },
}

#[derive(Debug)]
pub enum Severity { Low, Medium, High, Critical }
```

### 7.3 Built-in Guardrails

```rust
/// Prevents the agent from leaking sensitive data (API keys, tokens)
/// in tool arguments or LLM outputs.
pub struct SensitiveDataGuardrail {
    patterns: Vec<Regex>,
}

impl SensitiveDataGuardrail {
    pub fn new() -> Self {
        let patterns = vec![
            Regex::new(r"(?i)(api[_-]?key|token|secret|password)\s*[:=]\s*\S+").unwrap(),
            Regex::new(r"sk-[a-zA-Z0-9]{20,}").unwrap(),           // OpenAI keys
            Regex::new(r"ghp_[a-zA-Z0-9]{36}").unwrap(),           // GitHub PATs
            Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),              // AWS keys
        ];
        Self { patterns }
    }
}

/// Prevents the agent from executing commands targeting paths
/// outside the workspace root.
pub struct PathTraversalGuardrail;

impl ToolCallGuardrail for PathTraversalGuardrail {
    async fn check_tool_call(&self, call: &ToolCall) -> GuardrailAction {
        let path_args = ["path", "file_path", "directory", "dest"];
        for arg in &path_args {
            if let Some(val) = call.arg(arg) {
                if val.contains("..") || val.starts_with('/') {
                    return GuardrailAction::Block {
                        reason: format!(
                            "Path traversal detected in '{}': '{}'. \
                             All paths must be relative to workspace root.",
                            arg, val
                        ),
                    };
                }
            }
        }
        GuardrailAction::Allow
    }

    async fn check_tool_result(&self, _result: &ToolResult) -> GuardrailAction {
        GuardrailAction::Allow
    }
}
```

### 7.4 Defense-in-Depth Stack

```
USER INPUT
    │
    ▼
┌──────────────────────┐
│  InputGuardrails     │  ── sanitize user prompt
│  • PromptInjection   │  ── detect injection attempts
│  • LengthLimit       │  ── enforce max token count
└──────────┬───────────┘
           ▼
┌──────────────────────┐
│  ApprovalGate Chain  │  ── risk classification
│  • RiskLevelGate     │  ── confirmation prompts
│  • PlanModeGate      │  ── plan-mode blocking
│  • BudgetGate        │  ── spend limits
└──────────┬───────────┘
           ▼
┌──────────────────────┐
│  ToolCallGuardrails  │  ── path traversal check
│  • PathTraversal     │  ── sensitive data redaction
│  • SensitiveData     │  ── command allowlist
│  • CommandAllowlist  │
└──────────┬───────────┘
           ▼
       TOOL EXEC
           │
           ▼
┌──────────────────────┐
│  ToolResultGuardrails│  ── redact secrets from output
│  • OutputSanitize    │  ── size limits
│  • SizeLimit         │
└──────────┬───────────┘
           ▼
┌──────────────────────┐
│  OutputGuardrails    │  ── prevent harmful agent responses
│  • OutputInjection   │  ── PII detection
│  • PIIFilter         │
└──────────┬───────────┘
           ▼
       USER OUTPUT
```

---

## 8. User Budget Guardrail

The `UserBudgetGuardrail` prevents cost overruns by tracking LLM token usage
and enforcing hard/soft limits per task, per session, and per user.

### 8.1 Budget Model

```rust
pub struct UserBudgetGuardrail {
    /// Per-task budget
    task_budget: TokenBudget,
    /// Per-session (multiple tasks) budget
    session_budget: TokenBudget,
    /// Per-user (persistent, across sessions) budget
    user_budget: TokenBudget,
    /// Current counters
    task_usage: AtomicU64,
    session_usage: AtomicU64,
    user_usage: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct TokenBudget {
    /// Soft limit: warn the user
    pub soft_limit: u64,
    /// Hard limit: block further LLM calls
    pub hard_limit: u64,
    /// Token type being tracked
    pub token_type: TokenType,
}

#[derive(Debug, Clone, Copy)]
pub enum TokenType {
    Input,
    Output,
    Total,
    CostDollars,  // approximate cost tracking
}

#[derive(Debug)]
pub struct BudgetStatus {
    pub task_used: u64,
    pub task_limit: u64,
    pub session_used: u64,
    pub session_limit: u64,
    pub user_used: u64,
    pub user_limit: u64,
    pub is_exceeded: bool,
    pub is_warning: bool,
}
```

### 8.2 Budget Enforcement

```rust
impl InputGuardrail for UserBudgetGuardrail {
    async fn check_input(&self, prompt: &mut LlmPrompt) -> GuardrailAction {
        let estimated_tokens = prompt.estimated_tokens();
        let task_used = self.task_usage.load(Ordering::Relaxed);
        let session_used = self.session_usage.load(Ordering::Relaxed);
        let user_used = self.user_usage.load(Ordering::Relaxed);

        // Hard limit check
        if task_used + estimated_tokens > self.task_budget.hard_limit
            || session_used + estimated_tokens > self.session_budget.hard_limit
            || user_used + estimated_tokens > self.user_budget.hard_limit
        {
            return GuardrailAction::BlockAndWarn {
                reason: "Budget exceeded. Further LLM calls are blocked.".into(),
                severity: Severity::Critical,
            };
        }

        // Soft limit check
        if task_used + estimated_tokens > self.task_budget.soft_limit
            || session_used + estimated_tokens > self.session_budget.soft_limit
        {
            // Allow but warn — emit a telemetry event and log
            return GuardrailAction::Modify {
                content: prompt.content.clone(),
                reason: "Approaching budget limit. Consider winding down.".into(),
            };
        }

        GuardrailAction::Allow
    }
}
```

### 8.3 Budget UX

```
╔═══════════════════════════════════════════════════╗
║  💰 Budget Status                                  ║
╠═══════════════════════════════════════════════════╣
║                                                    ║
║  Task:    [████████████░░░░░░]  62% (31k/50k)     ║
║  Session: [██████░░░░░░░░░░░░]  30% (90k/300k)    ║
║  User:    [██░░░░░░░░░░░░░░░░]  10% (1M/10M)     ║
║                                                    ║
║  Estimated cost this task: $0.42 / $0.75          ║
╚═══════════════════════════════════════════════════╝
```

---

## 9. Audit Trail

Every gate evaluation and guardrail check is recorded to an immutable audit log:

```rust
pub struct AuditEntry {
    pub timestamp: DateTime<Utc>,
    pub task_id: Uuid,
    pub gate_name: String,
    pub tool_name: String,
    pub risk_level: RiskLevel,
    pub disposition: GateDisposition,
    pub user_response: Option<bool>,
    pub token_count: Option<u64>,
}

/// Append-only audit log backed by a local file or remote sink.
pub struct AuditLog {
    sink: Box<dyn AuditSink>,
}

impl AuditLog {
    pub async fn record(&self, entry: AuditEntry) -> Result<()> {
        self.sink.write(entry).await
    }
}
```

---

## 10. Configuration

All approval and safety settings are configurable via `xaft.toml`:

```toml
[safety]
# Maximum risk level allowed without confirmation
auto_approve_below = "low"

# Non-interactive policy (for CI)
non_interactive = "fail_on_confirm"

[safety.budget]
task_soft_limit = 50_000
task_hard_limit = 100_000
session_soft_limit = 300_000
session_hard_limit = 500_000
user_hard_limit = 10_000_000

[safety.gate_chain]
# Order matters: first non-Allow wins
gates = ["risk_level", "plan_mode", "budget", "guardrail"]

[safety.guardrails]
enable_path_traversal = true
enable_sensitive_data = true
enable_command_allowlist = true
enable_pii_filter = true

[safety.guardrails.command_allowlist]
allow = ["ls", "cat", "grep", "cargo", "npm", "git", "python"]
deny = ["sudo", "rm -rf /", "mkfs", "dd"]
```

---

## 11. Open Questions

| # | Question | Status |
|---|----------|--------|
| 1 | Should `Yes-to-all` persist across tasks within a session? | Open |
| 2 | Risk classification for custom user-defined tools — API or config? | Open |
| 3 | Should the audit log support remote attestation? | Deferred |
| 4 | Budget sharing across parallel sub-agents? | Open |
| 5 | How to handle guardrail conflicts (two guardrails disagree)? | Open |
| 6 | Should `#[requires_confirmation]` support conditional risk (e.g., risk depends on args)? | Planned |
| 7 | MCP tool risk classification — trust on first use or deny by default? | Open |
