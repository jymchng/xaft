cat > ./01_approval_safety.md << 'EOF'
# Approval & Safety Systems

## Guardrail Architecture

`xaft` uses `agtrs-runtime` guardrails at three interception points:

```
User Input → [Input Guardrails] → Agent → LLM → [Output Guardrails] → Tool Call → [Tool Hooks (before)] → Tool → [Tool Hooks (after)]
```

### Built-in Guardrails

```rust
// 1. Input sanitization
ctx.add_guardrail(Arc::new(InputSanitizerGuardrail));

// 2. Prompt injection detection
ctx.add_guardrail(Arc::new(PromptInjectionGuardrail::new(threshold: 0.8)));

// 3. Output content filter
ctx.add_guardrail(Arc::new(OutputContentFilter));

// 4. Cost guardrail (from agtrs examples)
ctx.add_guardrail(Arc::new(SessionBudgetGuardrail::new(
    Arc::clone(&cost_tracker),
    config.session_budget,
)));
```

### PromptInjectionGuardrail

```rust
#[guardrail]
pub struct PromptInjectionGuardrail {
    threshold: f64,
}

#[async_trait]
impl Guardrail for PromptInjectionGuardrail {
    async fn check_input(&self, message: &Message, _ctx: &GuardrailContext)
        -> Result<GuardrailDecision, AgtrsError>
    {
        let text = message.text();
        // Check for known injection patterns
        let suspicious_patterns = [
            "ignore previous instructions",
            "disregard all constraints",
            "you are now",
            "system prompt:",
            "```\nsystem:",
        ];

        if suspicious_patterns.iter().any(|p| text.to_lowercase().contains(p)) {
            return Ok(GuardrailDecision::Block(
                "Input contains potential prompt injection pattern".into()
            ));
        }

        Ok(GuardrailDecision::Pass)
    }
}
```

## Risk Classification System

```rust
pub struct RiskClassifier {
    rules: Vec<RiskRule>,
}

pub struct RiskRule {
    pub tool_pattern: Regex,       // matches tool name
    pub input_pattern: Option<Regex>,  // optional input pattern match
    pub risk_level: RiskLevel,
}

impl RiskClassifier {
    pub fn default_rules() -> Vec<RiskRule> {
        vec![
            // Always high risk
            RiskRule { tool_pattern: Regex::new("delete|remove|rm").unwrap(), risk_level: RiskLevel::High },
            RiskRule { tool_pattern: Regex::new("run_command").unwrap(), risk_level: RiskLevel::High },
            RiskRule { tool_pattern: Regex::new("git_push").unwrap(), risk_level: RiskLevel::High },

            // Medium risk
            RiskRule { tool_pattern: Regex::new("write_file").unwrap(), risk_level: RiskLevel::Medium },
            RiskRule { tool_pattern: Regex::new("apply_patch").unwrap(), risk_level: RiskLevel::Medium },
            RiskRule { tool_pattern: Regex::new("git_commit").unwrap(), risk_level: RiskLevel::Medium },
            RiskRule { tool_pattern: Regex::new("run_cargo").unwrap(), risk_level: RiskLevel::Medium },

            // Low risk (read-only)
            RiskRule { tool_pattern: Regex::new("read|list|search|find|diff|status|log").unwrap(), risk_level: RiskLevel::Low },
        ]
    }
}
```

## Safety Configuration

```toml
# .xaft/config.toml
[safety]
# Global auto-approve level: "none" | "low" | "medium" | "high"
auto_approve = "medium"
# Require confirmation for budget > this amount
confirm_budget_threshold = 0.50
# Deny any command not in the allowlist
strict_shell_mode = true

[safety.approval]
# Timeout for approval dialogs
timeout_secs = 60
# What happens on timeout: "deny" | "approve" | "suspend"
timeout_action = "deny"
```

## References

- agtrs: `agtrs-runtime/src/guardrail.rs`
- agtrs guide: `guides/06-guardrails.md`, `guides/13-approval-gates.md`
EOF

cat > ./02_sandboxing.md << 'EOF'
# Sandboxing

## ShellExecutor Policy Enforcement

The `ShellExecutor` from `agtrs-shell` enforces a policy at the point of execution. No command can bypass the policy — it is evaluated before any subprocess is spawned.

```rust
pub fn build_production_policy(config: &XaftConfig) -> ShellPolicy {
    ShellPolicy::new()
        .working_dir(&config.project_root)
        .max_output_bytes(10_000_000)
        .timeout(Duration::from_secs(300))
        // Allowlist approach
        .allow_commands(&config.safety.allowed_commands)
        // Env var restrictions
        .clear_env()
        .allow_env("PATH")
        .allow_env("HOME")
        .allow_env("CARGO_HOME")
        .allow_env("RUSTUP_HOME")
        .set_env("RUST_BACKTRACE", "1")
}
```

## Platform-Level Sandboxing (Future)

For maximum isolation, `xaft` plans platform-level sandboxing:

| Platform | Mechanism | Status |
|---|---|---|
| Linux | `seccomp` + `namespaces` via `landlock` | Planned v2 |
| macOS | `sandbox-exec` profiles | Planned v2 |
| Windows | Job Objects + AppContainer | Planned v3 |
| Cross-platform | Docker container per task | Available now via plugin |

## Worktree as Filesystem Boundary

The git worktree provides a logical filesystem boundary: agents write only to the worktree path, not the main working tree. The `WorkspaceEditor` enforces this:

```rust
impl WorkspaceEditor {
    fn validate_path(&self, path: &Path) -> Result<PathBuf, WorkspaceError> {
        let canonical = self.root.join(path).canonicalize()?;
        if !canonical.starts_with(&self.root) {
            return Err(WorkspaceError::PathEscape {
                attempted: path.to_owned(),
                root: self.root.clone(),
            });
        }
        Ok(canonical)
    }
}
```

## References

- agtrs: `agtrs-shell/src/policy.rs`, `agtrs-shell/src/sandbox.rs`
EOF

cat > ./03_security.md << 'EOF'
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
EOF

echo "Safety docs done"