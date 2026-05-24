# Security Model

## Threat Surface

| Threat | Mitigation |
|---|---|
| Malicious tool call (rm -rf) | ShellPolicy allowlist + approval gate |
| Prompt injection via file content | PromptInjectionGuardrail |
| Exfiltration via curl/wget | Blocked by default in ShellPolicy |
| API key in output | OutputContentFilter strips key patterns |
| Path traversal in write_file | WorkspaceEditor::validate_path() |
| LLM output executing arbitrary code | No eval; all tool calls go through registry |
| Budget abuse | Hard cost caps enforced by CostBudgetGuardrail |
| Session hijacking | Session IDs are UUIDs; no network exposure by default |
| Worktree pollution | Each task gets isolated branch; merges are explicit |

## API Key Management

```toml
# Recommended: use keychain, not plaintext config
# macOS: security add-generic-password -s xaft -a anthropic -w sk-ant-...
# Linux: pass insert xaft/anthropic-key

# xaft reads from (in order):
# 1. Environment variable: ANTHROPIC_API_KEY
# 2. System keychain (macOS: security, Linux: pass/secret-service)
# 3. ~/.config/xaft/secrets.toml (warn: readable by user)
```

## Audit Log Integrity

The audit log is append-only by design. Each line is a JSON object with a timestamp, event type, and relevant data. Future: HMAC signing of log entries for tamper detection.

```json
{"ts":"2026-01-15T10:23:45.123Z","ev":"session_start","sid":"ses-abc","pid":12345,"xaft_version":"0.1.0","project":"/home/user/myproject"}
{"ts":"2026-01-15T10:23:46.234Z","ev":"tool_call","tool":"write_file","input":{"path":"src/auth.rs"},"risk":"medium","approved":true}
{"ts":"2026-01-15T10:23:46.456Z","ev":"tool_result","tool":"write_file","success":true,"bytes":4231}
```

## References

- agtrs: `agtrs-shell/src/policy.rs`
- agtrs: `agtrs-runtime/src/guardrail.rs`
