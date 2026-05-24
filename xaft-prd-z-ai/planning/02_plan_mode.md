# XAFT Plan Mode — PRD

> Document ID: XAFT-PLAN-002
> Version: 0.1.0-draft
> Status: Design Phase
> Owner: xaft-core team

---

## 1. Overview

Plan Mode is `xaft`'s safety-critical execution gate. Before any mutation is applied to the user's repository, the agent must first produce a plan, obtain explicit approval, and only then transition to execution. This document specifies the `PlanModeAgent` typestate architecture, `ToolCapability` filtering, the Plan→Approve→Execute lifecycle, guardrail defenses, and the TUI experience.

---

## 2. Core Principle: No Mutation Without Approval

```
 ┌──────────────────────────────────────────────────────────────┐
 │                     INVARIANT                                │
 │                                                              │
 │   A PlanModeAgent in the Planning phase MUST NOT have access │
 │   to any tool that mutates filesystem state. Only after      │
 │   explicit user approval does the agent transition to the    │
 │   Execution phase, where write-capable tools are unlocked.   │
 └──────────────────────────────────────────────────────────────┘
```

This invariant is enforced at the **type system level** via Rust typestates, making it impossible to bypass at compile time.

---

## 3. Typestate Architecture

### 3.1 State Machine

```
                 ┌──────────────┐
                 │   Planning   │
                 │  (read-only) │
                 └──────┬───────┘
                        │  plan produced
                        │  + user calls .approve()
                        ▼
                 ┌──────────────┐
                 │   Approved   │
                 │ (transition) │
                 └──────┬───────┘
                        │  .execute()
                        ▼
                 ┌──────────────┐
                 │  Executing   │
                 │ (read-write) │
                 └──────┬───────┘
                        │  all steps done / abort
                        ▼
                 ┌──────────────┐
                 │   Finished   │
                 │ (terminal)   │
                 └──────────────┘
```

### 3.2 Typestate Implementation

