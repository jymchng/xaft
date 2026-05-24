# Data Flow Diagrams

## 1. Overview

This document presents five key data flow scenarios as ASCII sequence diagrams.
Each diagram traces the complete path of data through the xaft system, from user
input to final result, including all intermediate transformations and side effects.

---

## 2. Scenario 1: Single-Turn Agent Execution

The most basic flow: a user provides a prompt, the agent makes one LLM call,
executes one tool, and returns a result.

```
User           CLI            Session        AgentExecutor   LlmClient    Transport   ToolRouter   FileEditor   Workspace   SignalBus
 │              │               │               │              │            │            │            │           │           │
 │  "Read       │               │               │              │            │            │            │           │           │
 │   main.rs"   │               │               │              │            │            │            │           │           │
 │─────────────►│               │               │              │            │            │            │           │           │
 │              │  parse args   │               │              │            │            │            │           │           │
 │              │  resolve cfg  │               │              │            │            │            │           │           │
 │              │──────────────►│               │              │            │            │            │           │           │
 │              │               │  create        │              │            │            │            │           │           │
 │              │               │  session       │              │            │            │            │           │           │
 │              │               │───────────────►│              │            │            │            │           │           │
 │              │               │               │              │            │            │            │           │           │
 │              │               │               │ build prompt │            │            │            │           │           │
 │              │               │               │──────────────►│            │            │            │           │           │
 │              │               │               │              │ POST       │            │            │           │           │
 │              │               │               │              │ /messages  │            │            │           │           │
 │              │               │               │              │───────────►│            │            │           │           │
 │              │               │               │              │            │            │            │           │           │
 │              │               │               │              │  200 OK    │            │            │           │           │
 │              │               │               │              │◄───────────│            │            │           │           │
 │              │               │               │  LlmResponse │            │            │            │           │           │
 │              │               │               │◄──────────────│            │            │            │           │           │
 │              │               │               │              │            │            │            │           │           │
 │              │               │               │  emit:       │            │            │            │           │           │
 │              │               │               │  LlmResponse │            │            │            │           │           │
 │              │               │               │────────────────────────────────────────────────────────────────────────────►│
 │              │               │               │              │            │            │            │           │           │
 │              │               │               │  tool_call:  │            │            │            │           │           │
 │              │               │               │  file_read   │            │            │            │           │           │
 │              │               │               │──────────────────────────────────────►│            │           │           │
 │              │               │               │              │            │            │            │           │           │
 │              │               │               │              │            │  dispatch   │            │           │           │
 │              │               │               │              │            │───────────►│            │           │           │
 │              │               │               │              │            │            │            │           │           │
 │              │               │               │              │            │            │  read_file │           │           │
 │              │               │               │              │            │            │───────────►│           │           │
 │              │               │               │              │            │            │            │           │           │
 │              │               │               │              │            │            │            │  read     │           │
 │              │               │               │              │            │            │            │──────────►│           │
 │              │               │               │              │            │            │            │           │           │
 │              │               │               │              │            │            │            │  content  │           │
 │              │               │               │              │            │            │            │◄──────────│           │
 │              │               │               │              │            │            │  result    │           │           │
 │              │               │               │              │            │            │◄───────────│           │           │
 │              │               │               │  ToolResult  │            │            │            │           │           │
 │              │               │               │◄─────────────────────────────│            │            │           │           │
 │              │               │               │              │            │            │            │           │           │
 │              │               │               │  emit:       │            │            │            │           │           │
 │              │               │               │  ToolComplete│            │            │            │           │           │
 │              │               │               │────────────────────────────────────────────────────────────────────────────►│
 │              │               │               │              │            │            │            │           │           │
 │              │               │               │              │            │            │            │           │    TUI    │
 │              │               │               │              │            │            │            │           │  render   │
 │              │               │               │              │            │            │            │           │◄──────────│
 │              │               │               │              │            │            │            │           │           │
 │              │               │  result       │              │            │            │            │           │           │
 │              │               │◄──────────────│              │            │            │            │           │           │
 │              │  output       │               │              │            │            │            │           │           │
 │              │◄──────────────│               │              │            │            │            │           │           │
 │  display     │               │               │              │            │            │            │           │           │
 │◄─────────────│               │               │              │            │            │            │           │           │
 │              │               │               │              │            │            │            │           │           │
```

