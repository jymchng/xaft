# Troubleshooting

Practical fixes for common issues, with the diagnostic commands to pin down
the cause.

## Provider authentication errors

**Symptom:** `401 Unauthorized` or `authentication failed` on first run.

**Fix:**

1. Confirm the key is exported:
   ```bash
   echo "${ANTHROPIC_API_KEY:+set}"    # prints "set" if present
   echo "${OPENAI_API_KEY:+set}"
   ```
2. Check which provider is resolved:
   ```bash
   xaft config show | grep -A 3 provider
   ```
3. If using `api_key_env`, make sure the env var name in
   `[provider.<name>]` matches exactly.

## Model not found / unknown model

**Symptom:** the provider rejects the model id.

**Fix:**

- Override per run: `xaft run "task" --model <valid-model-id>`
- Or fix the default in config: `xaft config show` to inspect, then edit
  `[agent.<name>].model` / `[provider.<name>].models`.
- Some providers map friendly names to API ids via `[provider.<name>].models`
  — verify that map.

## Rate limits (rpm / tpm)

**Symptom:** `429` errors.

**Fix:**

- Set `rpm_limit` / `tpm_limit` in `[provider.<name>]` to match your plan.
- Retry logic is built in (`max_retries`, `timeout_secs`); raise
  `timeout_secs` for slow networks.

## Cost limit hit

**Symptom:** the run stops with a cost-limit message.

**Fix:**

- Check the configured limit: `xaft config show` → `guardrail.cost_limit_config`.
- Raise `max_spend` or set `cost_limit = false` if intentional.
- Use `--dry-run` for planning to avoid spending on exploration.

## Config validation failures

**Symptom:** xaft refuses to start with a config error.

**Fix:**

```bash
xaft config validate                    # validate the resolved config
xaft config validate -c path/to.toml    # validate a specific file
```

The error names the offending key. Common causes: unknown `provider_type`,
invalid `log_level`, or a `[provider]` block missing `api_key_env`/`base_url`.

## Session resume problems

**Symptom:** `--resume <id>` shows nothing or the wrong transcript.

**Fix:**

- List sessions to confirm the id: `xaft session list`.
- Inspect the session: `xaft session show <id>`.
- If the transcript is empty, check `[core].data_dir` — the session store may
  live under a different data dir than the one the run used.
- The newest 20 turns replay by default; set
  `[core].resume_transcript_turns` (or the session equivalent) higher if you
  need more context.

## TUI paste problems

**Symptom:** pasting multi-line text submits prematurely or garbles input.

**Fix:**

- xaft keeps large pastes behind a `[Pasted text #N …]` placeholder — press
  `Ctrl+V` to reveal, `Esc` after `]` to discard.
- If pastes are delivered as key events, ensure the terminal supports
  bracketed paste (most modern terminals do).

## Mode / badge confusion

**Symptom:** `/mode debug` is rejected, or the badge disappears in auto.

**Fix:**

- Only the cycle Safe → Plan → Yolo is selectable via Shift+Tab; `debug` is
  deliberately not in the cycle (agenthicc parity). Use `/mode debug` only if
  a direct selection path exists — the cycle will not include it.
- Auto (Yolo) hides the badge by design to keep the prompt clean.

## Slow or unresponsive TUI

**Fix:**

- Use `--log-level debug` for more diagnostics.
- Move long-running work to the background with `/bg`, then re-attach with
  `/bg <n>`.
- Reduce `conversation_height` in `[tui]` if the render feels heavy.

## Docs / build failures (contributors)

**Symptom:** CI fails on the docs step.

**Fix:**

```bash
node scripts/docs-site.cjs --check     # find broken internal links
```

The checker reports the exact file + link. Fix the markdown, then re-run.

## Generic diagnostics

- `xaft doctor` — run configuration and connectivity diagnostics (in the TUI).
- `xaft version` — confirm the binary matches expectations.
- `xaft --log-level debug run "task"` — verbose run with full tracing.

## Related

- [Configuration →](02-configuration.md)
- [Security →](10-security.md)
- [FAQ →](12-faq.md)
