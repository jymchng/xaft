//! `xaft_tui::mention_signals` had no tests at all (0/47). These are the
//! builders that turn mention-resolution internals into the public F3
//! signal payloads, so a field silently dropped here produces an audit
//! log that looks well-formed and is wrong.
//!
//! Signal payload types derive `Debug + Clone` but not `PartialEq`, so
//! the assertions compare fields rather than whole structs.

use std::sync::{Arc, Mutex};

use agtrs_runtime::signals::SignalBus;
use agtrs_runtime::transport::{ContentBlock, EscapeInfo, EscapeReason, FileRefContent};

use xaft_agent::signals::EscapeSignalEntry;
use xaft_agent::signals::{XaftEscapeMentionApproved, XaftEscapeMentionDenied};
use xaft_tui::mention::MentionError;
use xaft_tui::mention_signals::{
    emit_escape_approved, emit_escape_denied, escape_signal_entry, file_ref_attached_entry,
    file_ref_blocks, file_ref_not_found_entry, mentions_resolved_entry, path_from_warning,
};

fn escape(reason: EscapeReason, abs: &str, depth: u32, size: u64) -> EscapeInfo {
    EscapeInfo {
        reason,
        absolute_path: abs.to_string(),
        depth,
        byte_size: size,
    }
}

#[test]
fn an_escape_entry_carries_the_token_reason_path_depth_and_size() {
    let info = escape(EscapeReason::ParentTraversal, "/etc/shadow", 2, 4096);
    let entry = escape_signal_entry(&info, "@../etc/shadow");

    assert_eq!(entry.raw_token, "@../etc/shadow");
    assert_eq!(entry.reason, "parent_traversal");
    assert_eq!(entry.absolute_path, "/etc/shadow");
    assert_eq!(entry.depth, 2);
    assert_eq!(entry.byte_size, 4096);
}

#[test]
fn each_escape_reason_maps_to_its_own_audit_label() {
    // The three labels are the audit log's vocabulary. Collapsing two of
    // them into one default arm would make the log unable to distinguish
    // an absolute path from a traversal.
    let cases = [
        (EscapeReason::Absolute, "absolute"),
        (EscapeReason::ParentTraversal, "parent_traversal"),
        (EscapeReason::HomeExpansion, "home_expansion"),
    ];
    for (reason, expected) in cases {
        let info = escape(reason, "/x", 0, 0);
        assert_eq!(escape_signal_entry(&info, "@x").reason, expected);
    }
}

#[test]
fn an_absolute_escape_is_labelled_absolute_even_though_its_depth_is_zero() {
    // Depth 0 is shared by Absolute and HomeExpansion, so the label is
    // the ONLY thing distinguishing them. This test fails if the label is
    // ever derived from depth instead of the reason enum.
    let abs = escape(EscapeReason::Absolute, "/etc/hosts", 0, 10);
    let home = escape(EscapeReason::HomeExpansion, "/root/.ssh/id_rsa", 0, 20);
    assert_eq!(abs.depth, home.depth);
    assert_eq!(escape_signal_entry(&abs, "a").reason, "absolute");
    assert_eq!(escape_signal_entry(&home, "b").reason, "home_expansion");
}

#[test]
fn a_workspace_relative_file_ref_uses_its_own_path_as_the_canonical_one() {
    let content = FileRefContent::Text("hello".into());
    let attached = file_ref_attached_entry("src/main.rs", &content, 5, 1, "abc123", None);

    assert_eq!(attached.path, "src/main.rs");
    // With no escape info there is nothing to canonicalise against, so
    // canonical_path must fall back to the literal path rather than
    // becoming empty.
    assert_eq!(attached.canonical_path, "src/main.rs");
    assert_eq!(attached.byte_size, 5);
    assert_eq!(attached.line_count, 1);
    assert_eq!(attached.sha256, "abc123");
    assert!(!attached.is_escape);
}

#[test]
fn an_escaping_file_ref_reports_the_canonical_path_and_is_flagged_as_an_escape() {
    let content = FileRefContent::Text("secret".into());
    let info = escape(EscapeReason::Absolute, "/etc/passwd", 0, 42);
    let attached =
        file_ref_attached_entry("../etc/passwd", &content, 42, 1, "deadbeef", Some(&info));

    // The literal token stays in `path`; only `canonical_path` resolves.
    assert_eq!(attached.path, "../etc/passwd");
    assert_eq!(attached.canonical_path, "/etc/passwd");
    assert!(attached.is_escape);
    assert_ne!(attached.path, attached.canonical_path);
}