---

## 3. Scenario 2: Multi-Turn Execution with Transactional Rollback

A multi-turn agent flow where a compilation check fails, triggering a transactional
rollback of file changes, followed by a corrected retry.

```
AgentExecutor   LlmClient   ToolRouter   FileEditor   Workspace   GitRepo   SignalBus   BudgetTracker
     │              │            │            │           │          │          │            │
     │  Turn 1:     │            │            │           │          │          │            │
     │  read files  │            │            │           │          │          │            │
     │─────────────►│            │            │           │          │          │            │
     │◄─────────────│            │            │           │          │          │            │
     │              │            │            │           │          │          │            │
     │  Turn 2:     │            │            │           │          │          │            │
     │  plan edits  │            │            │           │          │          │            │
     │─────────────►│            │            │           │          │          │            │
     │◄─────────────│            │            │           │          │          │            │
     │  tool_calls: │            │            │           │          │          │            │
     │  [file_write,│            │            │           │          │          │            │
     │   file_write]│            │            │           │          │          │            │
     │              │            │            │           │          │          │            │
     │  TX begin ────────────────────────────────────────►│          │          │            │
     │              │            │            │           │ TX-001   │          │            │
     │              │            │            │  snapshot │ created  │          │            │
     │              │            │            │◄──────────│          │          │            │
     │              │            │            │           │          │          │            │
     │  write file A ──────────────────────►│           │          │          │            │
     │              │            │           │──────────►│          │          │            │
     │              │            │           │           │ emit:    │          │            │
     │              │            │           │           │ FileWritten          │            │
     │              │            │           │           │──────────────────────►            │
     │              │            │           │           │          │          │            │
     │  write file B ──────────────────────►│           │          │          │            │
     │              │            │           │──────────►│          │          │            │
     │              │            │           │           │          │          │            │
     │  Turn 3:     │            │            │           │          │          │            │
     │  verify      │            │            │           │          │          │            │
     │  shell_exec  │            │            │           │          │          │            │
     │─────────────────────────►│            │           │          │          │            │
     │              │            │  "cargo    │           │          │          │            │
     │              │            │   check"   │           │          │          │            │
     │              │            │────────────│───────────│─────────►│          │            │
     │              │            │            │           │          │          │            │
     │              │            │  exit=1    │           │          │          │            │
     │              │            │  ERROR:    │           │          │          │            │
     │              │            │  type      │           │          │          │            │
     │              │            │  mismatch  │           │          │          │            │
     │◄──────────────────────────│            │           │          │          │            │
     │              │            │            │           │          │          │            │
     │  ┌─────────────────────────────────────────────────────────────────────────────┐    │
     │  │ VALIDATION FAILED — ROLLBACK                                               │    │
     │  │                                                                             │    │
     │  │  state: Executing ──► Validating ──► RollingBack                           │    │
     │  └─────────────────────────────────────────────────────────────────────────────┘    │
     │              │            │            │           │          │          │            │
     │  TX rollback ─────────────────────────────────────────────────►          │            │
     │              │            │            │  restore │          │          │            │
     │              │            │            │◄─────────│          │          │            │
     │              │            │            │           │          │          │            │
     │  emit:       │            │            │           │          │          │            │
     │  Rollback    │            │            │           │          │          │            │
     │──────────────────────────────────────────────────────────────────────────►          │
     │              │            │            │           │          │          │            │
     │  Turn 4:     │            │            │           │          │          │            │
     │  retry with  │            │            │           │          │          │            │
     │  corrected   │            │            │           │          │          │            │
     │  approach    │            │            │           │          │          │            │
     │─────────────►│            │            │           │          │          │            │
     │◄─────────────│            │            │           │          │          │            │
     │              │            │            │           │          │          │            │
     │  TX-002 begin ───────────────────────────────────────────────►          │            │
     │              │            │            │           │          │          │            │
     │  write file A│            │            │           │          │          │            │
     │  (corrected) │            │            │           │          │          │            │
     │──────────────────────────────────────►│           │          │          │            │
     │              │            │           │──────────►│          │          │            │
     │              │            │            │           │          │          │            │
     │  shell_exec  │            │            │           │          │          │            │
     │  "cargo      │            │            │           │          │          │            │
     │   check"     │            │            │           │          │          │            │
     │─────────────────────────►│            │           │          │          │            │
     │              │            │  exit=0 ✅ │           │          │          │            │
     │◄──────────────────────────│            │           │          │          │            │
     │              │            │            │           │          │          │            │
     │  TX-002      │            │            │           │          │          │            │
     │  commit ─────────────────────────────────────────────────────►          │            │
     │              │            │            │           │ emit:    │          │            │
     │              │            │            │           │ Cost     │          │            │
     │              │            │            │           │──────────────────────────────────►│
     │              │            │            │           │          │          │   check    │
     │              │            │            │           │          │          │   budget   │
     │              │            │            │           │          │          │            │
     │  git commit  │            │            │           │          │          │            │
     │─────────────────────────────────────────────────────────────────────►     │            │
     │              │            │            │           │          │          │            │
     │  [Completed] │            │            │           │          │          │            │
```

