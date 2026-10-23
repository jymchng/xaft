# Configuration

xaft reads a single TOML config file with environment-variable overrides and
hot reload.

## Config file locations (precedence, highest first)

1. `.xaft/xaft.toml` — project-local config
2. `~/.config/xaft/xaft.toml` — user config
3. Built-in defaults (`xaft-config/src/defaults.rs`)

You can point xaft at an explicit file with `--config <path>`.

## Environment overrides

Every config key can be overridden with an env var. The mapping is
`POKE_VAULT_`-style: `xaft config set execution.provider openai` sets
`execution.provider`; the env var is `XAFT_EXECUTION_PROVIDER` (dotted paths
become `_`-joined upper-case). See `xaft-config/src/merge.rs` for the exact
merge order.

## Hot reload

`xaft-config/src/watcher.rs` watches the active config file and reloads on
change. The TUI picks up reloads and applies them to the next run request.

## Key sections

| Section | Keys | Purpose |
|---|---|---|
| `[execution]` | `provider`, `model`, `temperature`, `max_tokens` | LLM provider + sampling |
| `[security]` | `approval.*`, `sandbox.*`, `permissions.*` | Approval gates and sandboxing |
| `[workspace]` | `root`, `git_worktree`, `auto_commit` | Workspace + git isolation |
| `[session]` | `data_dir`, `resume_transcript_turns` | Session persistence + resume tail length |
| `[tui]` | `theme`, `mouse`, `keyboard_enhanced` | TUI appearance + input |
| `[memory]` | `recall_limit`, `auto_summarize` | Memory tool limits |
| `[tools]` | `browser_backend` | Which browser tool family a session gets (`cloakbrowser`, `playwright`, `none`) |
| `[tools.cloakbrowser]` / `[tools.playwright]` | `enabled`, `transport`, `allowed_domains`, `max_pages`, ... | Browser automation — see [Browser automation](browser.md#configuration-reference) |
| `[loops]` | `enabled`, `default_interval_s`, `min_interval_s`, `max_interval_s`, `max_age_s`, `max_prompt_bytes`, `max_consecutive_failures`, `busy_poll_s`, `persist`, `allow_slash_commands` | The TUI's `/loop` scheduler |

## `[loops]` — the `/loop` scheduler

agenthicc's `LoopSettings`, key for key and default for default. Limits are
deliberately finite: a loop is explicit user automation, not a background
daemon.

```toml
[loops]
enabled = true                 # false blocks creating/resuming loops; status/pause/stop still work
default_interval_s = 600       # interval when `/loop` is given none
min_interval_s = 60            # smallest interval `/loop` accepts
max_interval_s = 86400         # largest interval `/loop` accepts
max_age_s = 259200             # a loop expires 72 hours after it was created
max_prompt_bytes = 16384       # largest prompt or command payload (UTF-8 bytes)
max_consecutive_failures = 3   # failed iterations in a row before the loop stops
busy_poll_s = 1.0              # how often a due iteration re-checks for an idle session
persist = true                 # save with the session so `--resume` rehydrates it
allow_slash_commands = true    # accept a registered `/command` as the payload
```

The bounds are validated when the config loads (e.g. `default_interval_s` must
lie within `min_interval_s..=max_interval_s`), so a nonsensical table fails
startup rather than the first `/loop`. See [Commands](commands.md#recurring-prompts-loop).

See [Reference: repository state](../reference/repository-state.md) for the full
set of defaults and validation rules.

## Layering, inheritance and one-off overrides

Files are merged **key by key**: a layer only overrides the keys it actually
writes, so a project `.xaft.toml` never resets a value you set in
`~/.config/xaft/xaft.toml` just by existing.

* **Explicit file** — `xaft run -c path/to/xaft.toml`, or the `XAFT_CONFIG`
  environment variable, replaces project-config discovery (agenthicc's
  `--config` / `AGENTHICC_CONFIG`).
* **Inheritance** — any config file may start with
  `extends = "../team.toml"` (or a list, merged left to right); paths are
  relative to the file, parents are merged first and the file's own keys win.
  A cycle is reported with the full chain.
* **One-off overrides** — `xaft run --set section.key=value` (repeatable;
  values parse like TOML, so `7`, `true` and `["a", "b"]` keep their types)
  and `--set-secret section.key=ENV_VAR` (reads the value from the
  environment, keeping it out of shell history). Both apply after every file
  and environment variable; an unknown key is an error.

```bash
xaft run --set workflow.default_workflow=code_plan --set core.log_level=debug "add a flag"
XAFT_CONFIG=ci/xaft.ci.toml xaft run --headless "run the release checklist"
```