```rust
/// Phantom type markers for typestate enforcement.
pub mod state {
    pub struct Planning;
    pub struct Approved;
    pub struct Executing;
    pub struct Finished;
}

/// The PlanModeAgent, parameterized by its current state.
/// Different states expose different methods and tool sets.
pub struct PlanModeAgent<S> {
    inner: AgentCore,
    plan: Option<Plan>,
    capabilities: ToolCapabilitySet,
    _state: PhantomData<S>,
}

// ─── Planning State ─────────────────────────────────────────

impl PlanModeAgent<state::Planning> {
    /// Create a new agent in Planning mode with read-only tools only.
    pub fn new(config: AgentConfig) -> Self {
        let capabilities = ToolCapabilitySet::read_only();
        Self {
            inner: AgentCore::new(config),
            plan: None,
            capabilities,
            _state: PhantomData,
        }
    }

    /// The planning loop: use read-only tools to explore the repo
    /// and produce a plan. No mutations possible.
    pub async fn generate_plan(mut self, intent: &Intent) -> Result<Self, PlanError> {
        let planner = self.inner.select_planner(intent);
        let plan = planner.plan(intent).await?;

        // Validate: plan must not be empty
        if plan.steps.is_empty() {
            return Err(PlanError::EmptyPlan);
        }

        // Validate: every step must have a rollback (if configured)
        if self.inner.config.require_rollback {
            for step in &plan.steps {
                if step.rollback.is_none() {
                    return Err(PlanError::MissingRollback {
                        step_id: step.id,
                    });
                }
            }
        }

        Ok(Self {
            inner: self.inner,
            plan: Some(plan),
            capabilities: self.capabilities,
            _state: PhantomData,
        })
    }

    /// Transition to Approved state. Only callable when a plan exists.
    /// The user must explicitly invoke this (e.g., via TUI "Approve" button).
    pub fn approve(self) -> Result<PlanModeAgent<state::Approved>, PlanError> {
        let plan = self.plan.ok_or(PlanError::NoPlanGenerated)?;
        Ok(PlanModeAgent {
            inner: self.inner,
            plan: Some(plan),
            // Expand capabilities to read-write
            capabilities: ToolCapabilitySet::read_write(),
            _state: PhantomData,
        })
    }
}

// ─── Approved State ─────────────────────────────────────────

impl PlanModeAgent<state::Approved> {
    /// Begin execution of the approved plan.
    /// Transitions to Executing state.
    pub fn execute(self) -> PlanModeAgent<state::Executing> {
        PlanModeAgent {
            inner: self.inner,
            plan: self.plan,
            capabilities: self.capabilities,
            _state: PhantomData,
        }
    }

    /// Reject the plan and return to Planning.
    pub fn reject(self) -> PlanModeAgent<state::Planning> {
        PlanModeAgent {
            inner: self.inner,
            plan: None,
            capabilities: ToolCapabilitySet::read_only(),
            _state: PhantomData,
        }
    }

    /// Modify the plan (e.g., remove steps, reorder) before approving.
    pub fn modify_plan(mut self, modification: PlanModification) -> Result<Self, PlanError> {
        let plan = self.plan.as_mut().ok_or(PlanError::NoPlanGenerated)?;
        match modification {
            PlanModification::RemoveStep(step_id) => {
                plan.steps.retain(|s| s.id != step_id);
            }
            PlanModification::Reorder(step_ids) => {
                plan.reorder_steps(&step_ids)?;
            }
            PlanModification::AddConstraint(constraint) => {
                // Re-run planning with added constraint
                // (requires going back to Planning state)
                return Err(PlanError::RequiresReplan);
            }
        }
        Ok(self)
    }
}

// ─── Executing State ────────────────────────────────────────

impl PlanModeAgent<state::Executing> {
    /// Execute the plan step by step.
    pub async fn run_to_completion(mut self) -> Result<PlanModeAgent<state::Finished>, RunnerError> {
        let plan = self.plan.take().ok_or(RunnerError::NoPlan)?;
        let mut runner = TaskRunner::new(plan, self.capabilities.clone());

        let summary = runner.run().await?;

        Ok(PlanModeAgent {
            inner: self.inner,
            plan: Some(runner.into_plan()),
            capabilities: self.capabilities,
            _state: PhantomData,
        })
    }
}

// ─── Finished State ─────────────────────────────────────────

impl PlanModeAgent<state::Finished> {
    pub fn summary(&self) -> Option<&Plan> {
        self.plan.as_ref()
    }
}

// COMPILE-TIME GUARANTEE: There is no impl of PlanModeAgent<state::Planning>
// that exposes any write-capable tool. The typestate makes it impossible.
```

---

## 4. ToolCapability Filtering

### 4.1 Capability Taxonomy

```rust
/// Classification of tool capabilities for access control.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolCapability {
    // ─── Read-Only ────────────────────
    ReadFile,
    ListDirectory,
    SearchFiles,
    GitLog,
    GitDiff,
    GitStatus,

    // ─── Read-Write ───────────────────
    WriteFile,
    EditFile,
    DeleteFile,
    CreateDirectory,
    ShellCommand,
    GitCommit,
    GitCheckout,

    // ─── Meta ─────────────────────────
    Replan,
    WebFetch,
}

#[derive(Debug, Clone)]
pub struct ToolCapabilitySet(HashSet<ToolCapability>);

impl ToolCapabilitySet {
    /// Read-only capabilities: safe during Planning phase.
    pub fn read_only() -> Self {
        Self([
            ToolCapability::ReadFile,
            ToolCapability::ListDirectory,
            ToolCapability::SearchFiles,
            ToolCapability::GitLog,
            ToolCapability::GitDiff,
            ToolCapability::GitStatus,
            ToolCapability::WebFetch,
        ].into_iter().collect())
    }

    /// Full capabilities: available during Execution phase.
    pub fn read_write() -> Self {
        let mut caps = Self::read_only();
        caps.0.extend([
            ToolCapability::WriteFile,
            ToolCapability::EditFile,
            ToolCapability::DeleteFile,
            ToolCapability::CreateDirectory,
            ToolCapability::ShellCommand,
            ToolCapability::GitCommit,
            ToolCapability::GitCheckout,
            ToolCapability::Replan,
        ]);
        caps
    }

    /// Filter a tool registry, keeping only tools matching allowed capabilities.
    pub fn filter_registry(&self, registry: &ToolRegistry) -> ToolRegistry {
        let allowed: HashSet<String> = self.0
            .iter()
            .flat_map(|cap| registry.tools_for_capability(cap))
            .collect();

        registry.retain(|name| allowed.contains(name))
    }
}
```

