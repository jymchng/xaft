# PRD 30b — @-Mention File Inputs (Implementation & Rollout)

**Status:** Proposal
**Target:** next minor release (M1 of the parity roadmap)
**Companion PRD:** `30a_mention_design.md`

---

## 11. Files & line-level changes

### 11.1 Upstream (`agtrs-runtime`)

| File | Change | Est. LOC |
|---|---|---|
| `agtrs-runtime/src/transport.rs` | Add `ContentBlock::FileRef { path, content, truncation, byte_size, line_count, sha256, escape: Option<EscapeInfo> }`. Add `FileRefContent`, `Truncation`, `EscapeInfo`, `EscapeReason` types. `EscapeReason` is `#[serde(rename_all = "snake_case")]` (`absolute`, `parent_traversal`, `home_expansion`). Mark `FileRef` `#[serde(rename = "file_ref")]`. Add `Message::user_with_parts(Vec<ContentBlock>)` constructor that returns a `MultiPart` `Message`. | +115 |
| `agtrs-runtime/src/transport.rs` | Add `MessageContent::MultiPart` constructor helper that collapses to `Text` if `parts` is a single `Text` block. | +15 |
| `agtrs-runtime/src/lib.rs` | Re-export `FileRefContent`, `Truncation`, `EscapeInfo`, `EscapeReason`. | +4 |

**Upstream total:** ~134 LOC. (Path dep: `crates/xaft-agent/Cargo.toml:34-46` points to `/root/rust_projects/agtrs/agtrs-runtime`.)

### 11.2 Workspace store

| File | Change | Est. LOC |
|---|---|---|
| `agtrs-workspace/src/store.rs` | Add `async fn read_bytes(&self, path: &str) -> Result<Vec<u8>, AgtrsError>` with default implementation. Add `async fn head_bytes(...)` for the byte-cap case. `InMemoryWorkspaceStore` switches from `HashMap<String, String>` to `HashMap<String, Vec<u8>>` (see §14.1). | +90 |
| `crates/xaft-tools/src/fs_store.rs` (or wherever `FsWorkspaceStore` lives) | Override `read_bytes` to use `tokio::fs::read`. | +15 |

**Workspace total:** ~105 LOC.

### 11.3 TUI

| File | Change | Est. LOC |
|---|---|---|
| `crates/xaft-tui/src/mention.rs` | **New** — `MentionToken`, `MentionError`, `MentionResolver`, `find_mentions(&str) -> Vec<MentionToken>`, `expand(&str, &WorkspaceStore, &MentionConfig) -> ExpandedMessage`. The `ResolvedFile` struct now carries `escape: Option<EscapeInfo>` (§5.5 of 30a). Includes ~35 unit tests (5 added for the escape path). | ~750 |
| `crates/xaft-tui/src/confirm.rs` | **New** — `EscapeConfirmDialog` widget. Renders the centred modal dialog (border, file list with absolute paths + sizes + reasons, key hints, "danger zone" hint when input is empty). Handles `a` / `A` / `c` / `Esc` / `Enter` / `?` key events. Returns an `EscapeConfirmOutcome` enum (`ApproveOnce`, `ApproveAllSession`, `Cancel`). | ~280 |
| `crates/xaft-tui/src/lib.rs` | `pub mod mention; pub mod confirm;` + re-exports. | +7 |
| `crates/xaft-tui/src/state.rs` | (a) Submit handler: call `mention::expand`, commit literal user line + warning lines + "N files attached" hint, send `UserMessage` over the typed channel. Migrate `user_message_tx: UnboundedSender<String>` → `UnboundedSender<UserMessage>`. (b) **New fields** for the confirmation flow: `pending_escape_mentions: Option<Vec<EscapeInfo>>` (set when the resolver produces escape mentions; cleared on approve/deny), `pending_expanded_message: Option<ExpandedMessage>` (held until approval), `escape_approved_for_session: bool` (default `false`), `escape_dialog: Option<EscapeConfirmDialog>`, `pending_input_restore: Option<String>` (input bar text to restore on deny). (c) New render path: if `escape_dialog.is_some()`, route all key events to the dialog first, then fall through to the input bar. | +180 (net) |
| `crates/xaft-tui/src/app.rs` | (a) Construct `UserMessage` channel instead of `String` channel. (b) Add `SignalsTx` for the new `XaftEscapeMentionApproved` / `XaftEscapeMentionDenied` signals. (c) Wire `escape_dialog` key handling into the main event loop. | +25 |
| `crates/xaft-tui/src/user_message.rs` | **New** — `pub enum UserMessage { Text(String), MultiPart(Vec<ContentBlock>) }` with conversion helpers (`Into<Message>`, `as_text_lossy`). | ~50 |
| `crates/xaft-tui/src/renderer.rs` | Render the `EscapeConfirmDialog` when `escape_dialog.is_some()`. Add a helper `draw_escape_dialog(frame, area, dialog, theme)` that draws the centred modal. The existing `draw_prompt_block` is unchanged; the dialog overlays it. | +60 |
| `crates/xaft-tui/tests/mention.rs` | **New** — integration tests driving `AppState` with simulated `@`-mention inputs. | ~300 |
| `crates/xaft-tui/tests/escape_confirmation.rs` | **New** — integration tests for the confirmation flow: dialog appears on escape mention, `a` approves, `A` sets session flag, `c` cancels and restores input, session flag is respected on subsequent submissions, multi-mention batch is approved/denied atomically. | ~250 |

**TUI total:** ~1 902 LOC.

### 11.4 Runtime

| File | Change | Est. LOC |
|---|---|---|
| `crates/xaft-runtime/src/dispatch.rs` | Add `RunRequest::user_message: Option<UserMessage>`. Update `RuntimeDispatch::run` to handle both `task: String` and `user_message: Option<UserMessage>`. | +40 |
| `crates/xaft-runtime/src/user_message.rs` | **New** — `UserMessage` enum (re-exported from `xaft-tui` to keep a single canonical type, or defined here and re-exported from `xaft-tui`). | ~50 (or just `pub use` of the TUI type) |
| `crates/xaft-runtime/src/orchestrator.rs` | Pass the structured parts to `HandoffRunParams::user_parts` when the request came in via `user_message`. | +15 |

**Runtime total:** ~105 LOC.

### 11.5 Agent framework (`agtrs-runtime` orchestrator)

| File | Change | Est. LOC |
|---|---|---|
| `agtrs-runtime/src/team.rs` | Add `HandoffRunParams::user_parts: Option<Vec<ContentBlock>>`. In `HandoffOrchestrator::run` and `run_stream`, build the first user `Message` from `user_parts` if present. | +40 |
| `agtrs-runtime/src/team.rs` | Ensure the `ConversationStore` receives a properly-serialised `Message::MultiPart` (the existing `agtrs-persist` already handles `ContentBlock` round-trips; we just need to make sure `FileRef` is `Serialize + Deserialize`, which it is by construction in 11.1). | +5 |
| `agtrs-anthropic/src/transport.rs` | Map `ContentBlock::FileRef` → Anthropic `text` content with the `<file path="…" lines="…" sha="…">…</file>` header. Split if > 200 000 chars. | +60 |
| `agtrs-openai/src/transport.rs` | Same as Anthropic, OpenAI-flavoured. | +60 |
| `agtrs-runtime/src/provider.rs` | Capability check helper: `provider.supports_vision() → bool` (already exists from F5; we just route image mentions through it). | +5 |

**Agent framework total:** ~170 LOC.

### 11.6 Agent (xaft)

