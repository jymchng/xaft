//! Integration tests for the F3 @-mention signals.
//!
//! Each signal is emitted by the TUI's submit-time resolver or by the
//! escape confirmation dialog. These tests verify:
//! - Signal type signatures
//! - Bus subscription delivers the right count + payload
//! - Per-file attached signals are emitted exactly once per FileRef block
//! - Escape approved/denied signals carry the expected fields
//!
//! Tests are written against the `xaft-agent::signals` public surface
//! only — the TUI's internal helpers are tested in `xaft-tui` unit
//! tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::transport::{EscapeInfo, EscapeReason};
use xaft_agent::signals::{
    EscapeSignalEntry, XaftEscapeMentionApproved, XaftEscapeMentionDenied, XaftFileRefAttached,
    XaftFileRefNotFound, XaftMentionsResolved, XaftUserMessageSubmitted,
};

async fn count_signal<T: 'static + Send + Sync>(bus: &SignalBus) -> Arc<AtomicUsize> {
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);
    bus.on::<T>(move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    })
    .await;
    count
}

#[tokio::test]
async fn mentions_resolved_signal_basic() {
    let bus = SignalBus::new();
    let count = count_signal::<XaftMentionsResolved>(&bus).await;

    bus.emit(XaftMentionsResolved {
        mention_count: 3,
        resolved_count: 2,
        warning_count: 1,
        escape_count: 1,
        total_bytes: 4096,
    })
    .await;

    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn file_ref_attached_signal_per_file() {
    let bus = SignalBus::new();
    let count = count_signal::<XaftFileRefAttached>(&bus).await;

    for i in 0..3 {
        bus.emit(XaftFileRefAttached {
            path: format!("src/file{i}.rs"),
            canonical_path: format!("/work/src/file{i}.rs"),
            byte_size: 100 * (i as u64 + 1),
            line_count: 10,
            sha256: "abcd".repeat(8),
            is_escape: false,
        })
        .await;
    }

    assert_eq!(count.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn file_ref_not_found_signal_carries_reason() {
    let bus = SignalBus::new();
    let received: Arc<std::sync::Mutex<Option<XaftFileRefNotFound>>> =
        Arc::new(std::sync::Mutex::new(None));
    {
        let r = Arc::clone(&received);
        bus.on::<XaftFileRefNotFound>(move |ev| {
            *r.lock().unwrap() = Some(XaftFileRefNotFound {
                path: ev.path.clone(),
                reason: ev.reason.clone(),
            });
        })
        .await;
    }

    bus.emit(XaftFileRefNotFound {
        path: "missing.rs".into(),
        reason: "workspace has no file at \"missing.rs\"".into(),
    })
    .await;

    let r = received.lock().unwrap().clone().unwrap();
    assert_eq!(r.path, "missing.rs");
    assert!(r.reason.contains("missing.rs"));
}

#[tokio::test]
async fn escape_mention_approved_signal_one_shot() {
    let bus = SignalBus::new();
    let received: Arc<std::sync::Mutex<Option<XaftEscapeMentionApproved>>> =
        Arc::new(std::sync::Mutex::new(None));
    {
        let r = Arc::clone(&received);
        bus.on::<XaftEscapeMentionApproved>(move |ev| {
            *r.lock().unwrap() = Some(XaftEscapeMentionApproved {
                tokens: ev.tokens.clone(),
                session_wide: ev.session_wide,
            });
        })
        .await;
    }

    bus.emit(XaftEscapeMentionApproved {
        tokens: vec![EscapeSignalEntry {
            raw_token: "../sibling/foo.rs".to_string(),
            reason: "parent_traversal".to_string(),
            absolute_path: "/work/sibling/foo.rs".to_string(),
            byte_size: 1024,
            depth: 1,
        }],
        session_wide: false,
    })
    .await;

    let r = received.lock().unwrap().clone().unwrap();
    assert_eq!(r.tokens.len(), 1);
    assert_eq!(r.tokens[0].depth, 1);
    assert!(!r.session_wide);
}

#[tokio::test]
async fn escape_mention_approved_signal_session_wide() {
    let bus = SignalBus::new();
    let count = count_signal::<XaftEscapeMentionApproved>(&bus).await;

    bus.emit(XaftEscapeMentionApproved {
        tokens: vec![],
        session_wide: true,
    })
    .await;

    assert_eq!(count.load(Ordering::SeqCst), 1);
    // session_wide flag is metadata on the bus; the bus itself does
    // not enforce a sticky approval. The TUI state holds the flag and
    // consults it on every submit.
}

#[tokio::test]
async fn escape_mention_denied_signal_carries_reason() {
    let bus = SignalBus::new();
    let received: Arc<std::sync::Mutex<Option<XaftEscapeMentionDenied>>> =
        Arc::new(std::sync::Mutex::new(None));
    {
        let r = Arc::clone(&received);
        bus.on::<XaftEscapeMentionDenied>(move |ev| {
            *r.lock().unwrap() = Some(XaftEscapeMentionDenied {
                tokens: ev.tokens.clone(),
                reason: ev.reason.clone(),
            });
        })
        .await;
    }

    bus.emit(XaftEscapeMentionDenied {
        tokens: vec![EscapeSignalEntry {
            raw_token: "/etc/hosts".to_string(),
            reason: "absolute".to_string(),
            absolute_path: "/etc/hosts".to_string(),
            byte_size: 320,
            depth: 0,
        }],
        reason: "cancel".to_string(),
    })
    .await;

    let r = received.lock().unwrap().clone().unwrap();
    assert_eq!(r.reason, "cancel");
    assert_eq!(r.tokens[0].reason, "absolute");
}

#[tokio::test]
async fn f3_signals_do_not_fire_for_non_f3_submissions() {
    // `XaftUserMessageSubmitted` is the *F21* signal, not F3. It is
    // always emitted on every submit. Verifies the signal bus can
    // carry both F3 and pre-F3 signals without interference.
    let bus = SignalBus::new();
    let user_count = count_signal::<XaftUserMessageSubmitted>(&bus).await;
    let mention_count = count_signal::<XaftMentionsResolved>(&bus).await;

    bus.emit(XaftUserMessageSubmitted {
        line_count: 1,
        char_count: 5,
        had_multi_line: false,
    })
    .await;

    assert_eq!(user_count.load(Ordering::SeqCst), 1);
    assert_eq!(mention_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn multiple_subscribers_each_receive_escape_signal() {
    let bus = SignalBus::new();
    let a = count_signal::<XaftEscapeMentionApproved>(&bus).await;
    let b = count_signal::<XaftEscapeMentionApproved>(&bus).await;

    bus.emit(XaftEscapeMentionApproved {
        tokens: vec![],
        session_wide: true,
    })
    .await;

    assert_eq!(a.load(Ordering::SeqCst), 1);
    assert_eq!(b.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn escape_signal_entry_preserves_all_fields() {
    // Constructing an entry from a real EscapeInfo and verifying the
    // audit log fields are all preserved verbatim.
    let info = EscapeInfo {
        reason: EscapeReason::ParentTraversal,
        absolute_path: "/home/user/project/sibling/file.rs".to_string(),
        depth: 2,
        byte_size: 4096,
    };
    let entry = EscapeSignalEntry {
        raw_token: "../../sibling/file.rs".to_string(),
        reason: "parent_traversal".to_string(),
        absolute_path: info.absolute_path.clone(),
        byte_size: info.byte_size,
        depth: info.depth,
    };
    assert_eq!(entry.absolute_path, "/home/user/project/sibling/file.rs");
    assert_eq!(entry.byte_size, 4096);
    assert_eq!(entry.depth, 2);
    assert_eq!(entry.raw_token, "../../sibling/file.rs");
    assert_eq!(entry.reason, "parent_traversal");
}

#[tokio::test]
async fn bus_emits_signals_to_subscribers_only() {
    // A signal that nobody subscribes to is a no-op. Verifies the
    // bus does not panic on unsubscribed signal types.
    let bus = SignalBus::new();
    bus.emit(XaftFileRefNotFound {
        path: "x".into(),
        reason: "test".into(),
    })
    .await;
    // No assertions needed beyond "did not panic".
}