### 4.2 Filtering Flow

```
 ┌────────────────┐     ┌──────────────────┐     ┌─────────────────────┐
 │ ToolRegistry   │────▶│  Capability      │────▶│ Filtered ToolRegistry│
 │ (all tools)    │     │  Filter          │     │ (only allowed tools) │
 └────────────────┘     └──────┬───────────┘     └─────────────────────┘
                               │
                    ┌──────────┴──────────┐
                    │                     │
               ┌────▼─────┐        ┌──────▼──────┐
               │ReadOnly  │        │  ReadWrite  │
               │Filter    │        │  Filter     │
               │(Planning)│        │  (Executing)│
               └──────────┘        └─────────────┘
```

```rust
impl AgentCore {
    /// Build the LLM agent with only the tools allowed by the current state.
    fn build_agent(&self, capabilities: &ToolCapabilitySet) -> LlmAgent {
        let full_registry = ToolRegistry::default();
        let filtered = capabilities.filter_registry(&full_registry);

        LlmAgent::builder()
            .model(self.config.model.clone())
            .system_prompt(self.system_prompt_for_state())
            .tools(filtered.into_tool_definitions())
            .build()
    }
}
```

---

## 5. Plan→Approve→Execute Transition

### 5.1 Lifecycle State Diagram

```
 ┌───────────────────────────────────────────────────────────────────┐
 │                                                                   │
 │  ┌──────────┐   generate_plan()   ┌──────────┐   approve()      │
 │  │ PLANNING │────────────────────▶│ PLANNED  │─────────────┐    │
 │  │          │                     │ (plan    │              │    │
 │  │ read-only│                     │  ready)  │              │    │
 │  └──────────┘                     └────┬─────┘              │    │
 │       ▲                                │                    │    │
 │       │  reject()                      │ modify()           ▼    │
 │       │                                │           ┌────────────┐│
 │       │                                │           │  APPROVED  ││
 │       └────────────────────────────────┘           │            ││
 │                                                    │ read-write ││
 │                                                    └─────┬──────┘│
 │                                                          │       │
 │                                               execute()  │       │
 │                                                          ▼       │
 │                                                   ┌───────────┐ │
 │                                                   │ EXECUTING │ │
 │                                                   │           │ │
 │                                                   └─────┬─────┘ │
 │                                                         │       │
 │                                              completion  │       │
 │                                                         ▼       │
 │                                                   ┌───────────┐ │
 │                                                   │ FINISHED  │ │
 │                                                   │ (terminal)│ │
 │                                                   └───────────┘ │
 └───────────────────────────────────────────────────────────────────┘
```

### 5.2 Transition Guards

Each transition has guard conditions that must be satisfied:

| Transition        | Guard Condition                                    |
|-------------------|----------------------------------------------------|
| Planning → Planned| Plan is non-empty; all steps have descriptions     |
| Planned → Approved| User explicitly approves (TUI action / CLI flag)   |
| Planned → Planning| User rejects; agent returns to plan generation     |
| Approved → Executing| Plan is still valid (intent hash matches)        |
| Executing → Finished| All steps completed or abort signal received     |