| File | Change | Est. LOC |
|---|---|---|
| `crates/xaft-agent/src/signals.rs` | Add `XaftMentionsResolved`, `XaftFileRefAttached`, `XaftFileRefNotFound`, `XaftEscapeMentionApproved`, `XaftEscapeMentionDenied`. | +140 |
| `crates/xaft-agent/src/lib.rs` | Re-export new signal types. | +5 |
| `crates/xaft-agent/src/router.rs` (or wherever the `TuiEvent → XaftSignal` bridge lives) | (a) Map `UserMessage::MultiPart` to the new resolution signals. (b) Map `EscapeConfirmDialog` outcomes to `XaftEscapeMentionApproved` (with `session_wide` flag) and `XaftEscapeMentionDenied`. | +55 |

**Agent total:** ~200 LOC.

### 11.7 Config

| File | Change | Est. LOC |
|---|---|---|
| `crates/xaft-config/src/types.rs` | Add `MentionConfig { max_inline_lines: usize, max_inline_bytes: usize, image_max_bytes: usize, resolver_max_file_bytes: usize, dedupe: bool, escape_policy: EscapePolicy, escape_allowlist: Vec<String> }`. `EscapePolicy` enum: `Confirm` (default), `Always`, `Never`. `escape_allowlist` is a `Vec<String>` of glob patterns matched against the canonicalised absolute path. Add `XaftConfig::mention: MentionConfig`. | +85 |
| `crates/xaft-config/src/defaults.rs` | Default values for `MentionConfig` and `EscapePolicy::Confirm`. | +18 |
| `crates/xaft-config/src/validate.rs` | Validate `escape_allowlist` patterns: each pattern must be a valid glob (use the `globset` crate if available, or hand-roll a basic parser); reject empty patterns; warn (not error) on patterns that match nothing in the user's filesystem. | +30 |
| `crates/xaft-config/src/migrate.rs` | Add a migration step: for users with an existing `xaft.toml` that has no `[mention]` section, write `escape_policy = "confirm"` as the explicit default and add a one-time `[CHANGELOG]` line `"# mention.escape_policy was added; default is 'confirm' (was 'never' before F3)"`. | +20 |

**Config total:** ~153 LOC.

### 11.8 Tools

| File | Change | Est. LOC |
|---|---|---|
| `crates/xaft-tools/src/fs/read_file.rs` | Add `head_mode: bool` input flag that, when `true`, returns the first `max_inline_lines` lines plus a `(truncated, N more lines)` note (delegating to the same logic as the mention resolver's text case). | +30 |
| `crates/xaft-tools/src/error.rs` | Promote `validate_path` to `pub fn` and re-export as `xaft_tools::path::validate_relative`. | +10 |

**Tools total:** ~40 LOC.

### 11.9 Tests

| File | Scope | Est. LOC |
|---|---|---|
| `crates/xaft-tui/src/mention.rs` (inline) | Unit tests: parser, resolver, security, truncation, images. | included in 11.3 |
| `crates/xaft-tui/tests/mention.rs` | Integration: drive `AppState` end-to-end with `TuiHarness`. | included in 11.3 |
| `crates/xaft-runtime/tests/mention_orchestration.rs` | **New** — verify `RunRequest::user_message` flows through `XaftRuntime::run` into the orchestrator's first user `Message`. | ~200 |
| `crates/xaft-tools/tests/read_file_head.rs` | **New** — tests for the new `head_mode` flag. | ~80 |
| `crates/xaft-config/tests/mention_config.rs` | **New** — config defaults + overrides. | ~60 |
| `crates/xaft-agent/tests/mention_signals.rs` | **New** — verify the three new signals fire with correct payloads. | ~100 |

**Tests total:** ~440 LOC (separate from inline unit tests).

---

## 11.10 Grand total

| Crate | LOC (incl. tests) |
|---|---|
| `agtrs-runtime` (upstream) | ~134 |
| `agtrs-workspace` (upstream) | ~105 |
| `agtrs-anthropic` (upstream) | ~60 |
| `agtrs-openai` (upstream) | ~60 |
| `xaft-tui` | ~1 902 |
| `xaft-runtime` | ~105 |
| `xaft-agent` | ~200 |
| `xaft-config` | ~153 |
| `xaft-tools` | ~40 |
| Tests (separate) | ~440 |
| **Total** | **~3 199 LOC** |

The TUI grew by ~782 LOC and the config grew by ~103 LOC to accommodate
the escape confirmation flow (new widget, new state fields, new dialog
renderer, new signals, new config keys, migration step). This is
within the budget allocated to F3 in the parity roadmap; no scope
expansion beyond what the escape policy requires.

No new crate dependencies. `regex`, `base64`, `tokio`, `serde`, `sha2`
(all needed) are already in the workspace. If `globset` is not already
in `xaft-config`'s `Cargo.toml` for the `escape_allowlist` validation,
hand-roll a basic glob-to-regex conversion (~30 LOC, supports `*`, `**`,
`?`); this avoids adding a dep just for one config field.

---

## 12. Tests

### 12.1 Unit tests in `crates/xaft-tui/src/mention.rs`

| Test | Asserts |
|---|---|
| `parser_finds_single_mention` | input `"see @src/lib.rs"` → 1 token, range correct |
| `parser_finds_multiple_mentions` | input `"fix @a.rs and @b.rs"` → 2 tokens |
| `parser_handles_dot_slash_prefix` | input `"@./local.txt"` → token, path = `./local.txt` |
| `parser_handles_parent_traversal` | input `"@../sibling.txt"` → 1 token, path = `../sibling.txt` |
| `parser_handles_absolute_path` | input `"@/etc/hosts"` → 1 token, path = `/etc/hosts` |
| `parser_handles_home_expansion` | input `"@~/notes.md"` → 1 token, path = `~/notes.md` (NOT expanded at parse time) |
| `parser_handles_other_user_home` | input `"@~root/.bashrc"` → 1 token, path = `~root/.bashrc` |
| `parser_rejects_email` | input `"user@example.com"` → 0 tokens |
| `parser_rejects_social_handle` | input `"thanks @alice!"` → 0 tokens (no path separator) |
| `parser_stops_at_whitespace` | input `"@src/lib.rs and more"` → 1 token, end at space |
| `parser_stops_at_colon` | input `"@src/main.rs:42"` → 1 token (path ends at `:`) |
| `parser_stops_at_period` | input `"@docs/adr.md."` → 1 token (path ends before `.`) |
| `parser_handles_escaped_at` | input `"\@not a mention"` → 0 tokens |
| `parser_handles_at_inside_word` | input `"user@host"` → 0 tokens (no word boundary before `@`) |
| `parser_handles_empty_path` | input `"@"` → 0 tokens |
| `parser_handles_trailing_slash` | input `"@src/"` → 0 tokens (no body after `/`) |
| `parser_allows_multiline_mention` | input `"@src/\nfoo.rs"` → 1 token spanning newlines |
| `parser_unicode_safe` | input with emoji → ranges are byte-correct (no panic) |
| `parser_idempotent` | Running `find_mentions` twice on the same input produces the same output |
| `parser_preserves_raw` | Token's `raw` field includes the leading `@` |
| `resolver_reads_text_file` | `@src/lib.rs` (file exists) → `ResolvedFile` with correct content |
| `resolver_truncates_large_lines` | 2 500-line file → content has 2 000 lines + truncation note |
| `resolver_truncates_large_bytes` | 60 000-byte file → content has ≤ 50 000 bytes + truncation note |
| `resolver_truncation_includes_read_more_hint` | Truncation note includes "use read_file with start_line=N+1" |
| `resolver_records_sha256` | Resolved `FileRef` has the correct sha256 of the original file |
| `resolver_reads_image_png` | `@logo.png` → `ResolvedFile` with `Image` content, base64-decodable |
| `resolver_reads_image_jpeg_by_magic` | Renamed `.png` that is actually a JPEG → still `image/jpeg` |
| `resolver_rejects_unknown_extension_no_magic` | `@data.bin` with random bytes → `Unresolved` with "not a recognised file type" |
| `resolver_accepts_escape_path_with_escape_info` | `@../etc/hosts` → `ResolvedFile` with `escape = Some(EscapeInfo { reason: ParentTraversal, absolute_path, depth: 1, byte_size })`. **Replaces** the old `resolver_rejects_path_traversal` test. |
| `resolver_accepts_absolute_path_with_escape_info` | `@/etc/hosts` → `ResolvedFile` with `escape = Some(EscapeInfo { reason: Absolute, absolute_path: "/etc/hosts", depth: 0, byte_size })`. **Replaces** the old `resolver_rejects_absolute_path` test. |
| `resolver_handles_home_expansion` | `@~/notes.md` (HOME=/home/alice, file exists at `/home/alice/notes.md`) → `ResolvedFile` with `escape.reason = HomeExpansion`, `absolute_path = /home/alice/notes.md`. |
| `resolver_records_symlink_escape` | Workspace contains a symlink `safe → /etc/hosts`; `@safe` → `ResolvedFile` with `escape.reason = Absolute` (canonicalised path is outside workspace). **Replaces** the old `resolver_rejects_symlink_escape` test. |
| `resolver_rejects_oversized_image` | 12 MB image with `image_max_bytes = 10 MB` → `Unresolved` with size warning |
| `resolver_rejects_oversized_file_resolver_cap` | 200 MB file with `resolver_max_file_bytes = 100 MB` → `Unresolved` with size warning (applies to escape paths too) |
| `resolver_returns_unchanged_text_when_no_mentions` | `"hello world"` → 1 `Text` block, no warnings |
| `resolver_preserves_literal_for_missing_file` | `@missing.rs` → `Unresolved`, the literal `@missing.rs` is in the output `Text` block |
| `resolver_collapses_single_text_to_text_variant` | Message with only 1 `Text` block → `MessageContent::Text`, not `MultiPart` |
| `resolver_handles_mixed_text_and_files` | `"see @a.rs and @b.png"` → 1 Text + 1 FileRef + 1 Text + 1 Image (or 4 Text if vision is off) |
| `resolver_records_warnings` | 2 unresolved mentions → `warnings.len() == 2` with correct `MentionError` |
| `resolver_warning_text_format` | Warning text is `"⚠ File not found: <path>"` (or appropriate variant) |
| `resolver_dedupe_disabled_in_v1` | 3× the same file in one message → 3 `FileRef` blocks (per §16 open question 2) |
| `resolver_handles_empty_file` | `@empty.txt` (0 bytes) → `FileRef` with `FileRefContent::Text("")` and line_count 0 |
| `resolver_handles_crlf_normalisation` | File with CRLF line endings → line_count counts each `\n` (so 3 lines for 3 CRLFs) |
| `resolver_handles_invalid_utf8` | File with invalid UTF-8 bytes → content has `U+FFFD` replacements, warning recorded |
| `resolver_escape_info_has_canonical_path` | `@../symlink_target` where the target is `/etc/hosts` → `escape.absolute_path == "/etc/hosts"` (canonicalised), not the literal `../symlink_target`. |
| `resolver_escape_info_depth_counts_traversal_segments` | `@../../foo` → `escape.depth == 2` |
| `resolver_escape_policy_never_rejects_at_resolve` | Config `escape_policy = "never"` → resolver **still** attaches the file, but the **dialog** (in a separate test) shows a "workspace escape disabled" warning and the message is dropped. The resolver itself is policy-agnostic; the dialog enforces `never`. |

### 12.2 Integration tests in `crates/xaft-tui/tests/mention.rs`

```rust
#[tokio::test]
async fn submit_with_one_mention_attaches_file() {
    let mut harness = TuiHarness::new();
    harness.workspace().write("src/lib.rs", "fn foo() {}").await;
    harness.type_str("explain @src/lib.rs");
    harness.press(key("enter"));

    // TUI side
    assert_eq!(harness.transcript_last_user_line(), "❯ explain @src/lib.rs");
    assert!(harness.transcript_contains("1 file attached: src/lib.rs (1 lines)"));

    // Wire side
    let msg = harness.next_user_message().await;
    let UserMessage::MultiPart(parts) = msg else { panic!() };
    assert_eq!(parts.len(), 2);
    assert!(matches!(&parts[0], ContentBlock::Text { text } if text == "explain "));
    let ContentBlock::FileRef { path, content, .. } = &parts[1] else { panic!() };
    assert_eq!(path, "src/lib.rs");
    let FileRefContent::Text(s) = content else { panic!() };
    assert!(s.contains("fn foo()"));
}

#[tokio::test]
async fn submit_with_unresolved_mention_warns_but_sends() {
    let mut harness = TuiHarness::new();
    harness.type_str("update @missing.rs");
    harness.press(key("enter"));

    assert!(harness.transcript_contains("⚠ File not found: missing.rs"));

    let msg = harness.next_user_message().await;
    let UserMessage::MultiPart(parts) = msg else { panic!() };
    assert_eq!(parts.len(), 1);
    let ContentBlock::Text { text } = &parts[0] else { panic!() };
    assert!(text.contains("@missing.rs"));
}

#[tokio::test]
async fn submit_with_image_mention_attaches_base64() {
    let mut harness = TuiHarness::new();
    // Write a 1×1 red PNG.
    let png_bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90wS\xde\x00\x00\x00\x0cIDAT\x08\x99c\xf8\xcf\xc0\x00\x00\x00\x03\x00\x01\x5b\xd6\x35\x4a\x00\x00\x00\x00IEND\xaeB`\x82";
    harness.workspace().write_bytes("logo.png", png_bytes).await;
    harness.type_str("what's in @logo.png?");
    harness.press(key("enter"));

    let msg = harness.next_user_message().await;
    let UserMessage::MultiPart(parts) = msg else { panic!() };
    let ContentBlock::Image { media_type, data, .. } = parts.iter().find_map(|b| match b {
        ContentBlock::Image { media_type, data, .. } => Some((media_type, data)),
        _ => None,
    }).unwrap() else { panic!() };
    assert_eq!(media_type, "image/png");
    let decoded = base64::decode(data.as_deref().unwrap()).unwrap();
    assert_eq!(decoded, png_bytes);
}