---

## 4. Scenario 3: Plan-and-Execute Mode

The agent first creates a plan, then executes it step-by-step, with the ability
to modify the plan if a step fails.

```
User      CLI      AgentExecutor   Planner    LlmClient   ToolRouter   Workspace   SignalBus
 │         │            │            │           │            │           │          │
 │ "Refactor│            │            │           │            │           │          │
 │  error   │            │            │           │            │           │          │
 │  handling│            │            │           │            │           │          │
 │─────────►│            │            │           │            │           │          │
 │         │───────────►│            │           │            │           │          │
 │         │            │            │           │            │           │          │
 │         │            │  PLAN PHASE            │            │           │          │
 │         │            │───────────►│           │            │           │          │
 │         │            │            │           │            │           │          │
 │         │            │            │ analyze   │            │           │          │
 │         │            │            │ codebase  │            │           │          │
 │         │            │            │──────────►│            │           │          │
 │         │            │            │◄──────────│            │           │          │
 │         │            │            │           │            │           │          │
 │         │            │            │ create    │            │           │          │
 │         │            │            │ plan      │            │           │          │
 │         │            │            │──────────►│            │           │          │
 │         │            │            │◄──────────│            │           │          │
 │         │            │            │           │            │           │          │
 │         │            │  Plan {    │           │            │           │          │
 │         │            │    steps:  │           │            │           │          │
 │         │            │    [1. Add │           │            │           │          │
 │         │            │     anyhow │           │            │           │          │
 │         │            │    2. Fix  │           │            │           │          │
 │         │            │     config│           │            │           │          │
 │         │            │    3. Fix  │           │            │           │          │
 │         │            │     db.rs  │           │            │           │          │
 │         │            │    4. Fix  │           │            │           │          │
 │         │            │     server│           │            │           │          │
 │         │            │    5. Run  │           │            │           │          │
 │         │            │     tests] │           │            │           │          │
 │         │            │  }        │           │            │           │          │
 │         │            │◄───────────│           │            │           │          │
 │         │            │            │           │            │           │          │
 │         │            │  emit: PlanCreated      │            │           │          │
 │         │            │───────────────────────────────────────────────────────────►│
 │         │            │            │           │            │           │          │
 │         │            │  state: Planning ──► Executing      │           │          │
 │         │            │            │           │            │           │          │
 │         │            │  EXECUTE PHASE                      │           │          │
 │         │            │            │           │            │           │          │
 │         │            │  Step 1: Add anyhow      │            │           │          │
 │         │            │───────────────────────────────────►│           │          │
 │         │            │            │           │            │  write    │          │
 │         │            │            │           │            │──────────►│          │
 │         │            │  emit: PlanStepCompleted│           │            │           │          │
 │         │            │───────────────────────────────────────────────────────────►│
 │         │            │            │           │            │           │          │
 │         │            │  Step 2: Fix config.rs   │            │           │          │
 │         │            │───────────────────────────────────►│           │          │
 │         │            │            │           │            │  write    │          │
 │         │            │            │           │            │──────────►│          │
 │         │            │            │           │            │           │          │
 │         │            │  Step 3: Fix db.rs ──── FAIL! ─────│           │          │
 │         │            │  compilation error after edit       │           │          │
 │         │            │            │           │            │  rollback │          │
 │         │            │            │           │            │◄──────────│          │
 │         │            │            │           │            │           │          │
 │         │            │  PLAN MODIFICATION                  │           │          │
 │         │            │───────────►│           │            │           │          │
 │         │            │            │ modify    │            │           │          │
 │         │            │            │ step 3:   │            │           │          │
 │         │            │            │ add return│            │           │          │
 │         │            │            │ type first│            │           │          │
 │         │            │            │──────────►│            │           │          │
 │         │            │            │◄──────────│            │           │          │
 │         │            │            │           │            │           │          │
 │         │            │  emit: PlanModified      │            │           │          │
 │         │            │───────────────────────────────────────────────────────────►│
 │         │            │            │           │            │           │          │
 │         │            │  Retry Step 3 (modified) │           │           │          │
 │         │            │───────────────────────────────────►│           │          │
 │         │            │            │           │            │  write    │          │
 │         │            │            │           │            │──────────►│          │
 │         │            │            │           │            │  verify   │          │
 │         │            │            │           │            │──────────►│          │
 │         │            │  ✅ Step 3 complete     │            │           │          │
 │         │            │            │           │            │           │          │
 │         │            │  Step 4: Fix server.rs  │           │           │          │
 │         │            │───────────────────────────────────►│           │          │
 │         │            │  ✅ Step 4 complete     │           │           │          │
 │         │            │            │           │            │           │          │
 │         │            │  Step 5: Run tests       │           │           │          │
 │         │            │───────────────────────────────────►│           │          │
 │         │            │  ✅ All tests pass       │           │           │          │
 │         │            │            │           │            │           │          │
 │         │            │  [Completed]             │           │           │          │
 │  result │            │            │           │            │           │          │
 │◄────────│◄───────────│            │           │            │           │          │
 │         │            │            │           │            │           │          │
```