```rust
pub struct TransitionGuard {
    pub require_user_approval: bool,   // always true for Planned→Approved
    pub validate_intent_hash: bool,    // always true for Approved→Executing
    pub check_git_clean: bool,         // optional: require clean working tree
}

impl TransitionGuard {
    pub fn default() -> Self {
        Self {
            require_user_approval: true,
            validate_intent_hash: true,
            check_git_clean: false,
        }
    }

    pub fn validate_transition(
        &self,
        from: &AgentState,
        to: &AgentState,
        context: &TransitionContext,
    ) -> Result<(), TransitionError> {
        match (from, to) {
            (AgentState::Planned, AgentState::Approved) => {
                if self.require_user_approval && !context.user_approved {
                    return Err(TransitionError::ApprovalRequired);
                }
            }
            (AgentState::Approved, AgentState::Executing) => {
                if self.validate_intent_hash {
                    let current_hash = context.intent_hash();
                    let plan_hash = context.plan_intent_hash();
                    if current_hash != plan_hash {
                        return Err(TransitionError::IntentDrift {
                            expected: plan_hash,
                            actual: current_hash,
                        });
                    }
                }
                if self.check_git_clean && !context.is_git_clean()? {
                    return Err(TransitionError::DirtyWorkingTree);
                }
            }
            _ => {}
        }
        Ok(())
    }
}
```

---

## 6. PlanModeGuardrail — Defense in Depth

The typestate system is the *primary* defense. `PlanModeGuardrail` is a *secondary* runtime defense that catches any tool invocation that violates the plan-mode contract.

### 6.1 Guardrail Architecture

```
 ┌─────────────────────────────────────────────────────────────┐
 │                    PlanModeGuardrail                        │
 │                                                             │
 │   Layer 1: Typestate (compile-time)                        │
 │   ┌─────────────────────────────────────────────────────┐  │
 │   │ PlanModeAgent<Planning> has no write-capable methods │  │
 │   └─────────────────────────────────────────────────────┘  │
 │                                                             │
 │   Layer 2: Tool Registry Filter (runtime, agent-side)      │
 │   ┌─────────────────────────────────────────────────────┐  │
 │   │ LLM agent only receives tool definitions matching   │  │
 │   │ current ToolCapabilitySet                           │  │
 │   └─────────────────────────────────────────────────────┘  │
 │                                                             │
 │   Layer 3: Tool Invocation Interceptor (runtime, exec-side)│
 │   ┌─────────────────────────────────────────────────────┐  │
 │   │ Before every tool execution, check: is this tool    │  │
 │   │ allowed given the current agent state?              │  │
 │   │ Block + log if violation detected.                  │  │
 │   └─────────────────────────────────────────────────────┘  │
 │                                                             │
 │   Layer 4: Filesystem Monitor (runtime, OS-level)          │
 │   ┌─────────────────────────────────────────────────────┐  │
 │   │ inotify/FSEvents watcher on repo root               │  │
 │   │ Detect unauthorized writes during Planning phase    │  │
 │   │ Alert + abort if mutation detected                  │  │
 │   └─────────────────────────────────────────────────────┘  │
 └─────────────────────────────────────────────────────────────┘
```

### 6.2 Tool Invocation Interceptor

