# Example Flows

## 1. Overview

This document describes six complete end-to-end flows through the xaft system. Each flow
shows the exact sequence of agent turns, tool calls, state transitions, and data
transformations that occur. These flows serve as both specification and test scenarios.

### Flow Notation

```
┌──────────┐  Agent Turn  ┌──────────┐  Tool Call   ┌──────────┐
│  Agent   │─────────────►│  LLM     │─────────────►│  Tool    │
│  State   │              │  Response│              │  Execution│
│          │◄─────────────│          │◄─────────────│          │
│  (next)  │  Updated     │  Tool    │  Tool Result │          │
└──────────┘  Context     │  Calls   │              └──────────┘
                          └──────────┘
State transitions: [Initialized] → [Planning] → [Executing] → [Validating] → [Completed]
```

---

## 2. Flow 1: Autonomous Bug Fix

**Scenario**: User reports a panic in the `parse_config` function when a required field
is missing. xaft autonomously identifies, fixes, and verifies the bug.

```
User: "Fix the panic in parse_config when 'database_url' is missing"
```

### Turn-by-Turn Execution

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ Flow 1: Autonomous Bug Fix                                                  │
│                                                                              │
│ State: [Initialized] ──► [Planning] ──► [Executing] ──► [Completed]        │
│                                                                              │
│ TURN 1 (Planning):                                                          │
│ ┌──────────────────────────────────────────────────────────────────────┐    │
│ │ Agent → LLM: "Fix the panic in parse_config when database_url       │    │
│ │              is missing"                                             │    │
│ │                                                                      │    │
│ │ LLM → Agent: "I need to read the config parsing code first."        │    │
│ │              Tool call: file_read({"path": "src/config.rs"})        │    │
│ │                                                                      │    │
│ │ Tool Result:                                                         │    │
│ │   1  pub fn parse_config(path: &Path) -> Result<Config> {           │    │
│ │   2      let content = fs::read_to_string(path)?;                   │    │
│ │   3      let value: Value = toml::from_str(&content)?;              │    │
│ │   4      Ok(Config {                                                │    │
│ │   5          database_url: value["database_url"].as_str()           │    │
│ │   6              .unwrap()  // ← PANIC HERE                        │    │
│ │   7              .to_string(),                                      │    │
│ │   8          port: value["port"].as_integer().unwrap_or(5432),      │    │
│ │   9      })                                                         │    │
│ │  10  }                                                              │    │
│ └──────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│ TURN 2 (Executing):                                                         │
│ ┌──────────────────────────────────────────────────────────────────────┐    │
│ │ Agent → LLM: [context includes file_read result]                    │    │
│ │                                                                      │    │
│ │ LLM → Agent: "The bug is on line 6 — unwrap() panics when the       │    │
│ │              field is missing. I'll fix it with proper error         │    │
│ │              handling."                                              │    │
│ │              Tool call: file_write({                                 │    │
│ │                  "path": "src/config.rs",                            │    │
│ │                  "search": "database_url: value[\"database_url\"]    │    │
│ │                           .as_str().unwrap().to_string(),",          │    │
│ │                  "replace": "database_url: value[\"database_url\"]   │    │
│ │                           .as_str()                                  │    │
│ │                           .ok_or_else(|| ConfigError::MissingField   │    │
│ │                           (\"database_url\"))?.to_string(),"         │    │
│ │              })                                                      │    │
│ │                                                                      │    │
│ │ Workspace Transaction: TX-001 begins                                 │    │
│ │   ├─ file_write(src/config.rs, ...) → ✅                            │    │
│ │   └─ TX-001 pending verification                                    │    │
│ └──────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│ TURN 3 (Validating):                                                        │
│ ┌──────────────────────────────────────────────────────────────────────┐    │
│ │ LLM → Agent: "Now let me verify the fix compiles."                  │    │
│ │              Tool call: shell_exec({                                 │    │
│ │                  "command": "cargo check",                           │    │
│ │                  "timeout": 60                                       │    │
│ │              })                                                      │    │
│ │                                                                      │    │
│ │ Tool Result: exit_code=0, stderr="" → ✅ Compilation passes         │    │
│ │                                                                      │    │
│ │ LLM → Agent: "Let me also run the existing tests."                  │    │
│ │              Tool call: shell_exec({                                 │    │
│ │                  "command": "cargo test config",                     │    │
│ │                  "timeout": 60                                       │    │
│ │              })                                                      │    │
│ │                                                                      │    │
│ │ Tool Result: exit_code=0, 3 tests passed → ✅                       │    │
│ │                                                                      │    │
│ │ Workspace: TX-001 committed ✅                                       │    │
│ │ Git: commit "fix: handle missing database_url in parse_config"       │    │
│ └──────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│ TURN 4 (Completed):                                                         │
│ ┌──────────────────────────────────────────────────────────────────────┐    │
│ │ LLM → Agent: "Fixed! The panic in parse_config has been replaced    │    │
│ │              with proper error handling using ok_or_else. The fix    │    │
│ │              compiles and all config tests pass."                    │    │
│ │                                                                      │    │
│ │ Final state: [Completed]                                            │    │
│ │ Cost: $0.008 (4 turns, ~2K tokens)                                  │    │
│ │ Duration: 12s                                                       │    │
│ │ Files modified: 1 (src/config.rs)                                   │    │
│ └──────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
```

### State Transition Diagram

```
[Initialized]
      │
      ▼ prompt received
[Planning]──────────────────┐
      │                     │
      ▼ file_read           │
[Executing]──────────────┐  │
      │                  │  │
      ▼ file_write       │  │
[Executing]──────────┐   │  │
      │              │   │  │
      ▼ cargo check  │   │  │
[Validating]─────┐  │   │  │
      │          │  │   │  │
      ▼ cargo    │  │   │  │
      test       │  │   │  │
[Validating]─┐  │  │   │  │
      │      │  │  │   │  │
      ▼      │  │  │   │  │
[Completed]◄─┘◄─┘◄─┘◄──┘◄─┘
```

---

## 3. Flow 2: Multi-File Refactor

**Scenario**: Refactor the project to replace all `unwrap()` calls with proper error
handling using `anyhow`.

```
User: "Replace all unwrap() calls with proper anyhow error handling"
```

### Turn-by-Turn Execution

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ Flow 2: Multi-File Refactor                                                │
│                                                                              │
│ Mode: Plan-and-Execute                                                       │
│                                                                              │
│ PLANNING PHASE (Turns 1-2):                                                 │
│ ┌──────────────────────────────────────────────────────────────────────┐    │
│ │ TURN 1:                                                             │    │
│ │ Tool: shell_exec("grep -rn '\.unwrap()' src/ --include='*.rs'")    │    │
│ │ Result: 23 matches across 7 files                                   │    │
│ │   src/main.rs:14   let addr = args.get(1).unwrap();                 │    │
│ │   src/config.rs:6  .as_str().unwrap()                               │    │
│ │   src/db.rs:22     pool.connect(url).unwrap()                       │    │
│ │   src/server.rs:8  listener.bind(addr).unwrap()                     │    │
│ │   ...                                                               │    │
│ │                                                                      │    │
│ │ TURN 2:                                                             │    │
│ │ LLM creates plan:                                                   │    │
│ │   Step 1: Add anyhow dependency to Cargo.toml                       │    │
│ │   Step 2: Refactor src/config.rs (3 unwrap calls)                   │    │
│ │   Step 3: Refactor src/db.rs (5 unwrap calls)                       │    │
│ │   Step 4: Refactor src/server.rs (4 unwrap calls)                   │    │
│ │   Step 5: Refactor src/main.rs (2 unwrap calls)                     │    │
│ │   Step 6: Refactor remaining files (9 unwrap calls)                 │    │
│ │   Step 7: Run cargo check after each file                           │    │
│ │   Step 8: Run cargo test to verify nothing broke                    │    │
│ └──────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│ EXECUTION PHASE (Turns 3-14):                                               │
│ ┌──────────────────────────────────────────────────────────────────────┐    │
│ │ TURN 3: Step 1 - Add anyhow                                         │    │
│ │ TX-101 begins                                                        │    │
│ │ Tool: file_write("Cargo.toml", add anyhow = "1.0" to deps)         │    │
│ │ Tool: shell_exec("cargo check") → ✅                                │    │
│ │ TX-101 committed                                                     │    │
│ │                                                                      │    │
│ │ TURN 4: Step 2 - Refactor config.rs                                 │    │
│ │ TX-102 begins                                                        │    │
│ │ Tool: file_read("src/config.rs")                                    │    │
│ │ Tool: file_write("src/config.rs", 3 replacements)                   │    │
│ │ Tool: shell_exec("cargo check") → ✅                                │    │
│ │ TX-102 committed                                                     │    │
│ │                                                                      │    │
│ │ TURN 5-6: Step 3 - Refactor db.rs                                   │    │
│ │ TX-103 begins                                                        │    │
│ │ Tool: file_read("src/db.rs")                                        │    │
│ │ Tool: file_write("src/db.rs", 5 replacements)                       │    │
│ │ Tool: shell_exec("cargo check") → ❌ ERROR                          │    │
│ │   error[E0277]: `?` can't be used in function returning _           │    │
│ │ TX-103 ROLLED BACK ← transactional safety!                          │    │
│ │                                                                      │    │
│ │ TURN 7: Retry db.rs with correct return type                        │    │
│ │ TX-104 begins                                                        │    │
│ │ Tool: file_read("src/db.rs")  // Re-read original                   │    │
│ │ Tool: file_write("src/db.rs", add -> Result<_, anyhow::Error>)      │    │
│ │ Tool: file_write("src/db.rs", 5 replacements)                       │    │
│ │ Tool: shell_exec("cargo check") → ✅                                │    │
│ │ TX-104 committed                                                     │    │
│ │                                                                      │    │
│ │ ... Turns 8-13: Continue with remaining files ...                   │    │
│ │                                                                      │    │
│ │ TURN 14: Step 8 - Final verification                                │    │
│ │ Tool: shell_exec("cargo test") → ✅ 47 tests passed                 │    │
│ │ Tool: git_commit("refactor: replace unwrap with anyhow throughout")  │    │
│ └──────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│ Final: [Completed]                                                           │
│ Cost: $0.045 (14 turns, ~15K tokens)                                        │
│ Duration: 2m 15s                                                             │
│ Files modified: 8 (7 source + Cargo.toml)                                    │
│ Unwrap calls remaining: 0 (was 23)                                           │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Key State Transitions

```rust
// The rollback in Turn 6 demonstrates xaft's transactional guarantee
AgentState::Executing   // TX-103 begins
    .transition(ExecutingTool::FileWrite)  // write db.rs
    .transition(ExecutingTool::ShellExec)  // cargo check → FAILS
    .transition(Validating)               // validation failed
    .transition(RollingBack)              // TX-103 rollback
    .transition(Executing)                // retry with corrected approach
```

---

## 4. Flow 3: Code Review with Fix

**Scenario**: Multi-agent flow where a Reviewer agent inspects code, identifies issues,
and a Fixer agent applies corrections.

```
User: "Review the authentication module and fix any security issues"
```

### Multi-Agent Execution

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ Flow 3: Code Review with Fix (Multi-Agent)                                  │
│                                                                              │
│ ┌───────────────────────────────────────────────────────────────────────┐   │
│ │ COORDINATOR AGENT                                                     │   │
│ │                                                                       │   │
│ │ Turn 1: Analyze request → delegate to Reviewer                       │   │
│ │   Delegation: Coordinator → Reviewer                                 │   │
│ │   Task: "Review src/auth/ for security vulnerabilities"              │   │
│ │                                                                       │   │
│ │ ┌─────────────────────────────────────────────────────────────────┐  │   │
│ │ │ REVIEWER AGENT (claude-sonnet-4-20250514)                      │  │   │
│ │ │                                                                 │  │   │
│ │ │ Turn 1: Read auth module files                                 │  │   │
│ │ │   Tool: file_read("src/auth/login.rs")                         │  │   │
│ │ │   Tool: file_read("src/auth/session.rs")                       │  │   │
│ │ │   Tool: file_read("src/auth/middleware.rs")                    │  │   │
│ │ │                                                                 │  │   │
│ │ │ Turn 2: Analyze for vulnerabilities                            │  │   │
│ │ │   Tool: shell_exec("grep -n 'unwrap\\|expect' src/auth/")     │  │   │
│ │ │   Tool: shell_exec("grep -n 'clone\\|copy' src/auth/")        │  │   │
│ │ │                                                                 │  │   │
│ │ │ Turn 3: Produce review report                                  │  │   │
│ │ │   Findings:                                                    │  │   │
│ │ │   1. [CRITICAL] SQL injection in login.rs:32                   │  │   │
│ │ │      format!("SELECT * FROM users WHERE name='{}'", name)      │  │   │
│ │ │   2. [HIGH] Plaintext password comparison in login.rs:45       │  │   │
│ │ │   3. [MEDIUM] Missing rate limiting on login endpoint          │  │   │
│ │ │   4. [LOW] Session token not rotated after login               │  │   │
│ │ │                                                                 │  │   │
│ │ │   Return to Coordinator: ReviewReport with 4 findings          │  │   │
│ │ └─────────────────────────────────────────────────────────────────┘  │   │
│ │                                                                       │   │
│ │ Turn 2: Process review → delegate to Fixer for CRITICAL + HIGH       │   │
│ │   Delegation: Coordinator → Fixer                                   │   │
│ │   Task: "Fix issues #1 (SQL injection) and #2 (plaintext compare)"  │   │
│ │                                                                       │   │
│ │ ┌─────────────────────────────────────────────────────────────────┐  │   │
│ │ │ FIXER AGENT (claude-sonnet-4-20250514)                         │  │   │
│ │ │                                                                 │  │   │
│ │ │ Turn 1: Read and understand the vulnerable code                │  │   │
│ │ │   Tool: file_read("src/auth/login.rs")                         │  │   │
│ │ │                                                                 │  │   │
│ │ │ Turn 2: Fix SQL injection (issue #1)                           │  │   │
│ │ │ TX-201 begins                                                   │  │   │
│ │ │   Tool: file_write("src/auth/login.rs",                        │  │   │
│ │ │     search: format!("SELECT * FROM users WHERE name='{}'", name)│  │   │
│ │ │     replace: sqlx::query("SELECT * FROM users WHERE name = $1")│  │   │
│ │ │              .bind(name)                                        │  │   │
│ │ │   )                                                             │  │   │
│ │ │                                                                 │  │   │
│ │ │ Turn 3: Fix plaintext password (issue #2)                      │  │   │
│ │ │   Tool: file_write("src/auth/login.rs",                        │  │   │
│ │ │     search: password == stored_password                        │  │   │
│ │ │     replace: verify(password, stored_hash)                      │  │   │
│ │ │   )                                                             │  │   │
│ │ │                                                                 │  │   │
│ │ │ Turn 4: Verify fixes                                           │  │   │
│ │ │   Tool: shell_exec("cargo check") → ✅                         │  │   │
│ │ │   Tool: shell_exec("cargo test auth") → ✅                     │  │   │
│ │ │   TX-201 committed                                              │  │   │
│ │ │                                                                 │  │   │
│ │ │   Return to Coordinator: FixReport (2 issues fixed)            │  │   │
│ │ └─────────────────────────────────────────────────────────────────┘  │   │
│ │                                                                       │   │
│ │ Turn 3: Summarize and finalize                                        │   │
│ │   Tool: git_commit("security: fix SQL injection and plaintext auth")  │   │
│ │   Output: "Fixed 2 critical/high security issues. 2 medium/low        │   │
│ │            issues remain for manual review."                          │   │
│ └───────────────────────────────────────────────────────────────────────┘   │
│                                                                              │
│ Delegations: 2 (Reviewer: 3 turns, Fixer: 4 turns)                          │
│ Total cost: $0.065                                                           │
│ Duration: 3m 42s                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Delegation State Machine

```rust
// Delegation transitions in the coordinator
CoordinatorState::Idle
    .transition(Delegating { to: "Reviewer", task: review_task })
    .transition(AwaitingResult { from: "Reviewer" })
    .transition(ProcessingResult { findings: vec![...] })
    .transition(Delegating { to: "Fixer", task: fix_task })
    .transition(AwaitingResult { from: "Fixer" })
    .transition(ProcessingResult { fixes: vec![...] })
    .transition(Committing)
    .transition(Completed)
```

---

## 5. Flow 4: Test Generation

**Scenario**: Generate comprehensive tests for the `UserService` struct, including
edge cases and error paths.

```
User: "Generate tests for src/services/user_service.rs"
```

### Turn-by-Turn Execution

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ Flow 4: Test Generation                                                     │
│                                                                              │
│ TURN 1: Read the source file                                                │
│ ┌──────────────────────────────────────────────────────────────────────┐    │
│ │ Tool: file_read("src/services/user_service.rs")                     │    │
│ │ Result:                                                             │    │
│ │   pub struct UserService { db: DbPool }                             │    │
│ │   impl UserService {                                                │    │
│ │     pub async fn create_user(&self, req: CreateRequest) -> Result   │    │
│ │     pub async fn get_user(&self, id: Uuid) -> Result<User>          │    │
│ │     pub async fn update_user(&self, id: Uuid, req: UpdateRequest)   │    │
│ │     pub async fn delete_user(&self, id: Uuid) -> Result<()>         │    │
│ │     pub async fn list_users(&self, filter: UserFilter) -> Vec<User> │    │
│ │   }                                                                 │    │
│ └──────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│ TURN 2: Read related types and dependencies                                 │
│ ┌──────────────────────────────────────────────────────────────────────┐    │
│ │ Tool: file_read("src/models/user.rs")                               │    │
│ │ Tool: file_read("src/requests/create_request.rs")                   │    │
│ │ Tool: file_read("src/errors.rs")                                    │    │
│ │ Tool: shell_exec("ls tests/")                                       │    │
│ │ Result: existing test patterns and conventions identified            │    │
│ └──────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│ TURN 3: Generate test file                                                  │
│ ┌──────────────────────────────────────────────────────────────────────┐    │
│ │ TX-301 begins                                                        │    │
│ │ Tool: file_write("tests/user_service_test.rs", ...)                 │    │
│ │                                                                      │    │
│ │ Generated tests:                                                     │    │
│ │   ✅ create_user_success                                             │    │
│ │   ✅ create_user_duplicate_email                                     │    │
│ │   ✅ create_user_invalid_email                                       │    │
│ │   ✅ create_user_missing_required_fields                             │    │
│ │   ✅ get_user_existing                                               │    │
│ │   ✅ get_user_nonexistent                                            │    │
│ │   ✅ update_user_success                                             │    │
│ │   ✅ update_user_partial_update                                      │    │
│ │   ✅ update_user_nonexistent                                         │    │
│ │   ✅ delete_user_existing                                            │    │
│ │   ✅ delete_user_nonexistent                                         │    │
│ │   ✅ list_users_empty                                                │    │
│ │   ✅ list_users_with_filter                                          │    │
│ │   ✅ list_users_pagination                                           │    │
│ └──────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│ TURN 4: Verify tests compile and pass                                       │
│ ┌──────────────────────────────────────────────────────────────────────┐    │
│ │ Tool: shell_exec("cargo test user_service --no-run") → ✅           │    │
│ │ Tool: shell_exec("cargo test user_service") → ✅ 14/14 passed      │    │
│ │ TX-301 committed                                                     │    │
│ │ Tool: git_commit("test: add comprehensive user service tests")       │    │
│ └──────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│ Final: [Completed]                                                           │
│ Cost: $0.032 (4 turns, ~8K tokens)                                          │
│ Tests generated: 14                                                          │
│ Coverage improvement: +23% (estimated)                                       │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 6. Flow 5: Dependency Upgrade

**Scenario**: Upgrade the `tokio` dependency from 1.35 to 1.40, handling any
breaking changes.

```
User: "Upgrade tokio from 1.35 to 1.40"
```

### Turn-by-Turn Execution

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ Flow 5: Dependency Upgrade                                                  │
│                                                                              │
│ Mode: Plan-and-Execute                                                       │
│                                                                              │
│ PLANNING PHASE (Turns 1-2):                                                 │
│ ┌──────────────────────────────────────────────────────────────────────┐    │
│ │ TURN 1: Assess current state                                        │    │
│ │ Tool: file_read("Cargo.toml")                                       │    │
│ │   → tokio = { version = "1.35", features = ["full"] }               │    │
│ │ Tool: shell_exec("cargo tree -i tokio")                             │    │
│ │   → 4 crates depend on tokio                                        │    │
│ │ Tool: shell_exec("cargo outdated tokio")                            │    │
│ │   → 1.40 available, breaking changes in io::AsyncRead              │    │
│ │                                                                      │    │
│ │ TURN 2: Create upgrade plan                                         │    │
│ │ Plan:                                                                │    │
│ │   Step 1: Update Cargo.toml version                                 │    │
│ │   Step 2: Run cargo check — identify compilation errors             │    │
│ │   Step 3: Fix each compilation error                                │    │
│ │   Step 4: Run cargo test to verify no regressions                   │    │
│ │   Step 5: Check for deprecation warnings and fix them               │    │
│ └──────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│ EXECUTION PHASE (Turns 3-8):                                                │
│ ┌──────────────────────────────────────────────────────────────────────┐    │
│ │ TURN 3: Update version                                              │    │
│ │ TX-401 begins                                                        │    │
│ │ Tool: file_write("Cargo.toml", tokio = "1.40")                     │    │
│ │ Tool: shell_exec("cargo update tokio") → ✅                         │    │
│ │                                                                      │    │
│ │ TURN 4: Check compilation                                           │    │
│ │ Tool: shell_exec("cargo check 2>&1") → ❌ 3 errors                 │    │
│ │   error: `AsyncRead::read` now returns `io::Result<usize>`         │    │
│ │          instead of `Poll<Result<usize>>`                           │    │
│ │   error: `tokio::spawn` requires `'static` — new borrow checker    │    │
│ │          rules                                                       │    │
│ │   error: deprecated `tokio::io::BufStream` removed                 │    │
│ │                                                                      │    │
│ │ TURN 5: Fix AsyncRead migration                                     │    │
│ │ Tool: file_read("src/io/async_reader.rs")                          │    │
│ │ Tool: file_write("src/io/async_reader.rs", update read signature)   │    │
│ │                                                                      │    │
│ │ TURN 6: Fix spawn 'static issue                                     │    │
│ │ Tool: file_read("src/tasks/manager.rs")                             │    │
│ │ Tool: file_write("src/tasks/manager.rs", add 'static bound)         │    │
│ │                                                                      │    │
│ │ TURN 7: Fix BufStream removal                                       │    │
│ │ Tool: file_read("src/io/buffered.rs")                               │    │
│ │ Tool: file_write("src/io/buffered.rs", replace BufStream with       │    │
│ │                   BufReader<BufWriter<T>>)                           │    │
│ │                                                                      │    │
│ │ TURN 8: Verify everything works                                     │    │
│ │ Tool: shell_exec("cargo check") → ✅                                │    │
│ │ Tool: shell_exec("cargo test") → ✅ 47/47 passed                   │    │
│ │ Tool: shell_exec("cargo clippy") → ✅ no warnings                  │    │
│ │ TX-401 committed                                                     │    │
│ │ Tool: git_commit("chore: upgrade tokio 1.35 → 1.40")               │    │
│ └──────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│ Final: [Completed]                                                           │
│ Cost: $0.052 (8 turns, ~12K tokens)                                         │
│ Duration: 3m 05s                                                             │
│ Files modified: 4 (Cargo.toml + 3 source files)                             │
│ Breaking changes resolved: 3                                                 │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 7. Flow 6: PR Generation

**Scenario**: Create a complete pull request for a feature: adding rate limiting
to the API endpoints.

```
User: "Add rate limiting to all API endpoints and create a PR"
```

### Turn-by-Turn Execution

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ Flow 6: PR Generation                                                       │
│                                                                              │
│ Mode: Plan-and-Execute with Git workflow                                    │
│                                                                              │
│ PLANNING (Turns 1-2):                                                       │
│ ┌──────────────────────────────────────────────────────────────────────┐    │
│ │ TURN 1: Understand the codebase                                     │    │
│ │ Tool: file_read("Cargo.toml")  → identify existing deps             │    │
│ │ Tool: shell_exec("ls src/routes/")  → list API routes               │    │
│ │ Tool: file_read("src/routes/mod.rs")  → route definitions           │    │
│ │ Tool: file_read("src/main.rs")  → server setup                      │    │
│ │                                                                      │    │
│ │ TURN 2: Create plan                                                 │    │
│ │ Plan:                                                                │    │
│ │   Step 1: Create feature branch feat/rate-limiting                  │    │
│ │   Step 2: Add tower-governor dependency                             │    │
│ │   Step 3: Create src/middleware/rate_limit.rs                       │    │
│ │   Step 4: Integrate rate limiting in main.rs                        │    │
│ │   Step 5: Add per-route rate limit configuration                    │    │
│ │   Step 6: Add tests for rate limiting                               │    │
│ │   Step 7: Run cargo check + cargo test                              │    │
│ │   Step 8: Generate PR description and push                          │    │
│ └──────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│ EXECUTION (Turns 3-10):                                                     │
│ ┌──────────────────────────────────────────────────────────────────────┐    │
│ │ TURN 3: Create branch                                               │    │
│ │ Tool: git_checkout("-b feat/rate-limiting")                         │    │
│ │                                                                      │    │
│ │ TURN 4: Add dependency + create rate limit module                   │    │
│ │ TX-501 begins                                                        │    │
│ │ Tool: file_write("Cargo.toml", add tower-governor = "0.4")          │    │
│ │ Tool: file_write("src/middleware/rate_limit.rs", new file)           │    │
│ │   → RateLimiterConfig struct with per-route limits                  │    │
│ │   → RateLimitLayer tower middleware                                  │    │
│ │   → Key extraction (IP-based, API-key-based)                        │    │
│ │                                                                      │    │
│ │ TURN 5: Integrate into main.rs                                      │    │
│ │ Tool: file_write("src/main.rs", add rate limit layer to router)     │    │
│ │ Tool: file_write("src/middleware/mod.rs", add pub mod rate_limit)   │    │
│ │                                                                      │    │
│ │ TURN 6: Configure per-route limits                                  │    │
│ │ Tool: file_write("src/config.rs", add RateLimitConfig)              │    │
│ │   → login: 5 req/min                                               │    │
│ │   → api endpoints: 100 req/min                                      │    │
│ │   → health: unlimited                                               │    │
│ │                                                                      │    │
│ │ TURN 7: Add tests                                                   │    │
│ │ Tool: file_write("tests/rate_limit_test.rs", ...)                   │    │
│ │   → test_rate_limit_enforced                                        │    │
│ │   → test_rate_limit_per_ip                                          │    │
│ │   → test_rate_limit_headers_present                                 │    │
│ │   → test_health_endpoint_unlimited                                  │    │
│ │                                                                      │    │
│ │ TURN 8: Verify                                                       │    │
│ │ Tool: shell_exec("cargo check") → ✅                                │    │
│ │ Tool: shell_exec("cargo test") → ✅ 51/51 passed                   │    │
│ │ Tool: shell_exec("cargo clippy") → ✅                               │    │
│ │ TX-501 committed                                                     │    │
│ │                                                                      │    │
│ │ TURN 9: Commit changes                                              │    │
│ │ Tool: git_add(["."])                                                │    │
│ │ Tool: git_commit("feat: add rate limiting to API endpoints")         │    │
│ │                                                                      │    │
│ │ TURN 10: Generate and push PR                                       │    │
│ │ Tool: shell_exec("git push -u origin feat/rate-limiting")           │    │
│ │ Tool: shell_exec("gh pr create --title 'feat: add rate limiting'     │    │
│ │                   --body '...' ")                                    │    │
│ │   PR body includes:                                                  │    │
│ │   - Summary of changes                                              │    │
│ │   - New dependencies                                                │    │
│ │   - Rate limit configuration table                                  │    │
│ │   - Test coverage summary                                           │    │
│ │   - Breaking changes: none                                          │    │
│ └──────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│ Final: [Completed]                                                           │
│ Cost: $0.078 (10 turns, ~18K tokens)                                        │
│ Duration: 4m 30s                                                             │
│ Files modified: 5, files created: 2                                         │
│ PR: https://github.com/org/repo/pull/42                                     │
└─────────────────────────────────────────────────────────────────────────────┘
```

### PR Body Generation

```rust
/// The PR body is generated from the collected signals during execution
pub fn generate_pr_description(result: &ExecutionResult) -> String {
    let mut body = String::new();

    // Summary
    body.push_str("## Summary\n\n");
    body.push_str(&format!("{}\n\n", result.original_prompt));

    // Changes
    body.push_str("## Changes\n\n");
    for file in &result.modified_files {
        let stats = result.diff_stats_for(file);
        body.push_str(&format!("- `{}` (+{}/-{})\n", file.display(), stats.additions, stats.deletions));
    }
    for file in &result.created_files {
        body.push_str(&format!("- `{}` (new file)\n", file.display()));
    }

    // New dependencies
    if !result.new_dependencies.is_empty() {
        body.push_str("\n## New Dependencies\n\n");
        for dep in &result.new_dependencies {
            body.push_str(&format!("- {} = \"{}\"\n", dep.name, dep.version));
        }
    }

    // Tests
    body.push_str("\n## Tests\n\n");
    body.push_str(&format!("- {} new tests added\n", result.new_tests_count));
    body.push_str(&format!("- All {} tests passing\n", result.total_tests_passed));

    // Cost
    body.push_str(&format!("\n---\n*Generated by xaft | Cost: ${:.4} | {} turns*\n",
        result.total_cost, result.turn_count));

    body
}
```

---

## 8. Cross-Flow Comparison

| Metric             | Bug Fix | Multi-File Refactor | Review+Fix | Test Gen | Dep Upgrade | PR Gen  |
|--------------------|---------|---------------------|------------|----------|-------------|---------|
| **Turns**          | 4       | 14                  | 7+4+3      | 4        | 8           | 10      |
| **Cost**           | $0.008  | $0.045              | $0.065     | $0.032   | $0.052      | $0.078  |
| **Duration**       | 12s     | 2m 15s              | 3m 42s     | 1m 20s   | 3m 05s      | 4m 30s  |
| **Files Modified** | 1       | 8                   | 1          | 0 (new)  | 4           | 5+2 new |
| **Rollbacks**      | 0       | 1                   | 0          | 0        | 0           | 0       |
| **Mode**           | Direct  | Plan-Execute        | Multi-Agent| Direct   | Plan-Execute| Plan+Git|
| **Agents Used**    | 1       | 1                   | 3          | 1        | 1           | 1       |

These six flows demonstrate the breadth of xaft's capabilities across different
execution modes, complexity levels, and agent configurations. Each flow leverages
xaft's core strengths: transactional editing for safety, compile-time validation
for correctness, and the SignalBus for full observability.