#[tokio::test]
async fn submit_with_traversal_attempt_pauses_for_confirmation() {
    // v0.1 behaviour was: reject with warning, literal preserved.
    // v0.2 behaviour: pause for confirmation dialog. The message is NOT
    // sent until the user approves.
    let mut harness = TuiHarness::new();
    harness.workspace().write("../sibling.txt", "sibling content").await;
    harness.type_str("cat @../sibling.txt");
    harness.press(key("enter"));

    // The dialog is visible; no message has been sent yet.
    assert!(harness.escape_dialog_visible());
    assert_eq!(harness.transcript_user_line_count(), 0);

    // The user approves once.
    harness.press(key("a"));

    // Now the message is sent.
    let msg = harness.next_user_message().await;
    let UserMessage::MultiPart(parts) = msg else { panic!() };
    let ContentBlock::FileRef { path, escape, .. } =
        parts.iter().find_map(|b| match b {
            ContentBlock::FileRef { path, escape, .. } => Some((path, escape)),
            _ => None,
        }).unwrap() else { panic!() };
    assert_eq!(path, "../sibling.txt");
    let info = escape.as_ref().unwrap();
    assert!(matches!(info.reason, EscapeReason::ParentTraversal));
    assert!(info.absolute_path.ends_with("sibling.txt"));
    assert_eq!(info.depth, 1);
}

#[tokio::test]
async fn submit_with_traversal_denied_restores_input() {
    let mut harness = TuiHarness::new();
    harness.workspace().write("../sibling.txt", "sibling content").await;
    harness.type_str("cat @../sibling.txt");
    harness.press(key("enter"));
    assert!(harness.escape_dialog_visible());

    // The user cancels.
    harness.press(key("c"));

    // The dialog closes, the input bar still contains the text, and no
    // message has been sent.
    assert!(!harness.escape_dialog_visible());
    assert_eq!(harness.input_text(), "cat @../sibling.txt");
    assert!(harness.next_user_message_try().await.is_none());
}