```rust
pub struct ToolInvocationInterceptor {
    allowed_capabilities: ToolCapabilitySet,
    audit_log: AuditLog,
    violation_policy: ViolationPolicy,
}

#[derive(Debug, Clone)]
pub enum ViolationPolicy {
    /// Block the invocation and return an error to the agent.
    Block,
    /// Block the invocation, log a warning, and trigger a replan.
    BlockAndReplan,
    /// Log only (for audit purposes) but allow the invocation.
    LogOnly,
    /// Immediately abort the entire session.
    Abort,
}

impl ToolInvocationInterceptor {
    pub fn check(&mut self, tool_name: &str, state: &AgentState) -> Result<(), GuardrailViolation> {
        let required_cap = self.infer_capability(tool_name);

        if !self.allowed_capabilities.0.contains(&required_cap) {
            let violation = GuardrailViolation {
                tool_name: tool_name.to_string(),
                required_capability: required_cap,
                current_state: state.clone(),
                timestamp: Utc::now(),
            };

            self.audit_log.record(&violation);

            match self.violation_policy {
                ViolationPolicy::Block => {
                    return Err(violation);
                }
                ViolationPolicy::BlockAndReplan => {
                    // Signal the agent to replan
                    return Err(violation);
                }
                ViolationPolicy::LogOnly => {
                    tracing::warn!("Guardrail violation (log-only): {:?}", violation);
                }
                ViolationPolicy::Abort => {
                    std::process::exit(1);
                }
            }
        }

        Ok(())
    }

    fn infer_capability(&self, tool_name: &str) -> ToolCapability {
        match tool_name {
            "read_file" | "list_directory" | "search_files" => ToolCapability::ReadFile,
            "edit_file" | "replace_block" | "apply_diff" => ToolCapability::EditFile,
            "write_file" => ToolCapability::WriteFile,
            "delete_file" => ToolCapability::DeleteFile,
            "shell_command" => ToolCapability::ShellCommand,
            "git_commit" => ToolCapability::GitCommit,
            _ => ToolCapability::ReadFile, // default to least privilege
        }
    }
}
```

### 6.3 Filesystem Monitor

```rust
pub struct FilesystemMonitor {
    repo_root: PathBuf,
    allowed_writes: HashSet<PathBuf>,
    watcher: RecommendedWatcher,
    violations: Vec<FsViolation>,
}

#[derive(Debug)]
pub struct FsViolation {
    pub path: PathBuf,
    pub event_kind: EventKind,
    pub timestamp: DateTime<Utc>,
}

impl FilesystemMonitor {
    pub async fn start(&mut self, agent_state: AgentState) -> Result<(), MonitorError> {
        let repo_root = self.repo_root.clone();
        let state = agent_state;

        self.watcher = notify::recommended_watcher(move |res| {
            if let Ok(event) = res {
                match state {
                    AgentState::Planning | AgentState::Planned => {
                        // Any write during planning is a violation
                        if event.kind.is_modify() || event.kind.is_create() {
                            tracing::error!(
                                "FS VIOLATION: Write detected during {:?} phase: {:?}",
                                state, event.paths
                            );
                        }
                    }
                    AgentState::Executing => {
                        // Writes are allowed, but only to paths in the plan
                        // (checked against allowed_writes)
                    }
                    _ => {}
                }
            }
        })?;

        self.watcher.watch(&repo_root, RecursiveMode::Recursive)?;
        Ok(())
    }
}
```

---

## 7. PlanResult and approve_and_execute()

### 7.1 PlanResult

```rust
/// The output of the planning phase, presented to the user for review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanResult {
    pub plan: Plan,
    pub risk_assessment: RiskAssessment,
    pub estimated_duration: Option<Duration>,
    pub files_affected: Vec<PathBuf>,
    pub commands_to_run: Vec<String>,
    pub rollback_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskAssessment {
    pub overall_risk: RiskLevel,
    pub step_risks: Vec<(StepId, RiskLevel)>,
    pub concerns: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskLevel {
    Low,      // Read-only operations, minor file edits
    Medium,   // Dependency changes, non-trivial refactors
    High,     // Database migrations, API signature changes
    Critical, // destructive operations, irreversible changes
}

impl PlanResult {
    /// Compute a summary for TUI display.
    pub fn summary(&self) -> PlanSummary {
        PlanSummary {
            step_count: self.plan.steps.len(),
            files_affected: self.files_affected.len(),
            highest_risk: self.risk_assessment.step_risks
                .iter()
                .map(|(_, r)| *r)
                .max()
                .unwrap_or(RiskLevel::Low),
            has_rollback: self.rollback_available,
        }
    }
}
```