#[test]
fn an_image_file_ref_is_accepted_the_same_way_a_text_one_is() {
    // `content` is not read by the builder, so the image variant must not
    // be rejected or dropped by it.
    let image = FileRefContent::Image {
        media_type: "image/png".into(),
        data_base64: "iVBORw0KGgo=".into(),
    };
    let attached = file_ref_attached_entry("logo.png", &image, 1024, 0, "sha", None);
    assert_eq!(attached.path, "logo.png");
    // Images always report a zero line count.
    assert_eq!(attached.line_count, 0);
    assert!(!attached.is_escape);
}

#[test]
fn mentions_resolved_carries_every_count_verbatim_including_the_zero_case() {
    let e = mentions_resolved_entry(4, 3, 1, 2, 8_192);
    assert_eq!(e.mention_count, 4);
    assert_eq!(e.resolved_count, 3);
    assert_eq!(e.warning_count, 1);
    assert_eq!(e.escape_count, 2);
    assert_eq!(e.total_bytes, 8_192);

    // Zero mentions is a distinct case from "no mentions at all" and must
    // not be collapsed.
    let none = mentions_resolved_entry(0, 0, 0, 0, 0);
    assert_eq!(none.mention_count, 0);
    assert_eq!(none.total_bytes, 0);
}

#[test]
fn a_missing_file_ref_records_the_path_and_the_reason() {
    let e = file_ref_not_found_entry("docs/ghost.md", "file not found");
    assert_eq!(e.path, "docs/ghost.md");
    assert_eq!(e.reason, "file not found");
}

#[test]
fn file_ref_blocks_keeps_only_filerefs_and_preserves_their_order() {
    let text = FileRefContent::Text("a".into());
    let blocks = vec![
        ContentBlock::Text {
            text: "intro".into(),
        },
        ContentBlock::FileRef {
            path: "one.rs".into(),
            content: text.clone(),
            truncation: None,
            byte_size: 1,
            line_count: 1,
            sha256: "h1".into(),
            escape: None,
        },
        ContentBlock::Text {
            text: "middle".into(),
        },
        ContentBlock::FileRef {
            path: "two.rs".into(),
            content: text,
            truncation: None,
            byte_size: 2,
            line_count: 1,
            sha256: "h2".into(),
            escape: None,
        },
    ];

    let kept = file_ref_blocks(&blocks);
    assert_eq!(kept.len(), 2);
    let paths: Vec<&str> = kept
        .iter()
        .map(|b| match b {
            ContentBlock::FileRef { path, .. } => path.as_str(),
            _ => unreachable!("file_ref_blocks returned a non-FileRef block"),
        })
        .collect();
    assert_eq!(paths, vec!["one.rs", "two.rs"]);
}

#[test]
fn file_ref_blocks_of_a_message_with_no_files_is_empty_not_an_error() {
    let blocks = vec![ContentBlock::Text {
        text: "just words".into(),
    }];
    assert!(file_ref_blocks(&blocks).is_empty());
    assert!(file_ref_blocks(&[]).is_empty());
}

#[test]
fn path_from_warning_returns_the_path_for_every_variant_that_carries_one() {
    use EscapeReason as R;
    let cases: Vec<(MentionError, &str)> = vec![
        (
            MentionError::FileNotFound {
                path: "a.rs".into(),
            },
            "a.rs",
        ),
        (
            MentionError::EscapeRejected {
                raw: "@/etc/passwd".into(),
                reason: R::Absolute,
            },
            "@/etc/passwd",
        ),
        (
            MentionError::TooLarge {
                path: "big.bin".into(),
                size: 99,
                cap: 10,
            },
            "big.bin",
        ),
        (
            MentionError::NotTextOrImage {
                path: "x.exe".into(),
                head_hex: "4d5a".into(),
            },
            "x.exe",
        ),
    ];
    for (err, expected) in cases {
        assert_eq!(path_from_warning(&err), expected);
    }
}