#[tokio::test]
async fn session_wide_approval_skips_dialog_on_subsequent_submissions() {
    let mut harness = TuiHarness::new();
    harness.workspace().write("../a.txt", "a").await;
    harness.workspace().write("../b.txt", "b").await;

    // First submission: dialog appears, user presses A.
    harness.type_str("see @../a.txt");
    harness.press(key("enter"));
    assert!(harness.escape_dialog_visible());
    harness.press(key("A"));
    let _ = harness.next_user_message().await;

    // Second submission: dialog does NOT appear (session-wide flag).
    harness.type_str("and @../b.txt");
    harness.press(key("enter"));
    assert!(!harness.escape_dialog_visible());
    let msg = harness.next_user_message().await;
    let UserMessage::MultiPart(parts) = msg else { panic!() };
    assert!(parts.iter().any(|b| matches!(b, ContentBlock::FileRef { .. })));
}

#[tokio::test]
async fn escape_allowlist_silently_attaches_matching_paths() {
    let mut harness = TuiHarness::new();
    harness.config().mention.escape_allowlist = vec!["~/notes/*.md".into()];
    std::fs::create_dir_all(harness.home().join("notes")).unwrap();
    std::fs::write(harness.home().join("notes/todo.md"), "todo").unwrap();

    harness.type_str("check @~/notes/todo.md");
    harness.press(key("enter"));

    // No dialog (matches allowlist).
    assert!(!harness.escape_dialog_visible());
    let msg = harness.next_user_message().await;
    let UserMessage::MultiPart(parts) = msg else { panic!() };
    let ContentBlock::FileRef { escape, .. } = parts.iter().find_map(|b| match b {
        ContentBlock::FileRef { escape, .. } => Some(escape),
        _ => None,
    }).unwrap() else { panic!() };
    assert!(matches!(escape.as_ref().unwrap().reason, EscapeReason::HomeExpansion));
}

#[tokio::test]
async fn escape_policy_never_blocks_all_escape_mentions() {
    let mut harness = TuiHarness::new();
    harness.config().mention.escape_policy = EscapePolicy::Never;
    harness.workspace().write("../sibling.txt", "x").await;
    harness.type_str("see @../sibling.txt");
    harness.press(key("enter"));

    // Dialog is NOT shown. The TUI commits the literal user message and
    // a warning, but the FileRef is dropped (only literal text is sent).
    assert!(!harness.escape_dialog_visible());
    assert!(harness.transcript_contains("⚠ Workspace escape disabled: ../sibling.txt"));
    let msg = harness.next_user_message().await;
    let text = msg.as_text_lossy();
    assert!(text.contains("@../sibling.txt"));
    assert!(!matches!(
        harness.wire_parts(&msg).iter().find(|b| matches!(b, ContentBlock::FileRef { .. })),
        Some(_)
    ));
}

#[tokio::test]
async fn submit_with_very_long_text_truncates() {
    let mut harness = TuiHarness::new();
    let content = (1..=3000).map(|i| format!("line {i}"))
        .collect::<Vec<_>>().join("\n");
    harness.workspace().write("big.rs", &content).await;
    harness.type_str("refactor @big.rs");
    harness.press(key("enter"));

    assert!(harness.transcript_contains("truncated: 2000 of 3000 lines"));
    let msg = harness.next_user_message().await;
    let UserMessage::MultiPart(parts) = msg else { panic!() };
    let ContentBlock::FileRef { content, truncation, .. } = &parts[1] else { panic!() };
    let FileRefContent::Text(s) = content else { panic!() };
    assert!(s.contains("line 1"));
    assert!(s.contains("line 2000"));
    assert!(!s.contains("line 2001"));
    assert!(s.contains("use read_file with start_line=2001"));
    let t = truncation.as_ref().unwrap();
    assert_eq!(t.shown_lines, 2000);
    assert_eq!(t.total_lines, 3000);
}

#[tokio::test]
async fn submit_preserves_multi_line_input_with_mention() {
    // F21 interaction: a multi-line input with a mention on line 2.
    let mut harness = TuiHarness::new();
    harness.workspace().write("a.rs", "x").await;
    harness.type_str("first line");
    harness.press(key("shift+enter"));
    harness.type_str("second line @a.rs");
    harness.press(key("enter"));

    let msg = harness.next_user_message().await;
    let UserMessage::MultiPart(parts) = msg else { panic!() };
    // 3 text segments + 1 file ref.
    assert!(parts.iter().any(|b| matches!(b, ContentBlock::FileRef { path, .. } if path == "a.rs")));
}

#[tokio::test]
async fn history_recall_replays_mention_inputs() {
    // F23 interaction: recalling a past prompt with a mention re-submits
    // the same tokens; the resolver re-runs against the current workspace.
    let mut harness = TuiHarness::new();
    harness.workspace().write("a.rs", "x").await;
    harness.type_str("explain @a.rs");
    harness.press(key("enter"));
    harness.wait_for_user_message().await;

    // New prompt — clear, recall via Ctrl+R.
    harness.press(key("ctrl+r"));
    harness.type_str("explain");
    harness.press(key("enter"));     // accept recall
    harness.press(key("enter"));     // submit

    let msg = harness.next_user_message().await;
    let UserMessage::MultiPart(parts) = msg else { panic!() };
    assert!(parts.iter().any(|b| matches!(b, ContentBlock::FileRef { path, .. } if path == "a.rs")));
}
```

### 12.3 Orchestration tests in `crates/xaft-runtime/tests/mention_orchestration.rs`

```rust
#[tokio::test]
async fn runtime_with_user_message_produces_multipart_first_message() {
    let runtime = XaftRuntime::with_workspace(workspace_with("a.rs", "hello"));
    let req = RunRequest {
        task: String::new(),                     // unused
        user_message: Some(UserMessage::MultiPart(vec![
            ContentBlock::Text { text: "see ".into() },
            ContentBlock::FileRef { path: "a.rs".into(), content: FileRefContent::Text("hello".into()), truncation: None, byte_size: 5, line_count: 1, sha256: sha256_of("hello") },
        ])),
        working_dir: runtime.workspace_root().to_path_buf(),
        config: XaftConfig::default(),
        headless: true,
        dry_run: false,
        auto_approve: true,
        dangerously_skip_permissions: false,
        resume_session_id: None,
        workflow: WorkflowConfig::Standard,
        prior_messages: vec![],
    };

    let result = runtime.run(req).await.unwrap();
    // The first user message in the session's conversation history should
    // be a MultiPart message.
    let history = runtime.session_history(&result.session.id).await;
    let first_user = history.iter().find(|m| m.role == Role::User).unwrap();
    assert!(matches!(first_user.content, MessageContent::MultiPart(_)));
}

#[tokio::test]
async fn runtime_with_plain_task_string_unchanged() {
    let runtime = XaftRuntime::new();
    let req = RunRequest {
        task: "fix the bug".into(),
        user_message: None,
        // ...
    };
    let result = runtime.run(req).await.unwrap();
    let history = runtime.session_history(&result.session.id).await;
    let first_user = history.iter().find(|m| m.role == Role::User).unwrap();
    assert!(matches!(first_user.content, MessageContent::Text(t) if t == "fix the bug"));
}