---

## 5. Scenario 4: Multi-Agent Delegation

A coordinator agent delegates to specialized sub-agents, collecting results
and synthesizing a final answer.

```
User      AgentExecutor   CoordinatorAgent   LlmClient   ReviewerAgent   FixerAgent   SignalBus   BudgetTracker
 │             │               │               │              │              │           │            │
 │ "Review     │               │               │              │              │           │            │
 │  and fix   │               │               │              │              │           │            │
 │  security" │               │               │              │              │           │            │
 │────────────►│               │               │              │              │           │            │
 │            │  delegate to   │               │              │              │           │            │
 │            │  coordinator   │               │              │              │           │            │
 │            │──────────────►│               │              │              │           │            │
 │            │               │               │              │              │           │            │
 │            │               │  analyze task  │              │              │           │            │
 │            │               │──────────────►│              │              │           │            │
 │            │               │◄──────────────│              │              │           │            │
 │            │               │               │              │              │           │            │
 │            │               │  LLM decides:  │              │              │           │            │
 │            │               │  "delegate to  │              │              │           │            │
 │            │               │   reviewer"    │              │              │           │            │
 │            │               │               │              │              │           │            │
 │            │               │  emit: DelegationInitiated    │              │           │            │
 │            │               │──────────────────────────────────────────────────────────────────►│
 │            │               │               │              │              │           │            │
 │            │               │  delegate()   │              │              │           │            │
 │            │               │──────────────│──────────────►│              │           │            │
 │            │               │               │              │              │           │            │
 │            │               │               │              │  read files  │           │            │
 │            │               │               │              │─────────────►│           │            │
 │            │               │               │              │              │           │            │
 │            │               │               │              │  analyze for │           │            │
 │            │               │               │              │  vulns       │           │            │
 │            │               │               │              │─────────────►│           │            │
 │            │               │               │              │              │           │            │
 │            │               │               │              │  emit: cost  │           │            │
 │            │               │               │              │─────────────────────────────────────────────────►│
 │            │               │               │              │              │           │   $0.025   │
 │            │               │               │              │              │           │   (total:  │
 │            │               │               │              │              │           │    $0.025)  │
 │            │               │               │              │              │           │            │
 │            │               │               │              │  ReviewReport│           │            │
 │            │               │               │              │  - SQL inject│           │            │
 │            │               │               │              │  - plaintext │           │            │
 │            │               │◄─────────────│──────────────│              │           │            │
 │            │               │               │              │              │           │            │
 │            │               │  emit: DelegationCompleted   │              │           │            │
 │            │               │──────────────────────────────────────────────────────────────────►│
 │            │               │               │              │              │           │            │
 │            │               │  LLM decides:  │              │              │           │            │
 │            │               │  "delegate to  │              │              │           │            │
 │            │               │   fixer for    │              │              │           │            │
 │            │               │   critical     │              │              │           │            │
 │            │               │   issues"      │              │              │           │            │
 │            │               │               │              │              │           │            │
 │            │               │  delegate()   │              │              │           │            │
 │            │               │──────────────│──────────────│──────────────►│           │            │
 │            │               │               │              │              │           │            │
 │            │               │               │              │              │  TX-301   │            │
 │            │               │               │              │              │  begin    │            │
 │            │               │               │              │              │──────────►│            │
 │            │               │               │              │              │           │            │
 │            │               │               │              │              │  fix SQL  │            │
 │            │               │               │              │              │  injection│            │
 │            │               │               │              │              │──────────►│            │
 │            │               │               │              │              │           │            │
 │            │               │               │              │              │  fix      │            │
 │            │               │               │              │              │  plaintext│            │
 │            │               │               │              │              │──────────►│            │
 │            │               │               │              │              │           │            │
 │            │               │               │              │              │  verify   │            │
 │            │               │               │              │              │──────────►│            │
 │            │               │               │              │              │           │            │
 │            │               │               │              │              │  TX-301   │            │
 │            │               │               │              │              │  commit   │            │
 │            │               │               │              │              │──────────►│            │
 │            │               │               │              │              │           │            │
 │            │               │               │              │              │  emit:    │            │
 │            │               │               │              │              │  cost     │            │
 │            │               │               │              │              │─────────────────────────────────────────►│
 │            │               │               │              │              │           │   $0.040   │
 │            │               │               │              │              │           │   (total:  │
 │            │               │               │              │              │           │    $0.065)  │
 │            │               │               │              │              │           │            │
 │            │               │  FixReport    │              │              │           │            │
 │            │               │◄─────────────│──────────────│──────────────│           │            │
 │            │               │               │              │              │           │            │
 │            │               │  synthesize   │              │              │           │            │
 │            │               │  final result  │              │              │           │            │
 │            │               │──────────────►│              │              │           │            │
 │            │               │◄──────────────│              │              │           │            │
 │            │               │               │              │              │           │            │
 │            │  result       │               │              │              │           │            │
 │            │◄──────────────│               │              │              │           │            │
 │  output    │               │               │              │              │           │            │
 │◄───────────│               │               │              │              │           │            │
 │            │               │               │              │              │           │            │
```

