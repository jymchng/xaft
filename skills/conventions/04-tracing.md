# Tracing Conventions

## Purpose

Observability is the difference between a system you can debug in production and one you have to guess about. The xaft runtime runs inside a TUI that swallows stdout, so traditional `println!` debugging is useless. Instead, xaft uses the `tracing` crate with structured spans and fields. Every tool call, agent step, and runtime method is wrapped in an `#[instrument]` attribute that creates a span with key identifiers. This means that when a user reports "the agent got stuck," you can open the debug log file, find the agent's span by name, and trace every tool call, LLM request, and approval gate interaction that happened within it. Without these conventions, debug logs would be an undifferentiated stream of text with no structure or searchability.

## Mental Model

Think of tracing as a nested scope tree, not a flat log. Each `#[instrument]` call opens a new span that is a child of the current span. When a tool call happens inside an agent step, which happens inside a workflow, the log shows: `workflow{workflow=plan-and-edit} → agent{agent=editor} → tool{tool=write_file}`. This hierarchy lets you filter by any level—show me all tool calls, or show me only the editor agent's activity. Span fields are the index: `path`, `command`, `agent_name`, `session_id` are the keys you search by. Log levels are the severity filter: `error` for unrecoverable failures, `warn` for recoverable issues, `info` for significant lifecycle events (session started, agent switched), `debug` for detailed operational data (tool inputs/outputs), `trace` for verbose internal state (serialization details, channel sends).

## Extension Patterns

When adding a new method to a tool, agent, or runtime struct, add `#[instrument(skip(self, ctx), fields(key = value))]` where `key` is a domain-specific identifier (tool name, agent name, session ID) and `value` is the expression to capture. The `skip(self, ctx)` prevents logging entire struct contents. When adding a new crate, ensure it initializes tracing via `tracing_subscriber` in its test harness. When adding a new span field, choose a name that is consistent with existing fields (e.g., always use `path` for filesystem paths, never `file_path` or `filepath`). When adding a new log line, choose the correct level: if the system cannot continue, use `error!`; if it recovered automatically, use `warn!`; if it's a normal lifecycle event, use `info!`; if it's operational detail, use `debug!`; if it's internal plumbing, use `trace!`.

## Common Pitfalls

- **Forgetting `#[instrument]` on new methods**: A tool call without a span is invisible in the debug log. You'll see the LLM request but not what tool was called or what it returned. Always add `#[instrument]` to tool execution methods, agent step methods, and runtime orchestration methods.
- **Logging sensitive data in span fields**: Never put API keys, file contents, or user prompts in span fields. Use `skip(self)` to avoid logging the entire struct, and only include identifying fields (tool name, path basename, session ID prefix).
- **Using `println!` or `eprintln!` for debugging**: These are invisible when the TUI is active because the alternate screen captures stdout. Always use `tracing::debug!` or `tracing::info!` so the output goes to the debug log file.
- **Overusing `info!` for routine operations**: If every tool call logs at `info!` level, the signal-to-noise ratio drops and important events are buried. Reserve `info!` for lifecycle events (session start/end, agent handoff, workflow completion). Use `debug!` for tool inputs/outputs and LLM request/response metadata.
- **Inconsistent field names**: Using `file` in one span and `path` in another makes the logs unsearchable. Pick one canonical name per concept and use it everywhere.

## Invariants

1. Every public method on `Tool`, `Agent`, and runtime structs must have `#[instrument(skip(self, ctx), fields(...))]`.
2. Span fields must use canonical names: `path` for filesystem paths, `command` for shell commands, `agent_name` for agent identifiers, `session_id` for session identifiers, `tool` for tool names.
3. Log levels must follow the hierarchy: `error` = unrecoverable, `warn` = recoverable, `info` = significant lifecycle event, `debug` = operational detail, `trace` = verbose internal state.
4. When the TUI is active, all log output must go to `~/.xaft/debug-<pid>.log`, not to stdout/stderr.
5. Never log API keys, file contents, or user prompts in span fields or log messages.
6. The `RUST_LOG` environment variable must control verbosity at the crate level: `xaft=debug,agtrs_runtime=debug` is the standard debugging configuration.

## Examples

```rust
use tracing::{info, warn, error, debug, instrument};

// Tool execution with instrument
#[instrument(skip(self, ctx), fields(tool = %self.name(), path = %path.display()))]
async fn execute(&self, ctx: &ToolContext, path: &Path) -> ToolResult {
    debug!(input = %path.display(), "executing file read");
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            warn!(path = %path.display(), "file not found, returning soft error");
            return ToolResult { output: "File not found".into(), is_error: true };
        }
        Err(e) => {
            error!(path = %path.display(), error = %e, "unrecoverable read failure");
            return Err(AgtrsError::ToolFailed { name: self.name().into(), source: e.into() });
        }
    };
    debug!(bytes = content.len(), "file read complete");
    ToolResult { output: content, is_error: false }
}

// Agent step with instrument
#[instrument(skip(self, ctx), fields(agent_name = %self.name()))]
async fn step(&self, ctx: &mut AgentContext) -> Result<AgentAction, AgtrsError> {
    info!("agent starting step");
    // ... step logic ...
    info!(action = %action, "agent step complete");
    Ok(action)
}

// Runtime orchestration with instrument
#[instrument(skip(self), fields(session_id = %session.id()))]
async fn run_session(&self, session: Session) -> Result<(), RuntimeError> {
    info!("session started");
    // ... session loop ...
    info!("session completed");
    Ok(())
}
```