#[test]
fn path_from_warning_of_an_empty_token_is_the_empty_string_not_a_placeholder() {
    // The user typed a bare `@`. Emitting a fake path here would put a
    // bogus entry in the audit log.
    assert_eq!(path_from_warning(&MentionError::EmptyPath), "");
}

#[tokio::test]
async fn emit_escape_approved_delivers_one_entry_per_mention_and_forwards_the_scope_flag() {
    let bus = SignalBus::new();
    let seen = Arc::new(Mutex::new(Vec::<XaftEscapeMentionApproved>::new()));
    let sink = Arc::clone(&seen);
    bus.on::<XaftEscapeMentionApproved>(move |e| sink.lock().unwrap().push(e.clone()))
        .await;

    let mentions = vec![
        escape(EscapeReason::Absolute, "/etc/hosts", 0, 1),
        escape(EscapeReason::HomeExpansion, "/root/x", 0, 2),
    ];
    emit_escape_approved(&bus, &mentions, true).await;

    let got = seen.lock().unwrap().clone();
    assert_eq!(got.len(), 1);
    assert!(got[0].session_wide);
    assert_eq!(got[0].tokens.len(), 2);
    // emit_escape_approved has no separate token argument, so the raw
    // token defaults to the absolute path.
    assert_eq!(got[0].tokens[0].raw_token, "/etc/hosts");
    assert_eq!(got[0].tokens[0].reason, "absolute");
    assert_eq!(got[0].tokens[1].raw_token, "/root/x");
    assert_eq!(got[0].tokens[1].reason, "home_expansion");
}

#[tokio::test]
async fn a_session_scoped_approval_is_distinguished_from_a_session_wide_one() {
    let bus = SignalBus::new();
    let seen = Arc::new(Mutex::new(Vec::<XaftEscapeMentionApproved>::new()));
    let sink = Arc::clone(&seen);
    bus.on::<XaftEscapeMentionApproved>(move |e| sink.lock().unwrap().push(e.clone()))
        .await;

    emit_escape_approved(&bus, &[escape(EscapeReason::Absolute, "/a", 0, 1)], false).await;
    assert!(!seen.lock().unwrap()[0].session_wide);
}

#[tokio::test]
async fn emit_escape_denied_carries_the_reason_and_the_same_entries() {
    let bus = SignalBus::new();
    let seen = Arc::new(Mutex::new(Vec::<XaftEscapeMentionDenied>::new()));
    let sink = Arc::clone(&seen);
    bus.on::<XaftEscapeMentionDenied>(move |e| sink.lock().unwrap().push(e.clone()))
        .await;

    emit_escape_denied(
        &bus,
        &[escape(EscapeReason::ParentTraversal, "/tmp/x", 1, 7)],
        "user pressed c",
    )
    .await;

    let got = seen.lock().unwrap().clone();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].reason, "user pressed c");
    assert_eq!(got[0].tokens.len(), 1);
    assert_eq!(got[0].tokens[0].absolute_path, "/tmp/x");
    assert_eq!(got[0].tokens[0].depth, 1);
}

#[tokio::test]
async fn denying_with_no_mentions_still_emits_so_the_log_records_the_dismissal() {
    let bus = SignalBus::new();
    let seen = Arc::new(Mutex::new(Vec::<XaftEscapeMentionDenied>::new()));
    let sink = Arc::clone(&seen);
    bus.on::<XaftEscapeMentionDenied>(move |e| sink.lock().unwrap().push(e.clone()))
        .await;

    emit_escape_denied(&bus, &[], "nothing to approve").await;
    let got = seen.lock().unwrap().clone();
    assert_eq!(got.len(), 1);
    assert!(got[0].tokens.is_empty());
    assert_eq!(got[0].reason, "nothing to approve");
}

#[test]
fn the_escape_signal_entry_keeps_the_two_paths_distinct() {
    // Guards the one asymmetry in the builder: `raw_token` is what the
    // user typed, `absolute_path` is where it resolved. They coincide
    // here only because the emitter passes the path as the token.
    let info = escape(EscapeReason::Absolute, "/etc/hosts", 0, 3);
    let manual: EscapeSignalEntry = escape_signal_entry(&info, "/etc/hosts");
    assert_eq!(manual.raw_token, manual.absolute_path);
    assert_eq!(manual.byte_size, 3);
}