### Delegation Data Flow Detail

```rust
/// Data flow during a delegation
///
/// Coordinator                          Reviewer
///     │                                    │
///     │  DelegationRequest {               │
///     │    task: "Review for security",    │
///     │    context: project_summary,       │
///     │    files_to_review: [...],         │
///     │    budget: 1.0,                    │
///     │    max_turns: 10                   │
///     │  }                                 │
///     │───────────────────────────────────►│
///     │                                    │
///     │                  ... execution ... │
///     │                                    │
///     │  DelegationResult {                │
///     │    findings: [                     │
///     │      Finding { severity: Critical, │
///     │        file: "login.rs:32",       │
///     │        description: "SQL injection"│
///     │      },                            │
///     │      ...                           │
///     │    ],                              │
///     │    cost: 0.025,                    │
///     │    turns_used: 3                   │
///     │  }                                 │
///     │◄───────────────────────────────────│
///     │                                    │
```

---

## 6. Scenario 5: Cost-Tracked Streaming Execution

A streaming LLM response where tokens are counted in real-time, costs are tracked
incrementally, and budget enforcement triggers a warning mid-stream.

```
AgentExecutor   LlmClient   Transport   BudgetTracker   SignalBus   TUI
     │              │            │            │             │         │
     │  send prompt │            │            │             │         │
     │─────────────►│            │            │             │         │
     │              │  POST      │            │             │         │
     │              │  stream:   │            │             │         │
     │              │  true      │            │             │         │
     │              │───────────►│            │             │         │
     │              │            │            │             │         │
     │              │  SSE chunk │            │             │         │
     │              │  1: "I'll" │            │             │         │
     │              │◄───────────│            │             │         │
     │  stream      │            │            │             │         │
     │  chunk 1     │            │            │             │         │
     │◄─────────────│            │            │             │         │
     │              │            │            │             │         │
     │  emit:       │            │            │             │         │
     │  StreamChunk │            │            │             │         │
     │──────────────────────────────────────►│             │         │
     │              │            │            │             │         │
     │              │  SSE chunk │            │             │         │
     │              │  2: " fix" │            │             │         │
     │              │◄───────────│            │             │         │
     │  stream      │            │            │             │         │
     │  chunk 2     │            │            │             │         │
     │◄─────────────│            │            │             │         │
     │              │            │            │             │         │
     │              │  SSE chunk │            │             │         │
     │              │  3: " the" │            │             │         │
     │              │◄───────────│            │             │         │
     │              │            │            │             │         │
     │              │  ...more   │            │             │         │
     │              │  chunks... │            │             │         │
     │              │            │            │             │         │
     │              │  SSE: done │            │             │         │
     │              │  usage: {  │            │             │         │
     │              │    in: 1240│            │             │         │
     │              │    out: 890│            │             │         │
     │              │  }         │            │             │         │
     │              │◄───────────│            │             │         │
     │              │            │            │             │         │
     │  final       │            │            │             │         │
     │  response    │            │            │             │         │
     │◄─────────────│            │            │             │         │
     │              │            │            │             │         │
     │  record cost │            │            │             │         │
     │─────────────────────────────────────►│             │         │
     │              │            │            │             │         │
     │              │            │  total:    │             │         │
     │              │            │  $4.75     │             │         │
     │              │            │  limit:    │             │         │
     │              │            │  $5.00     │             │         │
     │              │            │  95% used! │             │         │
     │              │            │            │             │         │
     │              │            │  emit:     │             │         │
     │              │            │  BudgetWarning           │         │
     │              │            │  95%      │             │         │
     │              │            │──────────►│             │         │
     │              │            │            │             │         │
     │              │            │            │             │ render  │
     │              │            │            │             │ warning │
     │              │            │            │             │────────►│
     │              │            │            │             │         │
     │  ⚠️ Budget at 95%. Remaining: $0.25  │             │         │
     │  Agent will attempt to complete in 1-2 more turns.   │         │
     │              │            │            │             │         │
     │  execute     │            │            │             │         │
     │  tool calls  │            │            │             │         │
     │  from response           │            │             │         │
     │──────────────────────────────────────────────────►  │         │
     │              │            │            │             │         │
     │  next LLM    │            │            │             │         │
     │  call...     │            │            │             │         │
     │─────────────►│            │            │             │         │
     │              │            │            │             │         │
     │  final       │            │            │             │         │
     │  cost:       │            │            │             │         │
     │  $4.92       │            │            │             │         │
     │─────────────────────────────────────►│             │         │
     │              │            │  remaining:│             │         │
     │              │            │  $0.08     │             │         │
     │              │            │  (below    │             │         │
     │              │            │  threshold)│             │         │
     │              │            │            │             │         │
     │  emit:       │            │            │             │         │
     │  CostAccumul.│            │            │             │         │
     │──────────────────────────────────────────────────►  │         │
     │              │            │            │             │         │
     │  [Completed] │            │            │             │         │
     │  total cost: │            │            │             │         │
     │  $4.92 / $5.00           │            │             │         │
```