### 7.2 approve_and_execute()

A convenience method that chains approval and execution in a single call, primarily for non-interactive / CI use cases.

```rust
impl PlanModeAgent<state::Planning> {
    /// Generate a plan, auto-approve, and execute.
    /// ONLY for use with --auto-approve flag or CI mode.
    /// Emits a warning to stderr.
    pub async fn approve_and_execute(
        self,
        intent: &Intent,
        auto_approve_config: &AutoApproveConfig,
    ) -> Result<PlanModeAgent<state::Finished>, PlanModeError> {
        // Step 1: Generate plan
        let planned = self.generate_plan(intent).await?;
        let plan_result = PlanResult::from_plan(planned.plan.as_ref().unwrap());

        // Step 2: Auto-approve with risk checks
        if plan_result.risk_assessment.overall_risk > auto_approve_config.max_risk {
            return Err(PlanModeError::RiskTooHigh {
                risk: plan_result.risk_assessment.overall_risk,
                max: auto_approve_config.max_risk,
            });
        }

        tracing::warn!(
            "AUTO-APPROVE: Executing plan with risk={:?}",
            plan_result.risk_assessment.overall_risk
        );

        // Step 3: Approve
        let approved = planned.approve()?;

        // Step 4: Execute
        let finished = approved.execute().run_to_completion().await?;

        Ok(finished)
    }
}

#[derive(Debug, Clone)]
pub struct AutoApproveConfig {
    /// Maximum risk level that will be auto-approved.
    pub max_risk: RiskLevel,       // default: Medium
    /// Whether to auto-approve even without rollback.
    pub allow_without_rollback: bool, // default: false
    /// Require git worktree for auto-approved execution.
    pub require_worktree: bool,    // default: true
}
```

---

## 8. Plan→Approve→Execute TUI UX

### 8.1 Planning Phase Screen

```
┌──────────────────────────────────────────────────────────────────────┐
│ xaft v0.1.0                                           [PLANNING]    │
├──────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  Intent: "Add JWT authentication to /api endpoints"                  │
│                                                                      │
│  ┌─ Agent Activity ──────────────────────────────────────────────┐  │
│  │ 🔍 Reading src/main.rs ...                                    │  │
│  │ 🔍 Scanning src/api/ directory ...                            │  │
│  │ 📖 Found auth middleware in src/middleware/auth.rs             │  │
│  │ 📖 Checking Cargo.toml for jsonwebtoken dependency ...         │  │
│  │ 🤔 Generating plan ...                                        │  │
│  └───────────────────────────────────────────────────────────────┘  │
│                                                                      │
│  ┌─ Draft Plan ──────────────────────────────────────────────────┐  │
│  │                                                                │  │
│  │  Step 1 [LOW]    Add `jsonwebtoken` to Cargo.toml             │  │
│  │  Step 2 [MEDIUM] Create src/middleware/jwt.rs                 │  │
│  │  Step 3 [MEDIUM] Integrate JWT middleware in src/api/mod.rs   │  │
│  │  Step 4 [LOW]    Add /api/login endpoint in src/api/auth.rs  │  │
│  │  Step 5 [LOW]    Add integration tests in tests/api_jwt.rs   │  │
│  │  Step 6 [LOW]    Run `cargo test` to verify                   │  │
│  │                                                                │  │
│  │  Files affected: 5  |  Risk: MEDIUM  |  Rollback: ✅          │  │
│  └───────────────────────────────────────────────────────────────┘  │
│                                                                      │
│  [A] Approve   [E] Edit Plan   [R] Reject   [Q] Quit               │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

### 8.2 Approval Confirmation

```
┌──────────────────────────────────────────────────────────────────────┐
│ xaft v0.1.0                                          [APPROVAL]     │
├──────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  ⚠️  You are about to execute a plan that modifies 5 files.         │
│                                                                      │
│  Risk Assessment:                                                    │
│    Overall:     MEDIUM                                               │
│    Step 1:      LOW       (dependency addition)                      │
│    Step 2:      MEDIUM    (new file creation)                        │
│    Step 3:      MEDIUM    (existing file modification)               │
│    Step 4:      LOW       (new endpoint)                             │
│    Step 5:      LOW       (test file creation)                       │
│    Step 6:      LOW       (shell command)                            │
│                                                                      │
│  Rollback Strategy:                                                  │
│    Steps 1-3: git restore (staged, not yet committed)               │
│    Steps 4-5: git restore (staged, not yet committed)               │
│    Step 6:   no rollback needed (read-only)                         │
│                                                                      │
│  Type "yes" to approve, or "no" to reject: _                        │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