#[tokio::test]
async fn runtime_with_empty_user_message_falls_back_to_task() {
    let runtime = XaftRuntime::new();
    let req = RunRequest {
        task: "fix the bug".into(),
        user_message: Some(UserMessage::Text(String::new())),
        // ...
    };
    // Should behave identically to user_message: None.
    let result = runtime.run(req).await.unwrap();
    let history = runtime.session_history(&result.session.id).await;
    let first_user = history.iter().find(|m| m.role == Role::User).unwrap();
    assert!(matches!(first_user.content, MessageContent::Text(t) if t == "fix the bug"));
}
```

### 12.4 Tool tests in `crates/xaft-tools/tests/read_file_head.rs`

| Test | Asserts |
|---|---|
| `head_mode_returns_first_n_lines` | `read_file(path, head_mode: true)` returns 2 000 lines + truncation note |
| `head_mode_skips_truncation_note_for_small_files` | 100-line file → no truncation note |
| `head_mode_uses_max_inline_lines_from_config` | Config override → different line cap |
| `head_mode_records_sha256` | Output header includes sha256 |
| `head_mode_returns_correct_content_for_binary_rejected` | Binary file via `head_mode` → error result (mention resolver handles binaries via `read_bytes`, not via `read_file`) |

### 12.5 Config tests in `crates/xaft-config/tests/mention_config.rs`

| Test | Asserts |
|---|---|
| `mention_config_defaults` | All fields have the expected defaults (2 000 / 50 000 / 10 MB / 100 MB / `dedupe: false` / `escape_policy: Confirm` / `escape_allowlist: []`) |
| `mention_config_overrides_via_toml` | Custom values in `xaft.toml` are picked up |
| `mention_config_env_overrides` | `XAFT_MENTION_MAX_INLINE_LINES=5000` overrides |
| `mention_config_invalid_value_errors` | Negative value for `max_inline_lines` → `ConfigError` |
| `escape_policy_default_is_confirm` | Fresh `MentionConfig::default()` returns `escape_policy: Confirm` |
| `escape_policy_parses_from_toml` | `escape_policy = "always"` parses to `EscapePolicy::Always`; `"never"` → `Never`; `"confirm"` → `Confirm` |
| `escape_policy_unknown_value_errors` | `escape_policy = "maybe"` → `ConfigError::UnknownVariant` |
| `escape_allowlist_parses_glob_patterns` | `escape_allowlist = ["~/notes/*.md", "/usr/share/**/*.txt"]` parses without error |
| `escape_allowlist_invalid_glob_errors` | `escape_allowlist = ["["]` (unbalanced bracket) → `ConfigError` with line/column |
| `mention_config_migration_writes_escape_policy_for_existing_users` | Loading an existing `xaft.toml` (no `[mention]` section) writes `escape_policy = "confirm"` to a backup file with a one-time `[CHANGELOG]` comment |

### 12.6 Signal tests in `crates/xaft-agent/tests/mention_signals.rs`

| Test | Asserts |
|---|---|
| `mentions_resolved_signal_fires` | After a user message with 3 mentions (2 resolved, 1 not), `XaftMentionsResolved` is emitted with `total_tokens: 3, resolved: 2, unresolved: 1` |
| `file_ref_attached_signal_per_resolved` | Each resolved file emits its own `XaftFileRefAttached` with the right sha256 / line count |
| `file_ref_not_found_signal_per_unresolved` | Each unresolved file emits its own `XaftFileRefNotFound` |
| `no_signals_for_plain_text_message` | A message with no `@` tokens → no mention signals |
| `escape_mention_approved_signal_on_approve_once` | User presses `a` in dialog → `XaftEscapeMentionApproved { tokens, absolute_paths, reasons, session_wide: false }` |
| `escape_mention_approved_signal_session_wide` | User presses `A` in dialog → signal fires with `session_wide: true` |
| `escape_mention_denied_signal_on_cancel` | User presses `c` in dialog → `XaftEscapeMentionDenied { tokens, absolute_paths, reasons }` |
| `escape_mention_approved_signal_includes_all_escape_paths` | Submission with 3 escape mentions (parent + absolute + home) → signal's `tokens` / `absolute_paths` / `reasons` are all length 3 in input order |
| `no_escape_signals_when_no_escape_mentions` | Submission with only workspace-relative mentions → no `XaftEscapeMentionApproved` signal |

### 12.7 Backward-compat assertions

```rust
#[test]
fn existing_run_request_construction_unchanged() {
    // All existing callers of `RunRequest { task: ..., ... }` must still
    // compile by adding the new field with a default via `..Default::default()`.
    let req = RunRequest {
        task: "do the thing".into(),
        ..Default::default()
    };
    assert_eq!(req.task, "do the thing");
    assert!(req.user_message.is_none());
}

#[test]
fn existing_string_channel_constructor_still_compiles() {
    // The TUI's `user_message_tx: Option<UnboundedSender<String>>` becomes
    // `Option<UnboundedSender<UserMessage>>`. Callers that previously did
    // `s.user_message_tx = Some(tx)` where `tx: UnboundedSender<String>`
    // must now wrap: `s.user_message_tx = Some(tx.with_user_message())`.
    // (Provided as an extension trait on the channel to ease migration.)
}
```

---

## 13. Acceptance criteria

The three from PRD 27 §F3, plus deeper criteria from the design:

1. **(PRD 27)** `@src/main.rs` in the input bar expands to the file's
   content as a structured `FileRef` block in the on-wire message.
2. **(PRD 27)** Up/Down arrows and multi-line input (F21) work
   identically — the mention parser is line-agnostic.
3. **(PRD 27)** The original `@<path>` token is preserved in the user
   message text (for traceability).
4. **Workspace-relative paths attach without confirmation.** `@src/lib.rs`
   expands to a `FileRef` and is sent on the next message with no
   user interaction beyond the normal `Enter`-to-submit.
5. **Escape paths require per-submission confirmation.**
   `@../sibling.txt` causes the `EscapeConfirmDialog` to appear
   (§8.7 of 30a). The user must press `a` (approve once) or `A` (session
   wide) or `c` (cancel) before the message is sent. The input bar
   text is preserved on cancel.
6. **Absolute paths require confirmation.** `@/etc/hosts` triggers the
   same dialog with `EscapeReason::Absolute` and `absolute_path =
   "/etc/hosts"`.
7. **Home expansion requires confirmation.** `@~/notes.md` triggers the
   same dialog with `EscapeReason::HomeExpansion` and the canonicalised
   `$HOME/notes.md` absolute path.
8. **Symlink-canonicalised escape requires confirmation.** A workspace
   symlink `safe → /etc/hosts` mentioned as `@safe` triggers the dialog
   with `EscapeReason::Absolute` and `absolute_path = "/etc/hosts"`.
9. **Session-wide approval is opt-in.** Pressing `A` once sets
   `AppState::escape_approved_for_session = true`; subsequent escape
   mentions in the same session are attached silently.
10. **`escape_policy = "always"` is silent for allowlist matches.** A
    path matching `escape_allowlist` is attached without confirmation
    even when `escape_policy = "always"`.
11. **`escape_policy = "never"` blocks all escape mentions.** The
    literal token is preserved in the user message and a `⚠ Workspace
    escape disabled: <path>` warning is shown in the TUI.
12. **Large files truncate with a "use read_file" hint.** A 2 500-line
    file shows the first 2 000 lines + `// (truncated, 500 more lines; use
    read_file with start_line=2001)`.
13. **Image files embed as base64.** `@logo.png` produces an
    `Image` content block with the file's bytes base64-encoded and the
    correct `media_type` detected from magic bytes.
14. **Image too large is rejected.** A 12 MB image with
    `image_max_bytes = 10 MB` produces a warning and a path-only stub in
    the message.
15. **Unknown path is left as literal text.** `@missing.rs` produces a
    `⚠ File not found: missing.rs` warning in the TUI and the literal
    `@missing.rs` in the user message.
16. **Multiple mentions in one message expand correctly.** "see
    `@a.rs` and `@b.rs`" produces 2 `FileRef` blocks separated by a
    `Text` block.
17. **`@<tab>` opens a file picker (F22).** When the F22 completer is
    implemented, `@src/<tab>` lists the files under `src/`.
18. **`cargo test --workspace` passes** with the new tests included
    (target: +35 new tests across the workspace, +5 in the
    `escape_confirmation` integration test file).
19. **`cargo clippy --workspace -- -D warnings` is clean** for new
    code. (Pre-existing warnings documented in 30a §12 remain.)
20. **`cargo fmt -- --check` passes.**
21. **The runtime correctly distinguishes `task: String` from
    `user_message: UserMessage`.** A request with `user_message: Some`
    produces a `MultiPart` first user message; a request with
    `user_message: None` and `task: String` produces a `Text` first user
    message (regression-free for existing callers).
22. **Session resume re-attaches `FileRef` blocks with staleness
    warnings.** When a session is resumed and a referenced file has
    changed, the TUI shows a per-file `⚠ src/lib.rs has changed since
    attached` line.
23. **Audit log records mention resolutions AND escape approvals/
    denials.** Replaying a session shows the original `@<path>` tokens,
    the attached file metadata (path, sha, byte size), AND for every
    escape mention whether the user approved or denied the attachment
    along with the canonical absolute path that was on disk at the time.
24. **`XaftEscapeMentionApproved` and `XaftEscapeMentionDenied` fire
    exactly once per submission.** A single approval/denial event
    covers all escape mentions in that submission, not one event per
    individual mention.

---

## 14. Migration & backward compatibility

### 14.1 Breaking changes

| Change | Impact | Mitigation |
|---|---|---|
| `WorkspaceStore::read` semantics unchanged but `InMemoryWorkspaceStore` switches from `String` to `Vec<u8>`. | Direct field access breaks. | Provide a `String` view via `read_string` (calls `read_bytes` and decodes UTF-8 with lossy fallback). |
| `RunRequest` gains a new field `user_message: Option<UserMessage>`. | Existing call sites that use struct-literal syntax must add the field. | Provide a `Default` impl on `RunRequest`; all existing call sites use `..Default::default()`. |
| `user_message_tx: Option<UnboundedSender<String>>` → `Option<UnboundedSender<UserMessage>>`. | TUI runtime handover changes type. | Provide a `WithUserMessage` extension trait on `UnboundedSender<String>` that wraps the sender at construction time. Migration is a one-line change at the call site. |
| `HandoffRunParams` gains `user_parts: Option<Vec<ContentBlock>>`. | External `agtrs-runtime` consumers must add the field. | Default to `None`. Documented in `agtrs-runtime` CHANGELOG. |
| `ContentBlock` gains `FileRef` variant. | Downstream `match` blocks that don't have a wildcard arm will fail to compile. | Add a unit-test assertion that all internal `match` blocks are exhaustive; document the new variant in `agtrs-runtime::transport::CHANGELOG.md`. |

### 14.2 Non-breaking changes

| Change | Impact |
|---|---|
| `xaft-config` gains `XaftConfig::mention: MentionConfig` with defaults. | Existing configs without `[mention]` continue to work; serde fills in defaults via `#[serde(default)]`. |
| `xaft-agent::signals` gains 3 new variants. | Existing consumers that exhaustively match `XaftSignal` (none in the workspace) must add the new arms. |
| `agtrs-runtime::transport::Message` gains `Message::user_with_parts(...)`. | Purely additive. |

