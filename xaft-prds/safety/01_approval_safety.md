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
