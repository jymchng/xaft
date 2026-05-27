# Debugging with Tracing

## Purpose

Debugging a concurrent, TUI-based runtime is fundamentally different from debugging a CLI tool. Standard print-based debugging doesn't work because the TUI captures stdout, and the runtime has dozens of concurrent tasks whose interleaved output would be unreadable anyway. The `tracing` crate provides structured, hierarchical logging that survives these constraints. Spans create a call-chain hierarchy, fields provide searchable identifiers, and log files persist output even when the TUI is rendering. This document explains how to set up tracing for debugging, how to read the output, and how to diagnose common scenarios using the trace data.

## Mental Model

Think of tracing as a flight data recorder (black box) for the runtime. While the TUI displays the "cockpit view" (what the user sees), the trace log records everything that happens internally: every tool call, every LLM request, every approval gate interaction, every cancellation signal. The recorder is always on (when the TUI is active, logs go to `~/.xaft/debug-<pid>.log`; when the TUI is off, logs go to stderr). The recorder is hierarchical: spans nest inside each other, so you can see that `tool{tool=write_file}` happened inside `agent{agent_name=editor}` happened inside `workflow{workflow=plan-and-edit}`. The recorder is filterable: you can set `RUST_LOG` to see only errors, or only a specific crate, or only a specific span level.

## Extension Patterns

When debugging a specific issue, set `RUST_LOG` to target the relevant crate and level. For provider issues, use `RUST_LOG=xaft_providers=debug`. For agent loop issues, use `RUST_LOG=agtrs_runtime=debug,xaft_agents=debug`. For full detail, use `RUST_LOG=xaft=debug,agtrs_runtime=debug`. When adding new diagnostic information, add it as a span field rather than a log message—fields are structured and searchable, while messages are free text. When a bug is intermittent, increase the log level to `trace` temporarily to capture every channel send, lock acquisition, and state transition. When a bug is in the TUI rendering, check the debug log file rather than trying to read the terminal—the TUI's alternate screen makes terminal output invisible.

## Common Pitfalls

- **Setting `RUST_LOG=debug` without crate qualifiers**: This enables debug logging for every crate in the dependency tree, including `hyper`, `tokio`, and `reqwest`. The output is overwhelming and mostly irrelevant. Always qualify: `RUST_LOG=xaft=debug,agtrs_runtime=debug`.
- **Looking at stderr when the TUI is active**: The TUI renders on the alternate screen, so stderr output is invisible. Always check `~/.xaft/debug-<pid>.log` when the TUI is running.
- **Missing the span hierarchy**: If you look at a log line in isolation, you lose the context of which agent and workflow it belongs to. Always look at the span nesting to understand the call chain.
- **Not checking the PID in the log filename**: If you run xaft multiple times, each run creates a new `debug-<pid>.log`. Make sure you're reading the file for the correct PID (check `ps aux | grep xaft` or the startup log line that announces the PID).
- **Forgetting to enable the file logger**: The file logger is only enabled when the TUI is active. If you're running in headless mode, logs go to stderr, not to a file. Use `2>debug.log` to capture them.

## Invariants

1. When the TUI is active, all trace output must go to `~/.xaft/debug-<pid>.log`, never to stdout or stderr.
2. When the TUI is not active, trace output goes to stderr, respecting `RUST_LOG` filtering.
3. The standard debugging configuration is `RUST_LOG=xaft=debug,agtrs_runtime=debug`.
4. Span hierarchy must follow the call chain: `workflow → agent → tool → provider`.
5. Every span must include identifying fields (tool name, agent name, session ID, path) for searchability.
6. The debug log file must be created at startup and must announce the PID in the first line.

## Examples

```bash
# Enable detailed tracing for provider and runtime
RUST_LOG=xaft=debug,agtrs_runtime=debug xaft session --name my-session

# Trace everything (use sparingly - very verbose)
RUST_LOG=xaft=trace,agtrs_runtime=trace xaft session --name my-session

# Only provider-level tracing
RUST_LOG=xaft_providers=debug xaft session --name my-session

# Headless mode - redirect to file
RUST_LOG=xaft=debug xaft session --name my-session --no-tui 2>debug.log

# Find the debug log file for a running session
ls -la ~/.xaft/debug-*.log
# Output: /home/user/.xaft/debug-12345.log

# Filter for specific agent activity
rg "agent_name=editor" ~/.xaft/debug-12345.log

# Filter for tool calls
rg "tool=" ~/.xaft/debug-12345.log
```

```rust
// Debug log file setup (in runtime initialization)
fn init_tracing(tui_active: bool) {
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_span_events(FmtSpan::ENTER | FmtSpan::EXIT);

    if tui_active {
        let pid = std::process::id();
        let log_path = dirs::home_dir()
            .unwrap()
            .join(".xaft")
            .join(format!("debug-{pid}.log"));
        let file = std::fs::File::create(&log_path).expect("failed to create debug log");
        subscriber.json().with_writer(file).init();
        tracing::info!(pid, log_path = %log_path.display(), "debug logging initialized");
    } else {
        subscriber.with_writer(std::io::stderr).init();
    }
}

// Common debugging scenarios

// 1. Provider errors → check API key resolution
// Look for: resolve_api_key, ApiKeyNotFound, authentication error
// Fix: set correct env var or add api_key to config

// 2. Git conflicts → check worktree status
// Look for: WorktreeFailed, worktree path, branch name
// Fix: ensure git is initialized, no uncommitted changes in worktree

// 3. Session issues → check SQLite files
// Look for: SessionStore error, session_id, data_dir path
// Fix: check data_dir exists and is writable, session ID matches

// 4. Tool failures → check cancellation tokens
// Look for: Cancelled, is_cancelled, CancellationToken
// Fix: ensure CancellationToken is propagated to all spawned tasks
```