### 14.3 Deprecation policy

No public API is **removed** in this PRD. All additions are backwards-
compatible. The `format_prompt_line` deprecation pattern from F21
applies: any new function we add is preferred; the old function remains
as a shim for one release.

### 14.4 Data migration

None. `MentionConfig` uses `#[serde(default)]`, so existing
`xaft.toml` files load unchanged. `FileRef` blocks in stored sessions
are read back via the existing `Message::Deserialize` path; old
sessions without `FileRef` blocks deserialize unchanged.

### 14.5 Rollout

The feature ships behind a feature flag in v0.2: `[mention] enabled = true`
(default: `true` for new users, `false` for users with an existing
`xaft.toml` to give them a chance to opt in). After one release with
no reported issues, the default flips to `true` unconditionally.

Wait — re-reading the goals, this is not what we want. The default
should be `true` for everyone; the mention syntax is opt-out (the user
can simply not type `@`). There is no behavioural change for users who
don't use `@`. So no feature flag in the config; the feature is on by
default and the only opt-out is "don't type `@`".

---

## 15. Risk analysis

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| User types `@` in a non-mention context (e.g. an email address) and the parser misinterprets it | Medium | Low — false positive becomes a `⚠ File not found` warning + literal text preserved | §4.3 anti-false-positive rules + the existence probe for single-component paths. Tested in `parser_rejects_email` and `parser_rejects_social_handle`. |
| Mention expands a binary file as text (garbled UTF-8) | Low | Low — the LLM sees garbled text and ignores it | Magic-byte detection for images; for other binaries, the resolver's `resolver_max_file_bytes` cap + lossy UTF-8 fallback + warning line. |
| User types `@<path>` for a very large file and accidentally inflates their token bill | Medium | Medium — bill shock | The default `max_inline_bytes = 50 000` cap. The `/cost` panel shows per-prompt file bytes. The TUI commit line says "N files attached" so the user can spot large attachments. |
| **User accidentally attaches a sensitive file via escape mention** (e.g. `@~/.aws/credentials` recalled from history or auto-completed by an IDE) | Medium | High — secret disclosure to LLM provider | Per-submission confirmation dialog (§8.7 of 30a) shows the **absolute canonical path** before the file is sent. User must press `a`/`A` to confirm; `c`/`Esc` cancels and preserves the input for editing. The `escape_policy = "never"` setting (or removing it from `escape_allowlist`) reverts to the v0.1 reject behaviour. Audit log records the absolute path + sha256 of every approved attachment. Tested in `submit_with_traversal_attempt_pauses_for_confirmation`, `submit_with_traversal_denied_restores_input`, and `escape_policy_never_blocks_all_escape_mentions`. |
| Symlink-based escape from the workspace (workspace contains `safe → /etc/hosts`; user types `@safe`) | Medium | High — file disclosure | §5.3 canonicalisation check (30a) detects the symlink target is outside the workspace and sets `EscapeInfo::Absolute`, which triggers the confirmation dialog. Tested in `resolver_records_symlink_escape`. |
| User enables `escape_policy = "always"` and adds an overly-broad `escape_allowlist` pattern (e.g. `~/**`) | Low | High — broad disclosure without per-file confirmation | The dialog still appears for any escape path **not** matching the allowlist. The allowlist is matched against the canonicalised absolute path, not the literal token, so it is hard to bypass with `..` tricks. Documented in §8.7 of 30a. The `[CHANGELOG]` note in the migration step (§14.4) warns the user. |
| `escape_approved_for_session = true` is left set across distinct tasks in the same TUI session | Low | Medium — different task in same session may unintentionally attach escape files | The flag is **per-session** (per TUI process), not per-task. Documentation in §8.7 of 30a states this clearly. A follow-up PRD could add a "task boundary" reset if user feedback shows confusion. The flag also resets to `false` on TUI restart. |
| Provider receives an `Image` block it doesn't support | Medium | Low — image is silently dropped | F5's `supports_vision()` capability check; downgrade to `FileRef` (text content with a path-only stub). |
| `WorkspaceStore::read_bytes` is missing in a custom store | Low | High — resolver panics on any image mention | Default implementation in `WorkspaceStore` trait that calls `read()` and re-encodes. Custom stores can override but don't have to. |
| `ContentBlock::FileRef` deserialises incorrectly from old session stores | Low | Low — corruption in resume | The new variant is a `#[serde(tag = "type")]` addition; old stores have no `file_ref` entries. The deserialiser's `#[serde(untagged)]` on `MessageContent` ensures unknown block types are ignored (existing behaviour). |
| F22 `AtMentionCompleter` is not implemented | High | Low — feature works without tab completion | §8.4 contract is documented; F22 is a separate PRD. Users can type `@src/lib.rs` directly. |
| Confirmation dialog is dismissed via a TUI key remapping or a paste that contains `c\r` | Low | Medium — user accidentally cancels a submission they meant to approve | The dialog uses raw key events (`KeyEventKind::Press`), not crossterm's translated key strings. The `c` binding is a hard-coded `KeyCode::Char('c')` check, not a parseable key name. Tested manually in `crates/xaft-tui/tests/escape_confirmation.rs` by sending `c\r` as a paste and verifying the dialog is **not** dismissed (the `\r` is ignored, the `c` is interpreted as a character but the paste wrapper around it is rejected). |
| Audit log loses escape mention approval records on disk corruption | Low | High — no record of what was disclosed | Audit log writes (F39) are append-only and CRC32-checksummed. Corruption is detected at replay time; a single corrupted entry is logged as `⚠ audit entry N is corrupt` and replay continues. The same protection applies to `XaftEscapeMentionApproved` / `XaftEscapeMentionDenied` events. |
| Performance regression on the TUI submit path | Low | Low — slow submit delays the user's next action | The resolver is bounded by the byte cap and the line cap; reading 50 KB is fast (< 10 ms). Parallel reads via `tokio::join!` keep multi-file messages snappy. |
| AGENTS.md violation (structured-output state mutation) | Nil | — | The mention resolver produces a wire payload (`Vec<ContentBlock>`) consumed by the existing message-construction path. No new state-mutation paths. Reviewed in 30a §14. |
| Breaking change in `RunRequest` causes existing call sites to fail | Low | Medium | `Default` impl on `RunRequest`; documented in 14.1. Migration is a one-line change. |

