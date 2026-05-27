# Quick Start

This page walks you through running your first coding task with xaft in under five minutes. By the end, you will have a working xaft installation, a valid configuration, and a completed task with a session you can replay. We assume you have already installed the binary—see [Installation](01-installation.md) if not.

## Step 1: Set Your API Key

xaft needs an LLM provider to generate code. The fastest path is to set your Anthropic API key as an environment variable:

```bash
export ANTHROPIC_API_KEY="sk-ant-api03-..."
```

Add this line to your shell profile (`~/.bashrc`, `~/.zshrc`) so it persists across sessions. xaft reads the key at startup and passes it to the `CostedProvider`→`AnthropicProvider` chain. If the key is missing or invalid, xaft will exit with a clear error message before making any API calls—no silent failures.

If you prefer OpenAI:

```bash
export OPENAI_API_KEY="sk-..."
```

Both keys can coexist. When both are present, xaft uses whichever provider your configuration specifies, defaulting to Anthropic. The `FallbackProvider` wrapper automatically retries with the secondary provider if the primary returns a transient error (rate limit, 5xx).

## Step 2: Initialize a Project

Navigate to the Git repository where you want xaft to work. xaft requires a Git repository because it creates a worktree for every task—this provides full isolation and rollback capability:

```bash
cd ~/projects/my-app
git status  # verify this is a Git repo
```

If your project is not yet a Git repository, initialize one:

```bash
mkdir ~/projects/xaft-demo && cd ~/projects/xaft-demo
git init
echo "# xaft demo" > README.md
git add . && git commit -m "initial commit"
```

xaft will not operate on a directory without at least one commit because the worktree manager needs a commit SHA to check out.

## Step 3: Run Your First Task

The `xaft run` command is the primary entry point for executing coding tasks. It accepts a natural-language prompt describing what you want done:

```bash
xaft run "Add a CLI argument parser to main.rs that accepts --name and --count flags"
```

When you press Enter, the following sequence unfolds automatically:

1. **Bootstrap.** `XaftRuntime::bootstrap()` creates the `SignalBus`, opens the `FsSessionStore`, and attaches signal listeners for logging, metrics, and TUI updates.

2. **Config resolution.** The `ConfigLoader` merges all six configuration layers (defaults → global → project → session → env → CLI) into a single resolved `XaftConfig`.

3. **Agent selection.** The `run_task()` function resolves the agent preset. By default, this is `coder`, which uses the `HandoffOrchestrator` pipeline: Planner → Coder → QA → Fixer.

4. **Provider chain construction.** xaft builds the provider chain: `CostedProvider` wraps `FallbackProvider`, which wraps the concrete `AnthropicProvider` or `OpenAIProvider`. This chain handles cost tracking, failover, and retry logic transparently.

5. **Workspace setup.** A Git worktree is created at `.xaft/worktrees/<session-id>/`, and a `WorkspaceStore` is opened to track file edits transactionally.

6. **Orchestration.** The `HandoffOrchestrator` begins the first agent turn. Each turn involves an LLM API call, tool execution (subject to approval gates), and a `Handoff` decision.

7. **Approval gates.** When an agent wants to modify a file or run a shell command, the `TuiApprovalGate` pauses execution and presents the action for your review. You can approve, deny, or let the 120-second timeout auto-deny.

8. **Completion.** When the orchestrator produces a `Handoff::Terminate`, or when the maximum of 14 handoffs is reached, the task completes. xaft prints a summary of files changed, tools invoked, and token usage.

### Interactive Mode

By default, `xaft run` starts the Ratatui TUI dashboard. This dashboard shows real-time streaming of LLM responses, a tool-call log, token usage counters, and the approval gate interface. You can navigate between panels using keyboard shortcuts:

| Key | Action |
|-----|--------|
| `Tab` | Cycle focus between panels |
| `Enter` | Approve the pending tool call |
| `Esc` | Deny the pending tool call |
| `q` | Quit xaft (sends cancellation signal) |
| `↑`/`↓` | Scroll the active panel |

The TUI is powered by three concurrent tokio tasks: a runtime loop that processes agent turns, a terminal reader that captures key events, and a tick spawner that refreshes the UI at 60fps. The `EventBridge` subscribes to every `SignalBus` event and converts them into `TuiEvent` messages that the dashboard renders.

### Headless Mode

For CI pipelines or scripting, use `--no-tui` to disable the interactive dashboard and run xaft in headless mode. In this mode, the `AutoApproveGate` is used by default—all tool calls are approved automatically. This is dangerous for untrusted prompts but appropriate when the task and environment are controlled:

```bash
xaft run --no-tui "Fix all clippy warnings in the src/ directory"
```

You can also force the `TuiApprovalGate` in headless mode with `--require-approval`, which blocks on stdin for y/n input:

```bash
xaft run --no-tui --require-approval "Refactor the database layer"
```

## Step 4: Review the Session

Every task creates a session that is persisted to SQLite (WAL mode). You can list all sessions:

```bash
xaft sessions list
```

Replay a session to see the full conversation history, tool calls, and LLM responses:

```bash
xaft sessions show <session-id>
```

Resume a session to continue where it left off. This is useful when a task was interrupted or when you want to add a follow-up instruction:

```bash
xaft sessions resume <session-id>
```

Sessions are stored in `.xaft/sessions.db` within your project root. The WAL mode ensures that even if xaft crashes mid-write, the database will recover to a consistent state on the next launch. No corrupted partial sessions.

## Step 5: Configure Defaults (Optional)

xaft's six-layer configuration system means you can set defaults at the global level and override them per-project or per-session. The most common configuration changes are the default model and the approval policy.

Create a global config file:

```bash
mkdir -p ~/.config/xaft
cat > ~/.config/xaft/config.toml << 'EOF'
[provider]
default = "anthropic"
model = "claude-sonnet-4-20250514"

[approval]
mode = "interactive"   # "interactive" | "auto" | "require-approval"
timeout_secs = 120

[agent]
max_handoffs = 14
EOF
```

Create a project-level override in your repository:

```bash
cat > .xaft/config.toml << 'EOF'
[provider]
model = "claude-opus-4-20250514"   # use the heavier model for this project

[agent]
preset = "plan-mode"               # use PlanModeAgent for complex tasks
EOF
```

Run `xaft config show` to see the fully resolved configuration after all six layers are merged. This is invaluable for debugging why a particular setting is or is not taking effect.

## What's Next?

You now have a working xaft setup and have completed at least one task. The next page, [First Task Walkthrough](03-first-task.md), dissects a real task in detail—tracing every signal, tool call, and handoff decision so you understand exactly what happened under the hood.
