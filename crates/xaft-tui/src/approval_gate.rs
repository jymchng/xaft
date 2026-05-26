//! `TuiApprovalGate` — bridges agtrs `ApprovalGate` to the TUI event loop.
//!
//! When a tool requires confirmation, `request()` sends a `ToolPendingApproval`
//! event to the TUI via the signal bus.  The TUI shows the approval dialog;
//! when the user presses Y or N, the app calls `TuiApprovalGate::respond()`.
//!
//! Responses are matched by `tool_use_id`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Mutex, oneshot};

use agtrs_runtime::approval::ApprovalGate;
use agtrs_runtime::signals::SignalBus;

const APPROVAL_TIMEOUT_SECS: u64 = 120;

/// Approval gate that routes through the TUI dialog.
pub struct TuiApprovalGate {
    /// Pending approvals awaiting a response, keyed by `tool_use_id`.
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    signals: Arc<SignalBus>,
}

impl TuiApprovalGate {
    /// Create a new gate backed by `signals`.
    pub fn new(signals: Arc<SignalBus>) -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            signals,
        }
    }

    /// Respond to a pending approval request.
    ///
    /// Returns `true` if a pending entry was found and responded to.
    pub async fn respond(&self, tool_use_id: &str, approved: bool) -> bool {
        let mut pending = self.pending.lock().await;
        if let Some(tx) = pending.remove(tool_use_id) {
            let _ = tx.send(approved);
            return true;
        }
        false
    }

    /// Cancel all pending approvals (e.g., on TUI shutdown).
    pub async fn cancel_all(&self) {
        let mut pending = self.pending.lock().await;
        for (_, tx) in pending.drain() {
            let _ = tx.send(false);
        }
    }

    /// True if there are pending approvals.
    pub async fn has_pending(&self) -> bool {
        !self.pending.lock().await.is_empty()
    }
}

#[async_trait]
impl ApprovalGate for TuiApprovalGate {
    async fn request(&self, tool_name: &str, tool_use_id: &str, input: &serde_json::Value) -> bool {
        let (tx, rx) = oneshot::channel();

        {
            let mut pending = self.pending.lock().await;
            pending.insert(tool_use_id.to_string(), tx);
        }

        // NOTE: do NOT emit ToolPendingApproval here.
        // The agtrs executor already emits it before calling gate.request().
        // A second emit would create a duplicate approval dialog entry.

        // Wait for user response with a timeout
        match tokio::time::timeout(Duration::from_secs(APPROVAL_TIMEOUT_SECS), rx).await {
            Ok(Ok(approved)) => approved,
            Ok(Err(_)) => false, // sender dropped — reject
            Err(_) => {
                // Timeout — auto-reject and clean up
                tracing::warn!(
                    tool = tool_name,
                    tool_use_id,
                    "TuiApprovalGate: approval timed out, rejecting"
                );
                let mut pending = self.pending.lock().await;
                pending.remove(tool_use_id);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_runtime::signals::SignalBus;

    async fn make_gate() -> TuiApprovalGate {
        let signals = Arc::new(SignalBus::new());
        TuiApprovalGate::new(signals)
    }

    #[tokio::test]
    async fn respond_approve() {
        let gate = make_gate().await;
        let gate_clone = TuiApprovalGate {
            pending: Arc::clone(&gate.pending),
            signals: Arc::clone(&gate.signals),
        };

        let input = serde_json::json!({"command": "ls"});

        // Spawn the response before request so it's ready
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            gate_clone.respond("tu-1", true).await;
        });

        let result = gate.request("bash_exec", "tu-1", &input).await;
        assert!(result);
    }

    #[tokio::test]
    async fn respond_reject() {
        let gate = make_gate().await;
        let gate_clone = TuiApprovalGate {
            pending: Arc::clone(&gate.pending),
            signals: Arc::clone(&gate.signals),
        };

        let input = serde_json::json!({"command": "rm -rf /"});

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            gate_clone.respond("tu-2", false).await;
        });

        let result = gate.request("bash_exec", "tu-2", &input).await;
        assert!(!result);
    }

    #[tokio::test]
    async fn cancel_all_rejects_pending() {
        let gate = make_gate().await;
        let pending = Arc::clone(&gate.pending);
        let signals = Arc::clone(&gate.signals);

        // Insert a fake pending entry
        let (tx, rx) = oneshot::channel::<bool>();
        pending.lock().await.insert("tu-3".into(), tx);

        gate.cancel_all().await;

        // Receiver should get false
        let received = rx.await.unwrap_or(true);
        assert!(!received);
    }

    #[tokio::test]
    async fn respond_returns_false_for_unknown_id() {
        let gate = make_gate().await;
        let found = gate.respond("nonexistent", true).await;
        assert!(!found);
    }

    #[tokio::test]
    async fn has_pending_initially_false() {
        let gate = make_gate().await;
        assert!(!gate.has_pending().await);
    }
}
