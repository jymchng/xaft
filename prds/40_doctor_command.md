# PRD 40 — `/doctor` Command: Diagnostics Panel

## Status: Draft
## Depends on: PRD 32a–32e, PRD 22 (costed provider)

---

## 1. Problem

`/doctor` returns `"/doctor: not yet connected to runtime."`. The user has
no built-in way to check whether their config is valid, whether the LLM
provider is reachable, or whether required tools (git, gh) are installed.
Diagnosing setup problems today requires reading logs and checking files
manually.

---

## 2. Goals

`/doctor` runs a series of checks and prints a pass/warn/fail table:

```
CHECK                      STATUS   NOTE
───────────────────────────────────────────────────────────
Config file found          ✓ pass
Config valid               ✓ pass
ANTHROPIC_API_KEY set      ✓ pass
Provider reachable         ✓ pass   claude-3-5-sonnet-20241022
git in PATH                ✓ pass   git version 2.43.0
gh in PATH                 ⚠ warn   not found — /pr will not work
SQLite session store       ✓ pass   ~/.local/share/xaft/sessions.db
Working dir writable       ✓ pass   /home/user/project
```

Each check completes independently; failures in one do not stop others.
The last line summarises: `"All checks passed."` or `"N warning(s), M failure(s)."`.

---

## 3. Non-Goals

- Auto-fixing problems.
- Checking MCP server connectivity (that's `/mcp`).
- Network latency benchmarking.

---

## 4. Technical Design

### 4.1 `DiagnosticCheck` type

Defined in `DoctorHandler`'s module:

```rust
enum CheckStatus { Pass, Warn(String), Fail(String) }

struct DiagnosticCheck {
    name: &'static str,
    status: CheckStatus,
    note: Option<String>,
}
```

### 4.2 Check list

| # | Check | How |
|---|-------|-----|
| 1 | Config file found | `XaftConfig::config_path().exists()` |
| 2 | Config valid | Attempt `XaftConfig::load()` and check `validate()` |
| 3 | API key env var set | `std::env::var("ANTHROPIC_API_KEY").is_ok()` (or provider-specific key) |
| 4 | Provider reachable | Send a minimal `/models` list request (no token cost); timeout 5 s |
| 5 | `git` in PATH | `which git` |
| 6 | `gh` in PATH | `which gh`; warn (not fail) if absent |
| 7 | Session store accessible | Open `SessionManager::new()` and ping |
| 8 | Working dir writable | `tempfile::NamedTempFile::new_in(working_dir)` |

### 4.3 Async execution

`DoctorHandler::is_async() → true`. Checks 1–3 and 5–6 run synchronously;
checks 4, 7, 8 are async (tokio). All run in parallel via
`tokio::join!` or `futures::join_all`.

### 4.4 Rendering

`DoctorHandler` builds a `CliTable::new(["CHECK", "STATUS", "NOTE"])`.
Status cells use `StyledCell` with colour:
- `✓ pass` → green
- `⚠ warn` → yellow
- `✗ fail` → red

Summary line appended after the table.

### 4.5 `DoctorHandler` construction

Receives `Arc<XaftConfig>` and `Arc<SessionManager>` via constructor
(injected when the registry is built in `build_default_registry`).

---

## 5. Implementation Steps

1. Define `DiagnosticCheck` and `CheckStatus` in `doctor.rs`.
2. Implement each check as an `async fn`.
3. Implement `DoctorHandler::execute_async` — run checks in parallel,
   collect results, render table + summary.
4. Wire `Arc<XaftConfig>` and `Arc<SessionManager>` into
   `build_default_registry`.
5. Tests: mock each check path (pass / warn / fail); verify table output.

---

## 6. Tests

```
test doctor_all_pass_summary_message
test doctor_missing_api_key_shows_fail
test doctor_gh_not_found_shows_warn_not_fail
test doctor_provider_unreachable_shows_fail
test doctor_checks_run_in_parallel
test doctor_table_adapts_to_terminal_width
```
