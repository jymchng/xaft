//! Shared implementation for the `batch_move` and `batch_copy` parity tools.
//!
//! The two are the same operation with a different verb, and agenthicc itself
//! writes them out twice with identical logic. Keeping one implementation here
//! means the two tools cannot drift apart — the failure mode of duplicated
//! copy-pasted batch code.

use std::path::Path;

use serde_json::{Value, json};

/// Apply `op` to every `entry` pair, collecting per-entry results.
///
/// Entries are reported in the caller's spelling (not the resolved absolute
/// path), so the result reads back as what the caller asked for.
pub(crate) fn run_transfers(
    root: &Path,
    entries: &[(String, String)],
    op: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> (Vec<Value>, usize) {
    let mut results = Vec::with_capacity(entries.len());
    let mut succeeded = 0usize;

    for (source, destination) in entries {
        match op(&root.join(source), &root.join(destination)) {
            Ok(()) => {
                succeeded += 1;
                results.push(json!({
                    "source": source,
                    "destination": destination,
                    "ok": true,
                }));
            }
            Err(e) => results.push(json!({
                "source": source,
                "destination": destination,
                "ok": false,
                "error": e.to_string(),
            })),
        }
    }

    (results, succeeded)
}

/// Reject a batch whose entries are not `{ source, destination }` string pairs.
///
/// Returns the message to fail the whole call with, if any.
pub(crate) fn validate_pairs(
    tool: &str,
    field: &str,
    raw: &[Value],
) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::with_capacity(raw.len());
    for (i, item) in raw.iter().enumerate() {
        let source = item.get("source").and_then(|v| v.as_str());
        let destination = item.get("destination").and_then(|v| v.as_str());
        match (source, destination) {
            (Some(s), Some(d)) => out.push((s.to_string(), d.to_string())),
            _ => {
                return Err(format!(
                    "invalid input for tool '{tool}': entry {i} of '{field}' must have \
                     \"source\" and \"destination\" string keys"
                ));
            }
        }
    }
    Ok(out)
}

/// Assemble the result envelope agenthicc returns for move/copy batches:
/// `{ ok, results, succeeded, failed }` — deliberately *without* a `total`
/// key, matching `batch_move`/`batch_copy` in `agenthicc/tools/fs/agent_tools.py`.
pub(crate) fn envelope(results: Vec<Value>, succeeded: usize, total: usize) -> Value {
    let failed = total - succeeded;
    json!({
        "ok": failed == 0,
        "results": results,
        "succeeded": succeeded,
        "failed": failed,
    })
}

/// agenthicc's refusal shape for a batch that never ran: `results` is empty
/// and the full count is reported as failed.
pub(crate) fn refusal(message: &str, total: usize) -> String {
    json!({
        "ok": false,
        "error": message,
        "results": [],
        "succeeded": 0,
        "failed": total,
    })
    .to_string()
}

/// Create `destination`'s parent directory if it is missing.
///
/// Synchronous on purpose: `run_transfers` hands the same closure to both
/// tools, and one of them copies while the other renames within an already
/// prepared tree.
pub(crate) fn ensure_parent(destination: &Path) -> std::io::Result<()> {
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_omits_total_like_agenthicc() {
        let v = envelope(vec![json!({"ok": true})], 1, 1);
        assert!(
            v.get("total").is_none(),
            "agenthicc's move/copy envelope has no total key"
        );
        assert_eq!(v["succeeded"], 1);
        assert_eq!(v["failed"], 0);
    }

    #[test]
    fn validate_pairs_names_the_offending_entry() {
        let raw = vec![
            json!({"source": "a", "destination": "b"}),
            json!({"source": "c"}),
        ];
        let err = validate_pairs("batch_move", "moves", &raw).unwrap_err();
        assert!(err.contains("entry 1"), "got: {err}");
    }
}