---

## 7. Cross-Scenario Data Flow Summary

```
┌────────────────────────────────────────────────────────────────────────────────┐
│                    Data Flow Patterns Across Scenarios                         │
│                                                                                │
│  Pattern 1: Request-Response                                                   │
│  ─────────────────────                                                        │
│  User → CLI → AgentExecutor → LlmClient → Transport → LLM API                │
│  User ← CLI ← AgentExecutor ← LlmClient ← Transport ← LLM API               │
│                                                                                │
│  Pattern 2: Tool Dispatch                                                      │
│  ──────────────────                                                           │
│  AgentExecutor → ToolRouter → [FileEditor | ShellExec | GitOps | WASM]        │
│  AgentExecutor ← ToolRouter ← [Workspace | Process | libgit2 | wasmtime]      │
│                                                                                │
│  Pattern 3: Transactional Edit                                                 │
│  ──────────────────────                                                       │
│  AgentExecutor → TX begin → Workspace (snapshot) → Tool (write) → Verify     │
│  AgentExecutor ← TX commit ← Workspace (persist) ← Success                   │
│  OR                                                                            │
│  AgentExecutor ← TX rollback ← Workspace (restore) ← Failure                 │
│                                                                                │
│  Pattern 4: Signal Emission                                                    │
│  ────────────────────                                                         │
│  Any subsystem → SignalBus.emit(Signal) → broadcast to subscribers             │
│  TUI / DebugLog / CostTracker / PerfProfiler ← SignalBus.subscribe()           │
│                                                                                │
│  Pattern 5: Delegation                                                         │
│  ───────────────                                                              │
│  Coordinator → DelegationMgr → SubAgentExecutor → (recursive execution)       │
│  Coordinator ← DelegationMgr ← SubAgentExecutor ← (DelegationResult)         │
│                                                                                │
│  Pattern 6: Budget Enforcement                                                 │
│  ──────────────────────                                                       │
│  LlmClient → BudgetTracker.record(usage) → check limit                        │
│  BudgetTracker → SignalBus.emit(BudgetWarning) → TUI displays warning         │
│  BudgetTracker → Err(BudgetExceeded) → AgentExecutor halts                    │
└────────────────────────────────────────────────────────────────────────────────┘
```