---

## 16. Phasing

### Phase 1 — Parser + Resolver (TUI module) (1 PR)
- New `crates/xaft-tui/src/mention.rs` with `find_mentions` + `MentionResolver` + 30 unit tests.
- `WorkspaceStore::read_bytes` upstream change.
- No state.rs / runtime / provider changes yet. The parser is a pure function and the resolver is a struct that can be unit-tested against a mock workspace.
- Reviewable in isolation.

### Phase 2 — Submit-time integration (TUI submit handler) (1 PR)
- TUI state.rs calls the resolver on `InputAction::Submit`.
- TUI commits the literal user line + warning lines + "N files attached" hint.
- TUI sends `UserMessage` over a new typed channel.
- New `crates/xaft-tui/src/user_message.rs`.
- Tests: §12.2 (TUI integration).

### Phase 3 — Runtime boundary (`RunRequest::user_message`) (1 PR)
- Add `UserMessage` to `xaft-runtime` (or re-export from `xaft-tui`).
- Add `RunRequest::user_message: Option<UserMessage>` with `Default`.
- `XaftRuntime::run` handles both `task: String` and `user_message: UserMessage`.
- `HandoffRunParams::user_parts` upstream change in `agtrs-runtime`.
- Tests: §12.3 (orchestration tests).

### Phase 4 — Provider mapping (1 PR)
- `agtrs-anthropic`: render `FileRef` as `Text` with `<file path="…" lines="…" sha="…">` header.
- `agtrs-openai`: same.
- `Image` mentions flow through the existing F5 image mapping.
- Tests: provider-specific snapshots in `agtrs-anthropic` / `agtrs-openai` test suites.

### Phase 5 — Signals, audit, and config (1 PR)
- 3 new signals in `xaft-agent::signals`.
- `MentionConfig` in `xaft-config` with defaults.
- `/cost` integration (F25).
- `/doctor` check.
- F39 audit log entries.
- Tests: §12.5 (config), §12.6 (signals).

Each phase is independently mergeable and shippable. Phases 1–3 are
silent (no visible effect on the TUI; the resolver runs but its output
is not yet consumed). Phase 4 makes the structured message reach the
LLM. Phase 5 polishes observability.

---

## 17. Manual QA matrix

The mention feature is a TUI input primitive; it does not depend on
terminal capabilities. The QA matrix focuses on input modalities and
workspace configurations:

| Workspace | Path semantics | Notes |
|---|---|---|
| In-memory (sandbox, tests) | No filesystem | Mention resolver uses `InMemoryWorkspaceStore`; `read_bytes` returns the stored `Vec<u8>`. |
| Live filesystem (regular use) | Workspace-relative | Mention resolver uses `FsWorkspaceStore`; symlink canonicalisation active. |
| Git worktree | Workspace-relative to worktree root | Resolver reads the worktree's tracked files. |
| Read-only workspace (e.g. mounted `.iso`) | Workspace-relative, write-blocked | Mention reads work; writes (from F22 etc.) are blocked by the FS sandbox. |
| Network filesystem (NFS, SMB) | Workspace-relative, may be slow | Mention reads can be slow; the TUI shows a "Resolving mentions…" indicator if a resolve takes > 200 ms (out of scope for v1 — follow-up). |

| Input style | Is escape? | Behaviour |
|---|---|---|
| Type `@<path>` directly (workspace-relative) | No | Resolved on submit, no dialog. |
| Type `@./local.txt` (explicit relative) | No | Resolved, no dialog. |
| Type `@../sibling/file.rs` (parent traversal) | **Yes** | Dialog appears with `ParentTraversal` reason, absolute path, byte size. |
| Type `@/etc/hosts` (absolute) | **Yes** | Dialog appears with `Absolute` reason, `/etc/hosts` as absolute path. |
| Type `@~/notes/todo.md` (home expansion) | **Yes** | Dialog appears with `HomeExpansion` reason, `$HOME/notes/todo.md` as absolute path. |
| Type `@~root/.bashrc` (other-user home) | **Yes** | Dialog appears with `HomeExpansion` reason; `~root` is left as a literal POSIX-lookup path (the resolver does not call `getpwnam` in v1; the path is canonicalised by the FS only if it exists). |
| Type `@safe` where `safe` is a symlink to `/etc/hosts` | **Yes** (canonicalised) | Dialog appears with `Absolute` reason and `absolute_path = "/etc/hosts"`. |
| Paste a mention token | (depends on path) | Same as typing (parser is byte-oriented). |
| `@<tab>` | (depends on path) | F22 file picker (when implemented). |
| `\@not-a-mention` | No | Treated as literal `@not-a-mention`. |
| `@a.rs` (single component, file exists) | No | Resolved. |
| `@a.rs` (single component, file missing) | No | Warning, literal preserved. |
| `@a.rs` (single component, file missing, `--ignore-errors`) | No | Treated as `NotAMention` (no warning). (Future flag — not v1.) |
| `@` (trailing) | No | Not a mention. |
| ` ` after `@` | No | Not a mention. |
| Submit with mixed escape + workspace-relative | mixed | Dialog appears; workspace-relative mentions are attached unconditionally, escape mentions are listed in the dialog. |
| Press `a` in dialog | n/a | Approve once. Message is sent. |
| Press `A` in dialog | n/a | Approve and set `escape_approved_for_session = true`. |
| Press `c` in dialog | n/a | Cancel. Input bar text preserved. |
| Press `Esc` in dialog | n/a | Same as `c`. |
| Press `Enter` in dialog | n/a | Same as `a`. |
| Press `?` in dialog | n/a | Help overlay. |
| Submit a second message after `A` | n/a | No dialog (session flag). |
| Quit and restart TUI | n/a | Session flag is reset. |

| Dialog focus | Behaviour |
|---|---|
| Modal, centred in terminal | Verified by `EscapeConfirmDialog::layout(area)` returning a centred Rect. |
| Visible when terminal is 80×24 | Yes — minimum terminal size for the dialog. |
| Visible when terminal is 200×60 | Yes — centred with whitespace border. |
| Renders correctly with theme `default_dark` | Yes — uses theme `dim` for the border and `accent` for the heading. |
| Renders correctly with theme `solarized` | Yes — same theme keys. |

| Provider | Image behaviour |
|---|---|
| Anthropic (vision) | Image embedded as `ContentBlock::Image` → provider image content block. |
| OpenAI (vision) | Same. |
| Anthropic (no vision configured) | Image downgraded to `FileRef` with text "image omitted — provider does not support vision". |
| Local / no provider | Same as Anthropic (no vision). |