### 8.3 Execution Phase Screen

```
┌──────────────────────────────────────────────────────────────────────┐
│ xaft v0.1.0                                         [EXECUTING]     │
├──────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  Progress: ████████░░░░░░░░  3/6 steps                              │
│                                                                      │
│  ✅ Step 1: Add `jsonwebtoken` to Cargo.toml           (0.8s)       │
│  ✅ Step 2: Create src/middleware/jwt.rs                (2.1s)       │
│  ✅ Step 3: Integrate JWT middleware in src/api/mod.rs  (1.4s)       │
│  🔄 Step 4: Add /api/login endpoint ...                (running)    │
│  ⬜ Step 5: Add integration tests                                   │
│  ⬜ Step 6: Run `cargo test`                                        │
│                                                                      │
│  ┌─ Current Step Detail ─────────────────────────────────────────┐  │
│  │ Tool: edit_file                                                │  │
│  │ Path: src/api/auth.rs                                          │  │
│  │ Action: Adding login endpoint with JWT token generation        │  │
│  │ Diff:                                                          │  │
│  │   +41  +    pub async fn login(                                │  │
│  │   +42  +        State(state): State<AppState>,                 │  │
│  │   +43  +        Json(payload): Json<LoginRequest>,             │  │
│  │   +44  +    ) -> Result<Json<TokenResponse>, ApiError> {       │  │
│  └───────────────────────────────────────────────────────────────┘  │
│                                                                      │
│  [P] Pause   [S] Skip Step   [A] Abort                              │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

---

## 9. Plan Editing in the TUI

Users can modify the plan before approval:

```rust
pub enum PlanModification {
    /// Remove a step from the plan.
    RemoveStep(StepId),
    /// Reorder steps (provide new ordering).
    Reorder(Vec<StepId>),
    /// Replace a step's description.
    EditStepDescription { step_id: StepId, new_description: String },
    /// Add a manual step.
    AddStep { after: StepId, description: String },
    /// Merge two consecutive steps.
    MergeSteps { step_a: StepId, step_b: StepId, merged_description: String },
    /// Request replan with additional constraints.
    RequestReplan { reason: String },
}
```

### 9.1 Edit Mode TUI

```
┌──────────────────────────────────────────────────────────────────────┐
│ xaft v0.1.0                                        [EDIT PLAN]     │
├──────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  ┌─ Plan Steps ──────────────────────────────────────────────────┐  │
│  │                                                                │  │
│  │  1 │ [LOW]    Add `jsonwebtoken` to Cargo.toml          [✓]  │  │
│  │  2 │ [MEDIUM] Create src/middleware/jwt.rs              [✓]  │  │
│  │  3▶│ [MEDIUM] Integrate JWT middleware in src/api/mod.rs[✓]  │  │
│  │  4 │ [LOW]    Add /api/login endpoint in src/api/auth.rs[✓]  │  │
│  │  5 │ [LOW]    Add integration tests in tests/api_jwt.rs [✓]  │  │
│  │  6 │ [LOW]    Run `cargo test` to verify                [✓]  │  │
│  │                                                                │  │
│  └───────────────────────────────────────────────────────────────┘  │
│                                                                      │
│  [↑↓] Navigate  [D] Delete  [M] Move  [E] Edit  [I] Insert After   │
│  [Enter] Accept Changes   [Esc] Cancel Edit                         │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

