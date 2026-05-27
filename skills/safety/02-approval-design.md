# Approval Gate Design

## Purpose

The approval gate is the primary safety mechanism between the autonomous agent and the user's system. Without it, an LLM-driven agent could execute arbitrary shell commands, overwrite critical files, or push unwanted git commits—all without the user's knowledge or consent. The approval gate ensures that every dangerous operation is explicitly authorized by the user before execution. At the same time, the gate must not be so intrusive that it makes the system unusable for trusted workflows. This document specifies the approval gate trait, its implementations, cancellation semantics, and the guardrail configuration that controls which operations require approval.

## Mental Model

Think of the approval gate as a security checkpoint at a building entrance. The `ApprovalGate` trait is the checkpoint interface—it receives a request, waits for a response, and returns it. The `TuiApprovalGate` is the human guard: it displays the request on the TUI, waits for the user to press Y/N, and returns the response via a oneshot channel. The `AutoApproveGate` is the keycard scanner: it always returns `true` (used in headless mode or for trusted operations). The guardrail config is the access control list: it specifies which operations need to go through the checkpoint and which can bypass it. The `cancel_all()` method is the emergency evacuation: when the building is on fire (Ctrl+C), all pending checkpoints are immediately resolved with `false` so nobody is left waiting.

## Extension Patterns

When adding a new operation that needs approval, add it to the guardrail config's `requires_approval` list and update the `ToolRegistry::requires_approval()` method. When adding a new approval gate implementation (e.g., an API-based approval for remote workflows), implement the `ApprovalGate` trait with its `request()` method. When adding a new guardrail category (e.g., "network operations"), add a new boolean field to the `GuardrailConfig` struct and wire it into the approval check. When adding a new TUI prompt style (e.g., a multi-choice approval), add a new variant to the approval request type and handle it in the TUI's approval renderer.

## Common Pitfalls

- **Auto-approving write operations for convenience**: A `write_file` tool that auto-approves will overwrite the user's source code without confirmation. This is acceptable only if the user has explicitly set `auto_approve_writes = true` in their guardrail config. The default must always be to require approval.
- **Not timing out on approval requests**: If the TUI is not responsive (e.g., during a rendering lag), an approval request could hang indefinitely. Always use a timeout (120 seconds default) so the operation fails gracefully if the user doesn't respond.
- **Forgetting to call `cancel_all()` on shutdown**: If the runtime shuts down with pending approval requests, the tasks waiting on those oneshot channels will hang forever. The shutdown handler must call `cancel_all()` to resolve all pending requests with `false`.
- **Using `AutoApproveGate` in interactive mode**: The auto-approve gate is for headless mode and trusted environments. Using it in interactive mode defeats the purpose of the approval system and should only be done after an explicit danger confirmation (typing "yes").
- **Race condition between approval and cancellation**: If cancellation arrives while the TUI is displaying an approval prompt, the prompt must be dismissed immediately and the response must be `false`. The `cancel_all()` method must be called before the TUI shutdown sequence.

## Invariants

1. The `ApprovalGate` trait must have a `request()` method that takes an `ApprovalRequest` and returns a `bool` (true = approved, false = denied).
2. `TuiApprovalGate` must use oneshot channels for approval responses with a 120-second timeout. If the timeout expires, the request is denied.
3. `TuiApprovalGate.cancel_all()` must resolve all pending oneshot channels with `false` to prevent hanging tasks.
4. `AutoApproveGate` must always return `true`. It must never be used in interactive mode without explicit user opt-in.
5. Danger confirmation (typing "yes") is required before the TUI starts when `auto_approve` is enabled for any dangerous operation category. This is a one-time confirmation at startup, not per-operation.
6. The guardrail config must control which operation categories require approval. The default must be: read operations = auto-approve, write/execute operations = require approval.
7. Approval must be requested before the operation begins, not after. There is no retroactive approval.

## Examples

```rust
/// The approval gate trait - the interface between agents and user consent.
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    /// Request approval for an operation. Returns true if approved, false if denied.
    async fn request(&self, req: ApprovalRequest) -> bool;

    /// Cancel all pending approval requests (resolve with false).
    async fn cancel_all(&self);
}

/// An approval request carries the operation details for the user to review.
pub struct ApprovalRequest {
    pub operation: String,
    pub detail: String,
    pub response: oneshot::Sender<bool>,
}

/// TUI-based approval gate - human in the loop.
pub struct TuiApprovalGate {
    pending_approvals: Arc<Mutex<HashMap<Uuid, oneshot::Sender<bool>>>>,
    approval_timeout: Duration,
    event_tx: mpsc::UnboundedSender<TuiEvent>,
}

#[async_trait]
impl ApprovalGate for TuiApprovalGate {
    async fn request(&self, req: ApprovalRequest) -> bool {
        let (tx, rx) = oneshot::channel();
        let id = Uuid::new_v4();

        // Register the pending approval
        {
            let mut pending = self.pending_approvals.lock().await;
            pending.insert(id, tx);
        }

        // Send to TUI for display
        self.event_tx.send(TuiEvent::ApprovalNeeded {
            id,
            operation: req.operation.clone(),
            detail: req.detail.clone(),
        }).ok();

        // Wait with timeout
        match tokio::time::timeout(self.approval_timeout, rx).await {
            Ok(Ok(approved)) => approved,
            _ => {
                // Timeout or channel error → deny
                let mut pending = self.pending_approvals.lock().await;
                pending.remove(&id);
                false
            }
        }
    }

    async fn cancel_all(&self) {
        let mut pending = self.pending_approvals.lock().await;
        for (_, response_tx) in pending.drain() {
            // Resolve with false so waiting tasks don't hang
            let _ = response_tx.send(false);
        }
    }
}

/// Auto-approve gate - for headless mode and trusted environments.
pub struct AutoApproveGate;

#[async_trait]
impl ApprovalGate for AutoApproveGate {
    async fn request(&self, _req: ApprovalRequest) -> bool {
        true
    }

    async fn cancel_all(&self) {
        // Nothing to cancel - no pending approvals
    }
}

/// Guardrail config controls which operations need approval.
#[derive(Debug, Deserialize)]
pub struct GuardrailConfig {
    pub auto_approve_reads: bool,      // default: true
    pub auto_approve_writes: bool,     // default: false
    pub auto_approve_bash: bool,       // default: false
    pub cost_limit_config: Option<CostLimitConfig>,
}

impl Default for GuardrailConfig {
    fn default() -> Self {
        Self {
            auto_approve_reads: true,
            auto_approve_writes: false,
            auto_approve_bash: false,
            cost_limit_config: None,
        }
    }
}

/// Danger confirmation at startup - user must type "yes" to enable auto-approve.
pub async fn confirm_dangerous_auto_approve(config: &GuardrailConfig) -> Result<(), RuntimeError> {
    if config.auto_approve_writes || config.auto_approve_bash {
        println!("⚠️  WARNING: Auto-approve is enabled for dangerous operations.");
        println!("    Write operations: {}", if config.auto_approve_writes { "AUTO-APPROVED" } else { "require approval" });
        println!("    Bash execution: {}", if config.auto_approve_bash { "AUTO-APPROVED" } else { "require approval" });
        println!("    Type 'yes' to continue: ");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if input.trim() != "yes" {
            return Err(RuntimeError::ConfigError(
                "Dangerous auto-approve requires explicit 'yes' confirmation".into(),
            ));
        }
    }
    Ok(())
}
```