| Config | Behaviour |
|---|---|
| Default config (no `[mention]`) | All escape mentions show dialog. |
| `[mention] escape_policy = "always"` | No dialog for any escape mention (sends all). |
| `[mention] escape_policy = "never"` | No dialog, all escape mentions are rejected with `⚠ Workspace escape disabled` warning. |
| `[mention] escape_policy = "confirm"` (explicit) | Same as default. |
| `[mention] escape_allowlist = ["~/notes/*.md"]` + `@~/notes/todo.md` | No dialog. |
| `[mention] escape_allowlist = ["~/notes/*.md"]` + `@/etc/hosts` | Dialog appears (path does not match allowlist). |
| `[mention] escape_allowlist = ["["]` (invalid glob) | `ConfigError` at load time. |
| Existing user (v0.1 `xaft.toml`, no `[mention]`) loads the v0.2 binary | `[CHANGELOG]` line is added to the migration backup; `escape_policy = "confirm"` is the effective default. |

| Provider | Image behaviour |
|---|---|
| Anthropic (vision) | Image embedded as `ContentBlock::Image` → provider image content block. |
| OpenAI (vision) | Same. |
| Anthropic (no vision configured) | Image downgraded to `FileRef` with text "image omitted — provider does not support vision". |
| Local / no provider | Same as Anthropic (no vision). |

---

## 18. Open questions

(See 30a §16 for the design-side open questions. Implementation-side
additions:)

1. **Synchronous vs async file reads.** The TUI submit handler is
   currently synchronous (returns `InputAction::Submit(String)` from a
   sync method). The mention resolver needs `await workspace.read()`.
   Options:
   - (a) Make `handle_key` async — propagates async to the event loop
     which is already async (tokio task). Preferred.
   - (b) Use `tokio::task::block_in_place` from the sync handler. Ugly
     and breaks cancellation.
   - (c) Defer resolution: store the raw text in the input bar, return
     `InputAction::Submit(String)`, and have the app loop call the
     resolver before sending. Cleanest separation.
   - **Proposal:** option (c). The `InputAction::Submit` payload becomes
     `InputAction::Submit { raw: String }`; the app loop calls the
     resolver in an async context before sending over the channel.
2. **TUI ephemeral region for "Resolving…".** If a message has 10
   mentions, the resolve takes ~10 ms. Not worth a spinner. If a single
   mention is a 50 MB file, the resolve takes ~200 ms. Worth a
   spinner. **Proposal:** show a transient `⏳ Resolving 1 file…` line
   in the ephemeral region if the resolve takes > 100 ms; commit it as
   a `System` line on completion.
3. **Mention cache.** If the user submits the same `@<path>` five
   times in a session, do we re-read the file each time? **Proposal:**
   yes for v1 (cache adds complexity for a feature that's already
   capped at 50 KB per read). A follow-up adds an LRU cache keyed on
   `(path, mtime, sha256)`.
4. **Sequence number in `FileRef` blocks.** The block has `sha256` but
   not the original submission timestamp. **Proposal:** add a
   `submitted_at: SystemTime` field to `FileRef` so resume can show
   "attached 5 minutes ago" tooltips. Pure additive change.
5. **Renderer cost for "N files attached" hint.** The hint is
   committed as a `StyledLine` after the user message. The renderer's
   incremental append model handles this. No new code.

---

## 19. Out-of-scope follow-ups

(All carry-overs from 30a §15; tracked here for implementation-planning
visibility.)

- **F45: Glob expansion** (`@src/**/*.rs` → multiple `FileRef` blocks).
  Requires `globset` integration. Tracked as a separate PRD.
- **F46: Image preview in TUI** (small ASCII-art thumbnail in the
  ephemeral region). Requires `image` crate's `imageproc` or a custom
  half-block renderer.
- **F47: Drag-and-drop file mentions** (terminal SGR mouse drag events).
  Most terminals don't emit useful drop events; low-priority.
- **F48: PDF / Office `Document` block** for `@path/to/file.pdf` mentions.
  Tied to F5's `Document` block implementation upstream.
- **F49: Mention cache** (LRU on `(path, mtime, sha256)`).
- **F50: Staleness auto-refresh** (re-read a file at typing time if
  changed since mention started).

---

## 20. Cross-reference

| Section | 30a | 30b (this) |
|---|---|---|
| Problem, goals, non-goals | §1, §2, §3 | — |
| Syntax | §4 | — |
| Path resolution | §5 | — |
| Content blocks | §6 | — |
| File reading | §7 | §11.1, §11.8 |
| TUI integration | §8 | §11.3, §11.6 |
| Runtime integration | §9 | §11.4, §11.5 |
| Signals | §10 | §11.6 |
| Cross-cutting | §13 | — |
| AGENTS.md | §14 | §15 (Risk row 10) |
| Open questions | §16 | §18 |
| Out-of-scope | §15 | §19 |
| Files & line-level changes | — | §11 |
| Tests | — | §12 |
| Acceptance criteria | — | §13 |
| Migration & backward compat | — | §14 |
| Risk analysis | — | §15 |
| Phasing | — | §16 |
| Manual QA matrix | — | §17 |

---

## 21. Summary

`@-mention` is a small surface area with outsized UX impact. The full
implementation is ~3 200 LOC across 9 crates, with no new dependencies
(plus a hand-rolled glob-to-regex shim for `escape_allowlist` validation
in `xaft-config` if `globset` is not already available), four to five
review-sized PRs (depending on how aggressively we split Phases 4 and
5), and a 40+ test suite that exercises the parser, the resolver, the
TUI integration, the runtime boundary, the provider mapping, and the
**escape confirmation flow** (dialog appearance, per-submission and
session-wide approval, cancellation and input restoration, allowlist
matching, `escape_policy = "never"` rejection).

The bulk of the new code is in the escape confirmation flow: the new
`EscapeConfirmDialog` widget, the new `AppState` fields for the
pending/approved state, the new `XaftEscapeMentionApproved` /
`XaftEscapeMentionDenied` signals, and the new `escape_policy` /
`escape_allowlist` config keys. Without the escape confirmation flow,
the F3 implementation would be ~2 200 LOC; the escape flow adds the
remaining ~1 000 LOC.

Backward compatibility is preserved via the `Option<UserMessage>` and
`Option<Vec<ContentBlock>>` fields being optional; existing callers
using `task: String` continue to work unchanged. The new
`ContentBlock::FileRef` variant is additive and downstream `match`
blocks gain a single new arm. The new `escape_policy = "confirm"`
default is **strictly more conservative** than v0.1's "escape = reject"
default — a user who relied on the rejection sees a confirmation
dialog, which is one extra keypress away from the same outcome.

AGENTS.md compliance is preserved: the resolver produces wire payloads
consumed by the existing message-construction path; the
`EscapeConfirmDialog` widget is local UI state that emits signals
(`XaftEscapeMentionApproved` / `XaftEscapeMentionDenied`) rather than
mutating workflow state directly. No new state-mutation paths are
introduced.

**Ship plan:** phases 1–3 are silent (resolver runs but its output is
not yet consumed) → Phase 4 makes the structured message reach the LLM
→ Phase 5 polishes observability, config, and the escape confirmation
dialog (the dialog is a Phase 5 deliverable, not a Phase 1 one — the
resolver supports escape paths from Phase 1, but the dialog is not
shown until Phase 5 to keep Phases 1–4 shippable in isolation). Each
phase is independently mergeable and shippable.

After this PRD lands, `@<path>` joins the existing `/` (slash commands),
`!` (bash mode), and `#` (memory tags) as the fourth xaft input
primitive. With escape confirmation enabled by default, F3 also
becomes a **safety** primitive: the dialog is the last line of defence
against accidental disclosure of sensitive files via `~/`, `..`, or
absolute paths.