---

## 10. State Persistence and Recovery

### 10.1 Persistence Model

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct PlanModeSnapshot {
    pub id: Uuid,
    pub state: AgentState,
    pub intent: Intent,
    pub plan: Option<Plan>,
    pub completed_steps: Vec<StepId>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PlanModeSnapshot {
    pub fn save(&self, path: &Path) -> Result<(), SnapshotError> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self, SnapshotError> {
        let json = fs::read_to_string(path)?;
        let snapshot: Self = serde_json::from_str(&json)?;
        Ok(snapshot)
    }
}
```

### 10.2 Recovery on Crash

```
 ┌───────────────────────────────────────────────────┐
 │              Crash Recovery Flow                   │
 │                                                    │
 │  xaft starts → check .xaft/snapshots/             │
 │       │                                            │
 │       ├── No snapshot → normal start               │
 │       │                                            │
 │       └── Snapshot found →                         │
 │               │                                    │
 │               ▼                                    │
 │          ┌──────────────────┐                      │
 │          │  Validate        │                      │
 │          │  - intent hash   │                      │
 │          │  - git status    │                      │
 │          │  - completed     │                      │
 │          │    steps         │                      │
 │          └────────┬─────────┘                      │
 │                   │                                │
 │         ┌─────────┼──────────┐                     │
 │         ▼         ▼          ▼                     │
 │     [Valid]   [Dirty]   [Corrupt]                  │
 │         │         │          │                     │
 │    Resume   Rollback     Discard                   │
 │    from     completed   snapshot                   │
 │    snapshot  steps     and start                   │
 │         │    & ask       fresh                     │
 │         │    user                                 │
 │         ▼                                         │
 │    Restore agent to saved state                    │
 └───────────────────────────────────────────────────┘
```

---

## 11. Configuration

```toml
# .xaft.toml

[plan_mode]
# Require explicit user approval before execution
require_approval = true

# Allow auto-approve for CI/non-interactive mode
auto_approve = false

[plan_mode.auto_approve]
max_risk = "medium"
allow_without_rollback = false
require_worktree = true

[plan_mode.guardrails]
# Enable runtime tool invocation interception
interceptor_enabled = true
interceptor_policy = "block"       # block | block_and_replan | log_only | abort

# Enable filesystem monitoring during planning phase
fs_monitor_enabled = true

[plan_mode.snapshot]
# Enable state persistence for crash recovery
enabled = true
# Snapshot directory (relative to repo root)
directory = ".xaft/snapshots"
# Auto-save interval during execution (seconds)
auto_save_interval = 30
```

---

## 12. Security Considerations

| Threat                                | Mitigation                                         |
|---------------------------------------|----------------------------------------------------|
| LLM bypasses plan mode via prompt injection | Typestate enforcement (compile-time)           |
| Agent invokes write tool during Planning | Tool invocation interceptor (runtime, Layer 3)  |
| External process modifies files during Planning | Filesystem monitor (Layer 4)                   |
| Corrupted snapshot leads to unsafe state | Snapshot validation + intent hash verification  |
| Auto-approve in CI runs destructive plan | Risk level cap + require_worktree                |
| Agent modifies its own guardrail config | Config file is read-only during execution        |

---

## 13. Future Considerations

1. **Collaborative plan review** — Multiple users can review and vote on a plan before execution.
2. **Plan templates** — Common task patterns (add feature, fix bug, refactor) with pre-approved plan structures.
3. **Partial approval** — Approve specific steps while rejecting others, with automatic replanning.
4. **Dry-run mode** — Execute the plan against a temporary worktree, show results, then discard.
5. **Plan signing** — Cryptographic signing of approved plans for audit trails in CI environments.