### Data Transformation Chain

```
┌──────────────────────────────────────────────────────────────────────┐
│                  Data Transformation Chain                            │
│                                                                       │
│  User Prompt (String)                                                │
│       │                                                              │
│       ▼ PromptBuilder                                                │
│  LlmRequest { messages, tools, model, max_tokens }                  │
│       │                                                              │
│       ▼ LlmClient.send()                                            │
│  HTTP Request (bytes)                                                │
│       │                                                              │
│       ▼ Transport (network)                                          │
│  HTTP Response (bytes)                                               │
│       │                                                              │
│       ▼ LlmClient (parse)                                            │
│  LlmResponse { content, tool_calls, usage, stop_reason }            │
│       │                                                              │
│       ▼ AgentExecutor (dispatch)                                     │
│  ToolCall { name, parameters: Value }                                │
│       │                                                              │
│       ▼ ToolRouter (validate + execute)                              │
│  ToolResult { output: String, duration, is_error }                   │
│       │                                                              │
│       ▼ AgentExecutor (accumulate)                                   │
│  Turn { request, response, tool_calls, duration, token_usage }       │
│       │                                                              │
│       ▼ AgentExecutor (loop or terminate)                            │
│  ExecutionResult { turns, files_modified, total_cost, final_state }  │
│       │                                                              │
│       ▼ CLI / TUI                                                    │
│  User-visible output                                                 │
└──────────────────────────────────────────────────────────────────────┘
```

These five scenarios cover the complete range of xaft's data flow patterns, from
simple single-turn execution to complex multi-agent delegation with cost tracking.
Each scenario demonstrates how data flows through the layered architecture, with
clear boundaries between subsystems and well-defined interfaces at each layer.
