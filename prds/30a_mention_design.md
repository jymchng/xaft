# PRD 30a — @-Mention File Inputs (Design)

**Status:** Proposal
**Target:** next minor release (M1 of the parity roadmap)
**Companion PRDs:**
- `27_claude_code_parity_features.md` §F3 (originating requirement)
- `28_claude_code_parity_features_r2.md` §F22 (Tab completion), §F5 (multimodal paste)
- `29a_multi_line_input_design.md` (input parser, F21)

---

## 1. Problem statement

When a user wants the agent to act on a specific file, they currently have two
choices, both bad:

1. **Paste the entire file content into the input bar.** Bloats the prompt,
   duplicates content the agent could read on its own, and breaks for files
   larger than the visible terminal scrollback.
2. **Describe the file in prose** (`the main file`, `the readme`) and hope the
   agent finds it via `list_files` + `read_file`. Slow (extra LLM round-trip),
   ambiguous, and the agent often picks the wrong file.

Claude Code, Cursor, aider, and Continue all solve this with a single
inline-token convention: **`@<path>`**. The user types

```text
Fix the bug in @src/lib.rs and update @README.md
```

and the runtime expands each `@<path>` into the file's actual content before
the message reaches the LLM. This makes file references a first-class part
of the prompt language — short in the transcript, complete on the wire.

xaft needs the same primitive.

---

## 2. Goals

1. **Inline syntax.** `@<path>` tokens in the input bar expand into file
   content (text) or multimodal blocks (image) before the message reaches
   the agent loop.
2. **Security.** Paths are resolved relative to `working_dir`. Any `..`
   component that escapes the workspace root is **rejected**, mirroring the
   existing `read_file` tool policy.
3. **Bounded payload.** Files larger than `max_inline_lines` (default 2 000)
   are truncated with a `… (truncated, N more lines)` note. Files larger
   than `max_inline_bytes` (default 50 000) trigger a warning and are
   embedded only up to the byte cap.
4. **Multimodal.** Image files (`.png`, `.jpg`/`.jpeg`, `.gif`, `.webp`)
   are read as bytes and embedded as base64 in `ContentBlock::Image` blocks
   (see F5). The provider's `supports_vision()` capability gate decides
   whether the block is actually sent or downgraded to a path reference.
5. **Graceful fallback.** If a path is missing, the `@<path>` token is
   **left as literal text** in the user message so the LLM can still see
   what the user asked for, and a warning line (`⚠ File not found: …`) is
   committed to the TUI transcript.
6. **Tab completion.** `@<tab>` (powered by F22's `AtMentionCompleter`)
   opens a file picker scoped to the workspace, respecting `.gitignore`.
7. **Traceability.** The original `@<path>` token is preserved in the user
   message text in addition to the resolved content blocks, so the
   transcript and the agent's view both show the user typed `@src/lib.rs`,
   not `<file contents>`.
8. **Zero new dependencies.** All work is done with crates already in
   `Cargo.toml`: `regex`, `serde`, `tokio`, `base64`, `bytes`, `image`
   (already pulled in transitively).

---

## 3. Non-goals

1. **No new content-block types in `agtrs-runtime` core schema for v1.**
   We will extend `ContentBlock` upstream with a single `FileRef` variant
   (see §6), but we do **not** add a general "file upload" pipeline, remote
   URLs, S3-backed storage, or signed-URL generation.
2. **No fuzzy / partial matching on the path itself.** `@sr` will not match
   `src/main.rs` automatically. The user must type at least the directory
   boundary (`@src/`) or a complete prefix; Tab completes the rest. (This is
   a deliberate match to Claude Code and a hedge against accidental
   matches in normal text like `user@example.com` — see §4.3.)
3. **No binary-text files** (PDFs, Office documents, archives) are decoded
   inline in v1. PDF support is planned as part of F5 (image / PDF paste)
   and will reuse the same `Document` block; PDFs encountered via `@path.pdf`
   in v1 are surfaced as a warning + path-only reference.
4. **No watch-mode auto-attachment** (PRD 28 §F34) of changed files. The
   `FileChanged` signal is informational only.
5. **No remote file references** (`@https://…`, `@gist:abc123`, etc.).

---

## 4. Syntax

### 4.1 Token grammar

A **mention token** is the longest `@<path>` substring in the input where
`<path>` matches:

```text
@(~\w*/?|/{1,2}|\.{0,2}/)?[A-Za-z0-9_./-]+(?:[A-Za-z0-9_-])
```

The leading prefix (the first capture group) is one of:

| Prefix | Meaning | Example token |
|---|---|---|
| *(empty)* | Workspace-relative | `@src/lib.rs` |
| `./` | Explicit workspace-relative | `@./local.txt` |
| `../` (one or more) | Parent-directory relative | `@../sibling/foo.rs` |
| `/` | Absolute path (filesystem root) | `@/etc/hosts` |
| `~/` | Current user's home directory | `@~/notes.md` |
| `~user/` | Another user's home (POSIX `~user` expansion) | `@~root/.bashrc` |
| `~` (alone, no slash) | Treated as a token start with empty body — rejected by the parser | `@~` → not a mention |

The path body is `[A-Za-z0-9_./-]+` (one or more characters) and the
final character must be a non-separator (`[A-Za-z0-9_-]`) so that
trailing `.` and `/` are not consumed.

In plain English:

| Property | Rule |
|---|---|
| Trigger character | `@` (U+0040) |
| Required after `@` | At least one character; the path must end in a `A-Za-z0-9_-` (cannot end in `.` or `/`) |
| Allowed characters inside path | `A-Za-z0-9_./-` plus the leading `./`, `../`, `/`, `~/`, or `~user/` prefix |
| Forbidden | whitespace, `*`, `?`, `[`, `]`, `{`, `}`, `\|`, `<`, `>`, `:`, `;`, `,`, quotes, parens, NUL, control characters |
| Anchoring | Token ends at the first forbidden character OR a whitespace boundary OR a `,` / `;` / `:` / `)` / `]` / `>` that is **not** part of a path component |
| Escaping | The user can write `\@` to insert a literal `@` that should not be parsed as a mention. The parser treats `\@` as a literal `@` and continues scanning after it. |
| Workspace escape | A token whose resolved path is **outside** `working_dir` is still a valid mention; it triggers a per-message confirmation dialog in the TUI (see §5.2 and §8.7). |

### 4.2 Examples

| Input | Tokens recognised |
|---|---|
| `Fix the bug in @src/lib.rs and update @README.md` | `@src/lib.rs`, `@README.md` (workspace-relative, no confirmation) |
| `@./local.txt` | `@./local.txt` (workspace-relative) |
| `@../sibling-project/file.rs` | `@../sibling-project/file.rs` — escape; confirmation required |
| `@/etc/hosts` | `@/etc/hosts` — escape (absolute); confirmation required |
| `@~/notes.md` | `@~/notes.md` — escape (home); confirmation required |
| `@~root/.bashrc` | `@~root/.bashrc` — escape (other-user home); confirmation required |
| `Contact me at user@example.com` | none (the `.com` is not a valid path character class and the token's path-body would have to be `example` which has no boundary; we do **not** treat this as a mention) |
| `Use the @ symbol for mentions` | none (no path characters after `@`) |
| `Read @docs/adr-0007.md.` | `@docs/adr-0007.md` (trailing `.` is not part of the path) |
| `Compare @a.rs to @b.rs` | `@a.rs`, `@b.rs` |
| `Check @src/main.rs:42` | `@src/main.rs` (the `:` terminates the path) |
| `Visit https://x.com/@user` | none — the `@` is preceded by `/`, not a token-start boundary |
| `\@not a mention` | none (the `\` cancels the `@`) |

### 4.3 Anti-false-positive rules

Three rules prevent mention detection from corrupting normal prose:

1. **Email addresses.** `user@example.com` is not a mention because the
   `example` body has no path separator and the literal `.com` is not in
   the allowed character class. Concretely, the parser only emits a token
   when the path body contains at least one of `./` or `/` OR the body is
   a single filename **that exists in the workspace** (cheap probe — see
   §5.4).
2. **Handles in social text.** `Thanks @alice!` is not a mention. `@alice`
   has no path separator and the file `alice` does not exist in the
   workspace, so the "exists-in-workspace" probe rejects it. The literal
   `@alice` is preserved.
3. **URLs.** `Visit https://x.com/@user` — the `@` is preceded by `/`, not
   a word boundary, so it is not a valid token start. The token-start
   anchor is "the `@` is at the start of the input OR preceded by
   whitespace OR preceded by a non-path character" (whitespace, `,`, `;`,
   `(`, `[`, `{`, `<`, `:`, end-of-line, beginning-of-line).

### 4.4 Token boundaries in display

In the TUI input bar, an active mention token (one whose path resolves
successfully to a file) is rendered with a subtle underline and dim color
(theme `mention_resolved`). An unknown token (file missing) is rendered
with a strike-through and warning color (theme `mention_unknown`).
Inactive `@` literals (preceded by `\`) are rendered as a plain `@` with
no styling.

The styling is purely visual; the on-wire text remains the raw `@<path>`
string the user typed (plus the resolved content blocks; see §6).

---

## 5. Path resolution

### 5.1 All path types accepted

`@-mention` accepts four path flavours, all of which the parser and
resolver handle:

| Form | Example | Resolves to |
|---|---|---|
| Workspace-relative | `@src/lib.rs` | `working_dir/src/lib.rs` |
| Explicit relative | `@./local.txt` | `working_dir/local.txt` |
| Parent-directory relative | `@../sibling/foo.rs` | `working_dir/../sibling/foo.rs` (escape) |
| Absolute | `@/etc/hosts` | `/etc/hosts` (escape) |
| Home expansion | `@~/notes.md` | `$HOME/notes.md` (escape) |
| Other-user home | `@~root/.bashrc` | `~root/.bashrc` (escape, POSIX `~user`) |

**Workspace-relative** paths are resolved silently. **Escape** paths
(parent-traversal, absolute, or home) are resolved but **flagged** so
the TUI can require an explicit per-submission confirmation (§8.7).
The user always sees the literal `@<path>` they typed in the transcript;
the resolved absolute path is only revealed in the confirmation dialog
and the audit log.

`working_dir` is `RunRequest::working_dir` (defaults to
`std::env::current_dir()` in the CLI). The resolver produces both
`path: String` (the original token body) and `absolute_path: PathBuf`
(the resolved absolute path on disk) for downstream use.

### 5.2 Escape classification

A mention is classified as an **escape** if any of:

1. **Absolute path.** The path starts with `/` after the leading `@`.
2. **Parent traversal.** The path contains `..` components that resolve
   outside the workspace root (`working_dir`).
3. **Home expansion.** The path starts with `~` (which POSIX `glob` and
   `wordexp` expand to `$HOME`) or `~user` (which expands to the
   named user's home directory, looked up via `getpwnam`).

`@./` and bare relative paths are **not** escapes even if they are
logically outside the workspace via some external mount (e.g. on
chimaera-root systems where `/workspace/../etc` is a symlink to `/etc`).
Such cases are caught by §5.3's symlink check.

### 5.3 Symlink resolution

If the workspace uses the real filesystem (`FsWorkspaceStore`), symlinks
are resolved via `std::fs::canonicalize` **after** the join, regardless
of whether the path is an escape. The canonicalised path is recorded
as `absolute_path` on the resolved mention. This is essential even for
workspace-relative paths to prevent the classic
`working_dir/safe → /etc/passwd` attack.

If the canonicalised path is a descendant of the canonicalised
`working_dir`, the mention is **not** an escape by symlink. Otherwise it
is. (The classification is based on the **real** path on disk, not the
lexical path the user typed.)

For `InMemoryWorkspaceStore` (tests, sandbox) symlink semantics don't
apply; we skip canonicalisation.

### 5.4 Existence probe

For **single-component filenames** (no `/` in the path body), the parser
performs a cheap existence check via `WorkspaceStore::exists` before
treating the token as a mention. This is what prevents `Thanks @alice!`
from being mistakenly expanded: the file `alice` does not exist in the
workspace, so the token is left as literal text.

For **multi-component paths** (`src/lib.rs`, `docs/adr-0007.md`), we skip
the existence probe at parse time and let the resolver handle it. This
keeps the parser fast (one workspace query per token, not per typed
character) and avoids spamming the user with "file not found" warnings
while they are still typing the path. The resolver at submit time
produces the structured `FileRef` or the literal-with-warning result.

For **escape paths** (`@/etc/hosts`, `@~/notes.md`, `@../sibling/x`),
the existence probe is **skipped at parse time** because:

- An absolute path probe would require a `stat()` on every typed
  character, which is wasteful.
- A missing escape path is still a valid `NotAMention` candidate — the
  resolver at submit time will produce the `Unresolved` result with a
  `FileNotFound` reason and the user gets a `⚠ File not found` warning.

### 5.5 Resolver contract

```rust
pub struct MentionResolver<'a> {
    workspace: Arc<dyn WorkspaceStore>,
    working_dir: &'a Path,
    home_dir: Option<&'a Path>,  // $HOME, optional
    config: &'a MentionConfig,
}

pub enum MentionResolution {
    /// Token resolved to a file. The path may be workspace-relative or
    /// an absolute path on disk (for escape mentions).
    ResolvedFile {
        raw: String,                // original "@src/lib.rs" as typed
        path: String,               // the path body as typed (preserves ./, ../, /, ~/)
        absolute_path: PathBuf,     // canonicalised absolute path on disk
        kind: FileKind,             // Text | Image
        content: FileContent,       // Text(String) | Image { media_type, bytes_base64 }
        truncated: Option<TruncationInfo>,
        /// `Some` if the resolved path is outside the workspace root.
        /// Drives the TUI confirmation flow (§8.7).
        escape: Option<EscapeInfo>,
    },
    /// Token preserved as literal text. A warning will be committed to the TUI.
    Unresolved {
        raw: String,                // original "@missing.rs"
        path: String,               // what the user typed
        absolute_path: Option<PathBuf>,  // if the resolver reached canonicalise
        reason: MentionError,       // why it failed
    },
    /// The token looked like a mention but did not match the grammar.
    NotAMention,
}

/// Why a mention was classified as a workspace escape.
#[derive(Debug, Clone)]
pub struct EscapeInfo {
    /// Classification: absolute path, parent traversal, or home expansion.
    pub reason: EscapeReason,
    /// Canonicalised absolute path on disk (always populated).
    pub absolute_path: PathBuf,
    /// Number of `..` steps above `working_dir` (for parent traversal
    /// cases; `0` for absolute and home).
    pub depth: usize,
    /// File size in bytes (read at resolve time so the confirmation
    /// dialog can show "this will attach 12.3 KB").
    pub byte_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscapeReason {
    /// The path starts with `/`.
    Absolute,
    /// The path contains `..` components that resolve outside `working_dir`.
    ParentTraversal,
    /// The path starts with `~` or `~user/`.
    HomeExpansion,
}
```

---

## 6. Content blocks and message shape

### 6.1 New `ContentBlock::FileRef` variant

The PRD 27 example
`Message::user_with_parts(vec![Text("..."), FileRef{ ... }, ...])` requires a
`FileRef` variant on `agtrs_runtime::transport::ContentBlock`. The current
enum has `Text`, `ToolUse`, `ToolResult`, `Image`; we add:

```rust
#[serde(rename = "file_ref")]
FileRef {
    /// Workspace-relative path as the user typed it (preserved verbatim,
    /// including case) so the LLM can see what was referenced.
    path: String,
    /// Resolved content. For text files this is the inlined body (possibly
    /// truncated). For images it is the media type + base64 bytes.
    content: FileRefContent,
    /// `Some` if the file was truncated for inline embedding.
    truncation: Option<Truncation>,
    /// Total file size in bytes (before truncation).
    byte_size: u64,
    /// Total line count (before truncation; `0` for binary files).
    line_count: u64,
    /// SHA-256 of the original file contents — for cache-invalidation and
    /// for de-duplication if the same file is mentioned twice in one message.
    sha256: String,
},
```

`FileRefContent` is an enum:

```rust
pub enum FileRefContent {
    /// UTF-8 text content (the normal case).
    Text(String),
    /// Base64-encoded binary, with media type for the provider to route.
    Image {
        media_type: String,   // "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        data_base64: String,
    },
}
```

`Truncation` is a struct:

```rust
pub struct Truncation {
    /// First N lines kept.
    pub shown_lines: usize,
    /// Total lines in the original file.
    pub total_lines: usize,
    /// Bytes kept (for the byte-cap case).
    pub shown_bytes: usize,
    /// Total bytes in the original file.
    pub total_bytes: u64,
}
```

`ContentBlock::FileRef` is constructed in `xaft-tui::mention::MentionResolver`
and consumed by the provider layer in `xaft-agent` / `agtrs-anthropic` /
`agtrs-openai` (see §10 for the provider mapping).

### 6.2 Why a new variant instead of `Text`

Two reasons:

1. **LLM grounding.** A `FileRef` block can carry explicit metadata
   (path, line count, truncation note, sha256). The provider can format
   it as a fenced code block with a header
   `<file path="src/lib.rs" lines="120" sha="abc…">` instead of bare
   text. The LLM knows it is reading an attached file, not user prose.
2. **Round-tripping.** When a session is resumed from `SessionStore`, the
   `FileRef` block is stored verbatim and re-attached without re-reading
   the disk. The `sha256` lets us detect if the file has changed since
   the message was sent and surface a "⚠ src/lib.rs modified since
   attached" warning.

### 6.3 Image blocks

Image mentions use the existing `ContentBlock::Image` variant:

```rust
Image {
    url: String::new(),                  // unused for inline data
    media_type: "image/png".to_string(),
    data: Some(base64::encode(bytes)),   // base64-encoded file bytes
}
```

The provider layer (F5 already partially designs this) checks
`provider.capabilities().supports_vision()`. If the provider is text-only
(no vision), the image block is **downgraded** to a `FileRef` with a
`FileRefContent::Image` (or to a `Text` block with a path-only stub) and
the user sees a `⚠ Provider does not support images — saved reference:
@path/to/img.png` line in the TUI.

### 6.4 Message shape

After resolution, a single user submission produces a structured
`Message` with `MessageContent::MultiPart`:

```rust
fn expand_message(raw: &str, resolver: &MentionResolver) -> Message {
    let mut blocks: Vec<ContentBlock> = Vec::new();
    let mut cursor = 0usize;
    for token in parser::find_mentions(raw) {
        // Push the literal text between the previous token and this one.
        if token.start > cursor {
            blocks.push(ContentBlock::Text {
                text: raw[cursor..token.start].to_string(),
            });
        }
        match resolver.resolve(&token) {
            MentionResolution::ResolvedFile { content, path, .. } => {
                blocks.push(ContentBlock::FileRef { path, content, .. });
            }
            MentionResolution::Unresolved { raw, .. } => {
                // Keep the original literal so the LLM sees what the user asked.
                blocks.push(ContentBlock::Text { text: raw });
                resolver.warnings.borrow_mut().push(...);
            }
            MentionResolution::NotAMention => unreachable!(),
        }
        cursor = token.end;
    }
    if cursor < raw.len() {
        blocks.push(ContentBlock::Text {
            text: raw[cursor..].to_string(),
        });
    }
    Message { role: Role::User, content: MessageContent::MultiPart(blocks), .. }
}
```

If the result is a single `Text` block (no mentions), we collapse to
`MessageContent::Text` to avoid unnecessary multi-part overhead.

### 6.5 Traceback transcript

The TUI commit line for the user message **does not include the resolved
content** — it shows only the literal `"❯ Fix the bug in @src/lib.rs and
update @README.md"`. The resolved `FileRef` blocks are sent to the
runtime but are not rendered in the transcript (otherwise the TUI would
re-render thousands of lines of source code on every keypress). A
disclosure affordance (`❯ … (3 files attached) ▾`) is planned for a
follow-up.

---

## 7. File reading

### 7.1 Text files

For a text file, the resolver:

1. Resolves the path (§5).
2. Reads via `WorkspaceStore::read(path)`. The store handles encoding
   (assumed UTF-8 with lossy fallback for non-UTF-8 bytes — `String::from_utf8_lossy`).
3. Splits on `\n`, counts lines, records `byte_size = content.len()`.
4. If `lines.len() > max_inline_lines` OR `byte_size > max_inline_bytes`,
   truncates (see §7.3).
5. Wraps the body in a fenced `FileRefContent::Text` with the appropriate
   `truncation` note.
6. Computes SHA-256 of the **original** (pre-truncation) bytes via
   `sha2` (already a transitive dep via `agtrs-runtime`).

### 7.2 Image files

For an image file, the resolver:

1. Resolves the path.
2. Probes the file's **magic bytes** rather than trusting the extension,
   so a renamed `.png` that is actually a JPEG is still embedded as
   `image/jpeg`:
   - `89 50 4E 47 0D 0A 1A 0A` → `image/png`
   - `FF D8 FF` → `image/jpeg`
   - `47 49 46 38 37 61` or `47 49 46 38 39 61` → `image/gif`
   - `52 49 46 46 ?? ?? ?? ?? 57 45 42 50` → `image/webp`
3. If the magic bytes don't match a known image type, falls back to the
   extension-based guess. If still ambiguous, the file is **not** treated
   as an image — it's either treated as a text file (small binary → lossy
   UTF-8 with a warning) or rejected entirely.
4. Reads raw bytes (no `WorkspaceStore::read` for binaries — we add
   `WorkspaceStore::read_bytes`; see §11.1).
5. Encodes with `base64::engine::general_purpose::STANDARD.encode(bytes)`.
6. Records `byte_size`, `line_count = 0` (binary).

### 7.3 Truncation strategy

Two independent caps, applied in this order:

| Cap | Default | Behaviour |
|---|---|---|
| `max_inline_lines` | 2 000 | If the file has more than N lines, keep the first N and append `// (truncated, X more lines)`. |
| `max_inline_bytes` | 50 000 | If the (post-line-truncation) body is more than N bytes, keep the first N bytes at a char boundary and append `// (truncated, Y more bytes)`. The line cap is usually hit first for source files. |

Both caps are configurable via `[mention]` in `xaft.toml`:

```toml
[mention]
max_inline_lines = 2_000
max_inline_bytes  = 50_000
image_max_bytes   = 10_000_000
```

If `image_max_bytes` is exceeded, the image is **not** embedded. The
warning line is `⚠ Image too large to attach (@path/to/img.png, 23.4 MB;
cap 10 MB). Saved reference only.` and the message contains a `Text`
block describing the path so the LLM can find the file itself.

### 7.4 Encoding

- **UTF-8 with lossy fallback.** If the file contains invalid UTF-8
  sequences, replace them with `U+FFFD` (`\u{FFFD}`) and record a
  `// (warning: N invalid UTF-8 sequences were replaced)` note. This is
  the same policy as `read_file`.
- **No BOM stripping.** A UTF-8 BOM at the start of a file is preserved
  in the inlined content. The LLM handles it.
- **No newline normalisation.** CRLF is preserved as-is. The LLM can
  handle either.

### 7.5 Empty files

A zero-byte file is embedded as an empty `FileRefContent::Text` with a
note `// (empty file)`. No truncation, no warning.

---

## 8. TUI integration

### 8.1 Parser location

The parser lives in `crates/xaft-tui/src/mention.rs` as a pure function:

```rust
/// Find all mention tokens in `input`. Returns a `Vec<MentionToken>` in
/// left-to-right order. The tokens are non-overlapping and their ranges
/// are byte indices into the input string.
pub fn find_mentions(input: &str) -> Vec<MentionToken>;

pub struct MentionToken {
    /// Byte offset of the leading `@`.
    pub start: usize,
    /// Byte offset one past the end of the path body (i.e. where the
    /// parser stopped — typically the start of the trailing separator).
    pub end: usize,
    /// The literal `@<path>` as it appears in the input, with the
    /// leading `@` and any `./` prefix preserved.
    pub raw: String,
    /// The path body (without the leading `@`).
    pub path: String,
    /// Whether the path contains a `/` (multi-component).
    pub is_multi_component: bool,
}
```

The parser does **no I/O** — it does not query the workspace, does not
read files. The resolver (in the same file, different struct) does the I/O.

### 8.2 Submit-time resolution

When the user presses `Enter` on a non-empty input bar:

1. The TUI calls `InputBar::submit()` (F21) which returns
   `InputAction::Submit(String)`.
2. The TUI calls `MentionResolver::expand(&msg, &workspace, &config) ->
   ExpandedMessage { parts: Vec<ContentBlock>, warnings: Vec<Warning> }`.
3. If `escape_policy == Confirm` (the default; see §5.1) and the
   expansion produced any `EscapeInfo` entries, the TUI shows the
   **escape confirmation dialog** (§8.7) instead of committing the
   message. The user must approve (or cancel) before the message is
   sent. The TUI does **not** commit the user message line to the
   transcript until approval — a cancelled message leaves the input
   bar's text intact for the user to edit and resubmit.
4. After approval (or if no escape mentions were present), the TUI:
   - commits the user message **literally** (`❯ <raw text>`) to the
     transcript (line kind `UserMessage`).
   - commits any warnings (`⚠ File not found: …`) to the transcript
     (line kind `System`).
   - sends the structured `Message` over a new typed channel
     `user_message_tx: UnboundedSender<UserMessage>` where
     ```rust
     pub enum UserMessage {
         Text(String),
         MultiPart(Vec<ContentBlock>),
     }
     ```
   - resets the input bar.

The TUI does **not** render the resolved content. The transcript is a
faithful record of what the user typed. The structured message is the
on-wire payload to the runtime.

### 8.3 Existing `user_message_tx` migration

`xaft-tui/src/state.rs:53` currently has:

```rust
pub user_message_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
```

We change it to:

```rust
pub user_message_tx: Option<tokio::sync::mpsc::UnboundedSender<UserMessage>>,
```

The `xaft-cli` and `xaft-tui/src/app.rs` startup paths (which currently
construct a `String` channel) are updated to construct a `UserMessage`
channel. The receiver in `xaft-runtime` (where `RunRequest::task` is
parsed) is updated to handle both variants.

This is a breaking change to the `xaft-tui` ↔ `xaft-runtime` wire format
but `xaft-tui` and `xaft-runtime` are sibling crates in the same
workspace; there are no external consumers. Documented in §14.

### 8.4 Tab completion hookup

The F22 `AtMentionCompleter` is the source of `@<tab>` file suggestions.
This PRD does not implement F22 — it specifies the **contract** the
mention feature depends on:

```rust
pub trait AtMentionCompleter: Send + Sync {
    /// Return up to `limit` file paths matching the partial prefix.
    /// `prefix` is everything after the `@`, e.g. `src/`.
    fn complete(&self, prefix: &str, limit: usize) -> Vec<String>;
}
```

The mention feature itself does not depend on F22 being implemented; the
`@<path>` syntax is fully functional via direct typing. Tab completion
is the UX accelerator and is tracked as a follow-up (see §13).

### 8.5 Warning line rendering

Warnings are committed to the transcript as
`StyledLine { text: "⚠ <message>", kind: LineKind::System }`. They
appear after the user message line, before the next agent response. The
warning is dim (theme `dim`) and prefixed with `⚠` so it is visually
distinct from the regular transcript.

### 8.6 TUI surface for "N files attached"

When a message contains at least one resolved mention, the TUI commits
the literal user message line and a secondary status line directly below
it:

```text
❯ Fix the bug in @src/lib.rs and update @README.md
   ⎿  2 files attached: src/lib.rs (211 lines), README.md (1 file attached)
```

Wait, that example was wrong. Let me re-state:

```text
❯ Fix the bug in @src/lib.rs and update @README.md
   ⎿  2 files attached: src/lib.rs (211 lines), README.md (32 lines)
```

This is a **single secondary line** that lists every attached file with
its line count. If any file was truncated, the line includes a `(truncated)`
suffix:

```text
   ⎿  2 files attached: src/lib.rs (211 lines), README.md (truncated: 2000 of 4232 lines)
```

This line is purely for the user; it is **not** part of the structured
message sent to the runtime. The runtime has the full metadata in the
`FileRef` blocks.

### 8.7 Escape confirmation dialog

When a submission contains one or more **escape** mentions (parent
traversal, absolute path, or home expansion — see §5.2), the TUI
**pauses** before sending the message and renders a confirmation popup.
The dialog is implemented as a new `EscapeConfirmDialog` widget in
`crates/xaft-tui/src/confirm.rs`.

The dialog has three layout regions, rendered in the centre of the
terminal, dimmed above and below:

```text
┌─ Attach files outside the workspace? ───────────────────────────┐
│                                                                 │
│  The following 2 file(s) are OUTSIDE the workspace:              │
│                                                                 │
│    @../sibling-project/file.rs                                    │
│      → /home/user/projects/sibling-project/file.rs                │
│      → 12.3 KB · 312 lines                                        │
│                                                                 │
│    @/etc/hosts                                                    │
│      → /etc/hosts                                                 │
│      → 412 B · 14 lines                                           │
│                                                                 │
│  These files will be sent to the LLM in your next message.       │
│                                                                 │
│  [a]pprove once   [A]pprove all escape mentions this session     │
│  [c]ancel and edit                                                 │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

Key bindings:

| Key | Action |
|---|---|
| `a` / `Enter` | Approve this submission only. The escape mentions are attached; future escape mentions still require confirmation. |
| `A` (shift) | Approve and **remember for the session**. Future escape mentions in the same session are attached without confirmation. The session flag resets when the user quits the TUI or starts a new session. |
| `c` / `Esc` | Cancel. The user message is **not** sent. The input bar's text is restored so the user can edit and re-submit. |
| `?` | Show a help overlay explaining the danger. |

**Why per-submission confirmation by default?** The user typed
`@/etc/hosts` (or `@../sensitive.txt`) on autopilot, possibly
auto-completed by an IDE or recalled from history. The confirmation is
the last line of defence against accidentally sending a sensitive file
(SSH private key, password manager database, `~/.aws/credentials`,
`/etc/shadow`) to the LLM. The cost of one extra keypress is negligible
compared to the cost of an accidental disclosure.

**What gets revealed in the dialog.** The dialog shows:

- The literal token the user typed (`@../sibling/file.rs`).
- The resolved absolute path on disk (`/home/user/projects/sibling/file.rs`).
- The file size in human-readable form (`12.3 KB`).
- The line count for text files; the dimensions and media type for images.

The **file content is not shown in the dialog** — the dialog is for
confirmation, not preview. A `p` (preview) key could be added as a
follow-up to render the first 20 lines in a side panel, but it's
explicitly out of scope for v1.

**What happens to a denied submission.** The TUI restores the input
bar's text and clears the pending `ExpandedMessage`. The user can
edit (e.g. remove the offending `@<path>` token) and re-submit. The
`XaftMentionsResolved` signal is **not** emitted for a denied
submission (only for approved ones). The denial is itself logged
in the audit log as `XaftEscapeMentionDenied { token, reason }`.

**`escape_policy` config.** The behaviour is configurable via
`[mention]` in `xaft.toml`:

```toml
[mention]
# "confirm" (default) — show dialog per escape mention
# "always"             — silently attach all escape mentions
# "never"              — reject escape mentions with a warning
escape_policy = "confirm"

# When escape_policy = "always", this allowlist restricts which
# absolute / home paths can be attached without confirmation.
# Patterns are matched against the canonicalised absolute path.
# Default: empty (no restriction).
escape_allowlist = [
    "~/.config/xaft/agents/*.md",
    "~/.local/share/xaft/sessions/**",
    "/usr/share/doc/**/*.txt",
]
```

When `escape_policy = "always"`, every escape mention in the
allowlist is attached silently and every escape mention **outside** the
allowlist is still shown the dialog (the allowlist is a **relaxation**,
not a replacement, for the default confirmation flow). When
`escape_policy = "never"`, every escape mention is rejected with a
`⚠ Workspace escape disabled (@/etc/hosts). Set [mention] escape_policy
= "confirm" or "always" to enable.` warning line and the token is left as
literal text in the user message. The original behaviour of
`escape_policy = "never"` matches the v0.1 PRD (escape = reject).

The default for **new users** is `confirm`. The default for users
with an existing `xaft.toml` is also `confirm` (explicit opt-in to
the more permissive modes). A `[CHANGELOG]` note flags the change
from v0.1's "escape = reject" default to v0.2's "escape = confirm"
default.

---

## 9. Runtime integration

### 9.1 The expansion point

The expansion happens at the **TUI submit boundary**, not inside the
runtime. Rationale:

- The TUI has the input bar's raw text (post-`@<path>` typing, pre-resolve).
- The TUI has access to the `WorkspaceStore` (or can acquire it via the
  app context) for the resolve operation.
- The runtime receives a structured `UserMessage` payload that is
  already-resolved; it does not need to know that `@` syntax exists.
- This keeps `xaft-runtime` and `xaft-cli` decoupled from the input
  format. A programmatic caller (e.g. `xaft --task "Refactor @foo.rs"`)
  would have to perform its own resolution before constructing the
  `RunRequest::task` string, or — better — use the new
  `RunRequest::user_message: UserMessage` field (see §9.2).

### 9.2 `RunRequest` extension

```rust
pub struct RunRequest {
    // ... existing fields ...
    /// Structured user message. When `Some`, this takes precedence over
    /// `task`. When `None`, `task` is used as plain text.
    pub user_message: Option<UserMessage>,
}
```

Where `UserMessage` is a new shared type in `xaft-runtime`:

```rust
#[derive(Debug, Clone)]
pub enum UserMessage {
    /// Plain text — identical semantics to the existing `task: String`.
    Text(String),
    /// Structured parts (text + file refs + images). Used by the TUI
    /// after `@`-mention resolution.
    MultiPart(Vec<agtrs_runtime::transport::ContentBlock>),
}
```

The `MultiPart` variant is converted to a `Message` in
`XaftRuntime::run` and prepended to the orchestrator's initial state. The
orchestrator's `HandoffRunParams::message: String` continues to carry the
plain-text representation (for logging, history, and the system prompt
seed), but the actual agent invocation now also receives the structured
parts.

### 9.3 Handoff orchestrator extension

`HandoffRunParams` gains:

```rust
pub struct HandoffRunParams {
    // ... existing fields ...
    /// Optional structured user message. When `Some`, the orchestrator
    /// constructs the agent's first `Message` from these parts instead
    /// of wrapping `message: String` in `MessageContent::Text`.
    pub user_parts: Option<Vec<ContentBlock>>,
}
```

The orchestrator (in `agtrs-runtime::team.rs`) uses `user_parts` to
build the first user `Message` in the agent's conversation history:

```rust
let first_user_message = match params.user_parts {
    Some(parts) if !parts.is_empty() => Message {
        role: Role::User,
        content: MessageContent::MultiPart(parts),
        ..Default::default()
    },
    _ => Message::user(&params.message),
};
```

`HandoffOrchestrator::run` and `run_stream` both propagate this. The
plain `message: String` is still set on `HandoffRunParams` (used for
logging + system prompt seeding).

### 9.4 Provider mapping

When a `Message::MultiPart` reaches the provider boundary, the
existing serialization in `agtrs-anthropic` and `agtrs-openai` already
handles `ContentBlock::Text`, `Image`, `ToolUse`, `ToolResult`. We need
to add the mapping for the new `FileRef` variant:

| Provider | FileRef rendering |
|---|---|
| Anthropic | Render as a `Text` block with a header `<file path="…" lines="…" sha="…">` and the file body in a fenced code block. If the file is too large to send as a single text block, split into multiple `Text` blocks (provider-specific limit ~ 200k chars). |
| OpenAI | Render as a `Text` block with the same header + fenced code block. Same size limits. |
| Local / no provider | Render the same way; the model is small enough that the explicit `<file>` header is unnecessary but doesn't hurt. |

Image files route through the existing `ContentBlock::Image` →
provider-image-content mapping (already implemented in F5). No new code
in the provider layer is needed for image mentions.

### 9.5 Resume / persistence

`xaft-session` persists `Message`s via `agtrs-runtime`'s conversation
store. `FileRef` blocks are `Serialize + Deserialize` (we annotate them
accordingly), so they round-trip through SQLite / JSON stores without
modification.

**Staleness warning on resume.** When a session is resumed, the
`SessionResume` hook re-reads the workspace and compares each `FileRef`'s
`sha256` against the current file's SHA. If they differ, the TUI
commits a per-file warning line:

```text
⚠ src/lib.rs has changed since this message was sent (attached sha abc… vs current sha def…)
```

The `FileRef` block itself is not modified — the user explicitly
referenced the version of the file at submission time. The warning is
informational; the agent receives the original content (or the
agent can call `read_file` to get the fresh version if it wants).

---

## 10. Signal & observability

### 10.1 New signals

In `xaft-agent::signals` (mirroring `XaftUserMessageSubmitted` from F21):

```rust
/// Emitted once per submission after `@`-mention resolution has run.
#[derive(Debug, Clone)]
pub struct XaftMentionsResolved {
    /// Total number of mention tokens found in the input.
    pub total_tokens: usize,
    /// Number that resolved to a file.
    pub resolved: usize,
    /// Number that were left as literal text (path-not-found, etc.).
    pub unresolved: usize,
    /// Number of image blocks that will be sent to the provider.
    pub images: usize,
    /// Total bytes of resolved file content (post-truncation).
    pub total_inline_bytes: u64,
    /// Total bytes of resolved image data (base64-encoded).
    pub total_image_bytes: u64,
    /// Time spent in the resolver (microseconds).
    pub resolver_us: u64,
}

#[derive(Debug, Clone)]
pub struct XaftFileRefAttached {
    /// Workspace-relative path.
    pub path: String,
    /// `true` if the file was truncated.
    pub truncated: bool,
    /// Original line count.
    pub line_count: u64,
    /// Original byte size.
    pub byte_size: u64,
    /// SHA-256 of the original content.
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct XaftFileRefNotFound {
    /// The literal token the user typed.
    pub token: String,
    /// The resolved-on-disk path (may differ from the token if `./` was stripped).
    pub path: String,
    /// Why resolution failed.
    pub reason: String,
}

/// Emitted when the user approves a submission containing at least one
/// escape mention. One signal per approved submission (not per
/// individual escape mention — the dialog shows them as a batch).
#[derive(Debug, Clone)]
pub struct XaftEscapeMentionApproved {
    /// Literal @-tokens that were approved (in input order).
    pub tokens: Vec<String>,
    /// Canonicalised absolute paths of the attached files.
    pub absolute_paths: Vec<String>,
    /// Escape reasons, one per token (same length as `tokens` and `absolute_paths`).
    pub reasons: Vec<String>,
    /// `true` if the user pressed `A` (session-wide approval); `false`
    /// for a per-submission `a` press.
    pub session_wide: bool,
}

/// Emitted when the user cancels a submission that contained escape
/// mentions. The original message is NOT sent to the runtime.
#[derive(Debug, Clone)]
pub struct XaftEscapeMentionDenied {
    /// Literal @-tokens that were denied.
    pub tokens: Vec<String>,
    /// Canonicalised absolute paths (recorded so the audit log shows
    /// what the user *would have* attached).
    pub absolute_paths: Vec<String>,
    /// Escape reasons, one per token.
    pub reasons: Vec<String>,
}
```

### 10.2 `/cost` and `/doctor` integration

`/cost` (F25) shows a per-prompt breakdown including file-attachment
bytes so users can see when a large `@`-mention is bloating their token
bill. `/doctor` (F25) checks that `WorkspaceStore::read_bytes` is
reachable for the session's `working_dir` and surfaces a
`⚠ Mention resolver cannot read files — @-mentions disabled` if the
read fails.

---

## 11. WorkspaceStore extension

### 11.1 `read_bytes`

`agtrs_workspace::WorkspaceStore` gains:

```rust
/// Read the raw bytes of a file. Used for binary mentions (images).
/// Returns [`AgtrsError::Other`] if no file exists at that path.
async fn read_bytes(&self, path: &str) -> Result<Vec<u8>, AgtrsError>;
```

The default implementation reads the text form via `read()` and
re-encodes to bytes (lossy for non-UTF-8). `FsWorkspaceStore` overrides
this to read the raw bytes via `tokio::fs::read`. `InMemoryWorkspaceStore`
overrides to return the stored bytes verbatim (storing `Vec<u8>` instead
of `String` is a breaking change to the test helper — see §14).

### 11.2 `head_bytes`

Optional convenience for the byte-cap truncation case:

```rust
/// Read up to `max_bytes` from the start of a file. Used to avoid
/// materialising a 500 MB file in memory just to truncate it to 50 KB.
async fn head_bytes(&self, path: &str, max_bytes: usize) -> Result<Vec<u8>, AgtrsError>;
```

If the file is smaller than `max_bytes`, returns the full contents. If
the file is larger, returns the first `max_bytes` plus a flag
indicating truncation. Default implementation calls `read_bytes` and
truncates; `FsWorkspaceStore` uses `tokio::fs::File::take(max_bytes)`.

For v1 we use a synchronous read in the TUI's resolver path (the
resolver is bounded by the byte cap so memory pressure is fine). The
async API is provided as a follow-up.

---

## 12. Security & sandboxing

### 12.1 Path policy

The policy is now: **all path types are accepted; escape paths require
per-submission confirmation** (or are configured to be silent). The
table below shows the resolution result and the UX consequence.

| Input | Resolves to | Confirmation? |
|---|---|---|
| `src/lib.rs` | `working_dir/src/lib.rs` | No (workspace-relative) |
| `./src/lib.rs` | `working_dir/src/lib.rs` | No (workspace-relative) |
| `../etc/passwd` | `working_dir/../etc/passwd` | **Yes** (parent traversal) |
| `src/../../etc/passwd` | `working_dir/src/../../etc/passwd` | **Yes** (parent traversal after normalisation) |
| `/etc/hosts` | `/etc/hosts` | **Yes** (absolute path) |
| `~/notes.md` | `$HOME/notes.md` | **Yes** (home expansion) |
| `~root/.bashrc` | `~root/.bashrc` (POSIX lookup) | **Yes** (other-user home) |
| `safe → /etc/passwd` (symlink) | `/etc/passwd` (canonicalised) | **Yes** (canonical path is outside workspace) |
| `..` (literal) | not a mention | n/a (no path body) |
| `@` (trailing) | not a mention | n/a (no path body) |
| ` ` (space after `@`) | not a mention | n/a (space breaks the token) |

Confirmation is per-submission, not per-mention: if a single message
contains 3 escape mentions, the dialog is shown once with all 3 listed.
The user's approve/deny decision applies to all of them atomically.

### 12.2 Confirmation flow invariants

- The user message is **not** sent to the runtime until approval.
- The input bar text is **not** cleared until approval.
- A denied submission does not consume the input — the user can edit
  and re-submit.
- The "approve all for this session" choice is recorded in
  `AppState::escape_approved_for_session: bool`. It is reset when the
  session ends, when the user starts a new task, and when the user
  toggles it off via the dialog (pressing `A` again dismisses the
  session-wide approval and reverts to per-submission).
- A new `XaftEscapeMentionApproved` signal is emitted for every
  approved submission, regardless of whether the approval was
  per-submission or session-wide. A `XaftEscapeMentionDenied` signal
  is emitted for every denied submission (the audit log captures both).
- The confirmation dialog itself cannot be bypassed by keyboard
  remapping: the binding `?` in the dialog is hard-coded; the
  `escape_policy = "confirm"` setting is the only way to skip the
  dialog (set in `xaft.toml`).

### 12.3 Provider downgrade for images

A text-only provider receives a `FileRef` (text content) for image files
with a `// (image omitted — provider does not support vision)` note in
the `FileRefContent::Text` body. The original `@path/to/img.png` token
is preserved in the surrounding text block. The user sees a TUI warning:
`⚠ Provider does not support images — saved reference: @path/to/img.png`.

### 12.4 Audit log

The `MentionExpanded` signal (PRD 27 §13) is emitted for every resolved
mention, including the path, byte size, and truncation flag. The audit
log (F39) records this so replayed sessions can show which files were
attached to which messages.

In addition, the new `XaftEscapeMentionApproved` and
`XaftEscapeMentionDenied` signals (see §10) are recorded with the
**canonicalised absolute path** of every escape mention. Replay shows
not only what the user typed (`@/etc/hosts`) but also what was
actually read from disk (`/etc/hosts`, 412 B, sha `…`). This is the
audit trail that protects against both accidental disclosure and
deliberate exfiltration by a compromised agent.

### 12.5 Sensible defaults

The defaults are chosen so that the v0.1 "escape = reject" behaviour
is a strict subset of the v0.2 "escape = confirm" behaviour — a user
who relied on the rejection in v0.1 will see a confirmation dialog
in v0.2, which is strictly more conservative (the file is never
attached unless the user explicitly approves).

| Config | v0.1 default | v0.2 default | Notes |
|---|---|---|---|
| `escape_policy` | `never` (reject) | `confirm` (require approval) | New users see the dialog by default; existing users opt in. |
| `escape_allowlist` | n/a | empty | No silent allowlist. |
| `escape_approved_for_session` | n/a | `false` | Session-wide approval is opt-in via `A` in the dialog. |
| `resolver_max_file_bytes` | 100 MB | 100 MB | Unchanged. Escape path files are subject to the same cap. |
| `image_max_bytes` | 10 MB | 10 MB | Unchanged. |

---

## 13. Cross-cutting concerns

### 13.1 F21 (multi-line input)

A `@<path>` token can span a line break if and only if the path itself
contains a `/` (multi-component). The parser allows `\n` inside a path
body (treated as part of the path until a forbidden character is hit).
For practical use, the user types `@src/` on one line and the rest of
the path on the next:

```text
Look at @src/multi_line/
input.rs and tell me ...
```

This is supported. Single-line tokens (no `/`) cannot cross a newline.

### 13.2 F22 (tab completion)

The F22 `AtMentionCompleter` provides the file-picker UI. This PRD does
not implement F22; the mention feature is fully usable without it. The
contract `AtMentionCompleter::complete(prefix, limit) -> Vec<String>` is
documented in §8.4.

### 13.3 F5 (multimodal paste)

F5 handles pasted/dropped image bytes (from the OS clipboard or file
drop). F3 handles image files referenced via `@<path>`. Both produce
`ContentBlock::Image` blocks; the provider mapping (§9.4) treats them
identically. The TUI's ephemeral hint line aggregates both:

```text
📎 1 image attached (paste), 2 files mentioned (@)
```

### 13.4 F23 (input history)

History recall (Ctrl+R or Up-arrow on empty buffer) is unaware of
mention tokens — it stores and replays the raw input text verbatim. The
user can recall a past prompt that contained `@src/lib.rs` and re-submit
it; the resolver re-runs on the new submission with the current
workspace state.

### 13.5 F25 (/cost, /doctor)

`/cost` includes file-attachment bytes per prompt. `/doctor` validates
that the resolver can read the workspace.

### 13.6 F34 (watch mode)

If watch mode is enabled and a file mentioned in an active session
changes, the TUI commits a per-file line `⚠ src/lib.rs changed since
attachment` to the transcript. The `FileRef` block is not auto-updated;
the agent decides whether to re-read.

### 13.7 F39 (audit log)

`MentionExpanded` and `FileRefNotFound` signals are recorded in the
audit log. Session replay shows the original `@<path>` tokens and the
file metadata (path, sha, byte size).

---

## 14. AGENTS.md compliance

Per `AGENTS.md` (the workflow-state mutation rule), this PRD does **not**
introduce any new workflow-state mutation paths. The mention resolver
operates on the input bar (local UI state) and produces a structured
message that flows through the **existing** `user_message_tx` channel
(now typed as `UserMessage` instead of `String`).

| Concern | Status |
|---|---|
| Direct state mutation from agent | None. The resolver is a pure function over the input text + workspace. |
| Structured outputs as state mutations | None. The `Vec<ContentBlock>` is a wire payload, not a state mutation. It is consumed by the runtime's existing message-construction path. |
| Hidden state mutation | None. The TUI commit lines (literal user message + warnings + "N files attached" hint) are the observable state changes, all driven by existing `RenderMutation` commands. |
| Tool-only state mutations | Preserved. The resolver does not call any state-mutating tools; it reads the workspace via `WorkspaceStore::read` / `read_bytes`, which are read-only. |

This PRD is AGENTS.md-compliant and does not require any new approval
gates.

---

## 15. Out-of-scope follow-ups

- **Image preview in TUI.** Render a small ASCII-art thumbnail in the
  ephemeral region. (Requires `image` crate's `imageproc` or a custom
  half-block renderer.)
- **Drag-and-drop file mentions.** TUI listens for SGR mouse drag events
  that look like file paths; expand them to mentions. (Most terminals
  don't emit useful drop events; this is a low-priority follow-up.)
- **Glob expansion.** `@src/**/*.rs` expands to all matching files. The
  parser would need a glob crate (`globset`, already in workspace
  dependencies) and the resolver would return a `Vec<FileRef>`. Tracked
  as F45.
- **Staleness auto-refresh.** A configurable option to automatically
  re-read a file before sending the message if it has changed since the
  user started typing the path. Currently we always re-read at submit
  time; the auto-refresh would be at typing time. Not requested.
- **PDF / Office support.** F5's `Document` block will handle these for
  pasted files. For `@path/to/file.pdf` mentions, the same code path
  will work once the `Document` block is implemented upstream.
- **`@@`-escape for literal `@`.** Implemented in v1 (`\@`).
- **`@@user`-style team mentions.** Out of scope. (Not requested; would
  require a user directory and a permission model.)

---

## 16. Open questions

1. **Truncation prompt for the LLM.** When a file is truncated, the
   `FileRef` block contains the visible portion and a note. Should the
   resolver also include a "see also: `read_file` with start_line=N+1" hint
   so the LLM knows how to read the rest? **Proposal:** yes, add a
   `// (use read_file with start_line=2001, end_line=4232 to read more)`
   note when truncation occurs.
2. **Multi-file deduplication.** If a user writes `@src/lib.rs` three
   times in one message, should the resolver attach the file three
   times (verbose, but explicit) or once (de-duplicated via SHA, but the
   text doesn't reflect that)? **Proposal:** attach each occurrence
   separately. The LLM gets the same content three times, which is
   wasteful but matches the user's literal intent. A follow-up
   optimisation can de-duplicate if the SHA matches and the user opts in
   via `mention.dedupe = true`.
3. **Workspace scope.** If `working_dir` is a git worktree, should the
   resolver follow the worktree's tracked files or the main branch's
   files? **Proposal:** follow the worktree. `FsWorkspaceStore` already
   operates on the live filesystem; the worktree is the live view.
4. **Encoding fallback for non-UTF-8.** When a file is not valid UTF-8,
   we replace invalid bytes with `U+FFFD`. Should we instead skip the
   file and produce a warning? **Proposal:** lossy replacement (matches
   `read_file` policy; the LLM can usually infer intent from
   surrounding context).
5. **File size cap for the resolver itself.** Should the resolver refuse
   to read files larger than some absolute cap (e.g. 100 MB) to avoid
   OOM? **Proposal:** yes. Add `resolver_max_file_bytes` (default 100 MB)
   to `[mention]`. Files exceeding this cap are rejected with a warning,
   not silently truncated.
6. **Concurrent reads.** If a message contains 10 mentions, do we read
   them sequentially or in parallel? **Proposal:** parallel via
   `tokio::join!` (the workspace store is async). The cap of 10 also
   matches the F22 popup's `max_results` default.

---

## 17. Examples

### 17.1 Simple text mention

**Input:**
```text
Fix the bug in @src/lib.rs
```

**TUI commit lines:**
```text
❯ Fix the bug in @src/lib.rs
   ⎿  1 file attached: src/lib.rs (211 lines)
```

**On-wire message (truncated for brevity):**
```rust
Message {
    role: Role::User,
    content: MessageContent::MultiPart(vec![
        ContentBlock::Text { text: "Fix the bug in ".into() },
        ContentBlock::FileRef {
            path: "src/lib.rs".into(),
            content: FileRefContent::Text("// 1 | use std::io;\n// 2 | …".into()),
            truncation: None,
            byte_size: 6_823,
            line_count: 211,
            sha256: "9a3c…",
        },
    ]),
    ..Default::default()
}
```

### 17.2 Mixed text + image

**Input:**
```text
Compare @src/main.rs with the diagram in @docs/diagram.png
```

**TUI commit lines:**
```text
❯ Compare @src/main.rs with the diagram in @docs/diagram.png
   ⎿  1 file attached, 1 image: src/main.rs (45 lines), docs/diagram.png
```

**On-wire message:**
```rust
Message {
    role: Role::User,
    content: MessageContent::MultiPart(vec![
        ContentBlock::Text { text: "Compare ".into() },
        ContentBlock::FileRef { path: "src/main.rs".into(), content: FileRefContent::Text("…".into()), … },
        ContentBlock::Text { text: " with the diagram in ".into() },
        ContentBlock::Image {
            url: String::new(),
            media_type: "image/png".into(),
            data: Some("iVBORw0KGgo…".into()),  // base64
        },
    ]),
    ..Default::default()
}
```

### 17.3 Unresolved mention

**Input:**
```text
Update the schema in @db/schema.sql and the tests in @tests/missing.rs
```

**TUI commit lines:**
```text
❯ Update the schema in @db/schema.sql and the tests in @tests/missing.rs
   ⎿  1 file attached: db/schema.sql (87 lines)
⚠ File not found: tests/missing.rs
```

**On-wire message:**
```rust
Message {
    role: Role::User,
    content: MessageContent::MultiPart(vec![
        ContentBlock::Text { text: "Update the schema in ".into() },
        ContentBlock::FileRef { path: "db/schema.sql".into(), content: FileRefContent::Text("…".into()), … },
        ContentBlock::Text { text: " and the tests in @tests/missing.rs".into() },  // literal preserved
    ]),
    ..Default::default()
}
```

The LLM sees the literal `@tests/missing.rs` and can either ignore it or
call `read_file` to confirm it's missing.

### 17.4 Truncated mention

**Input:**
```text
Refactor @src/big_module.rs
```

**TUI commit lines:**
```text
❯ Refactor @src/big_module.rs
   ⎿  1 file attached: src/big_module.rs (truncated: 2000 of 4232 lines)
```

**On-wire `FileRef`:**
```rust
ContentBlock::FileRef {
    path: "src/big_module.rs".into(),
    content: FileRefContent::Text("// first 2000 lines…\n// (truncated, 2232 more lines; use read_file with start_line=2001)".into()),
    truncation: Some(Truncation {
        shown_lines: 2000,
        total_lines: 4232,
        shown_bytes: 64_512,
        total_bytes: 137_064,
    }),
    byte_size: 137_064,
    line_count: 4232,
    sha256: "7b4e…",
},
```

### 17.5 Escape mention — approved (parent traversal)

**Input:**
```text
Compare our @src/api.rs to the upstream @../upstream-project/api.rs
```

**TUI commit lines** (after the user presses `a` in the dialog):
```text
❯ Compare our @src/api.rs to the upstream @../upstream-project/api.rs
   ⎿  2 files attached: src/api.rs (211 lines), ../upstream-project/api.rs (498 lines)
```

**Confirmation dialog shown before submission:**
```text
┌─ Attach files outside the workspace? ───────────────────────────┐
│                                                                 │
│  The following 1 file(s) is OUTSIDE the workspace:              │
│                                                                 │
│    @../upstream-project/api.rs                                   │
│      → /home/user/projects/upstream-project/api.rs               │
│      → 18.7 KB · 498 lines                                       │
│                                                                 │
│  These files will be sent to the LLM in your next message.       │
│                                                                 │
│  [a]pprove once   [A]pprove all escape mentions this session     │
│  [c]ancel and edit                                                 │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

`src/api.rs` is in the workspace, so it does **not** appear in the
confirmation dialog. Only the escape mention is listed. The user
presses `a`, the dialog closes, the message is committed, and the
runtime receives both files.

**On-wire message (the @-tokens are NOT replaced with bare paths in the
text blocks; the surrounding text is preserved verbatim):**
```rust
Message {
    role: Role::User,
    content: MessageContent::MultiPart(vec![
        ContentBlock::Text { text: "Compare our ".into() },
        ContentBlock::FileRef {
            path: "src/api.rs".into(),
            content: FileRefContent::Text("…".into()),
            byte_size: 6_823, line_count: 211, sha256: "9a3c…", ..Default::default()
        },
        ContentBlock::Text { text: " to the upstream ".into() },
        ContentBlock::FileRef {
            path: "../upstream-project/api.rs".into(),
            content: FileRefContent::Text("…".into()),
            byte_size: 19_148, line_count: 498, sha256: "4aaf…",
            escape: Some(EscapeInfo {
                reason: EscapeReason::ParentTraversal,
                absolute_path: "/home/user/projects/upstream-project/api.rs",
                depth: 1,
                byte_size: 19_148,
            }),
            ..Default::default()
        },
    ]),
    ..Default::default()
}
```

The `escape` field is carried on the wire so the audit log (F39) and
provider-specific tool descriptions (F5) can record that this `FileRef`
is a workspace-escape attachment. The `path` field still holds the
**literal** form the user typed (e.g. `../upstream-project/api.rs`), not
the canonicalised absolute path — the absolute path is in `escape`.

### 17.6 Escape mention — denied

**Input** (same as §17.5, but the user changes their mind):
```text
Compare our @src/api.rs to the upstream @../upstream-project/api.rs
```

**User presses `Enter`, sees the dialog, then presses `c`.**

**TUI commit lines:** *(no commit — the message is not sent)*
```text
┌─ Attach files outside the workspace? ───────────────────────────┐
│  …  (dialog as above)                                            │
└─────────────────────────────────────────────────────────────────┘
(user presses `c`)
```

The dialog closes, the input bar's text is **restored verbatim** so the
user can edit it (e.g. change `../upstream-project` to a different path
or remove the mention entirely), and the audit log records
`XaftEscapeMentionDenied { token: "../upstream-project/api.rs", reason: ParentTraversal }`.

**On-wire message:** *(none — the submission is dropped)*

### 17.7 Escape mention — absolute path

**Input:**
```text
Why is @/etc/hosts pointing to 127.0.0.1?
```

**Confirmation dialog:**
```text
┌─ Attach files outside the workspace? ───────────────────────────┐
│                                                                 │
│  The following 1 file(s) is OUTSIDE the workspace:              │
│                                                                 │
│    @/etc/hosts                                                   │
│      → /etc/hosts                                                │
│      → 412 B · 14 lines                                          │
│                                                                 │
│  These files will be sent to the LLM in your next message.       │
│                                                                 │
│  [a]pprove once   [A]pprove all escape mentions this session     │
│  [c]ancel and edit                                                 │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

**On-wire `FileRef` block:**
```rust
ContentBlock::FileRef {
    path: "/etc/hosts".into(),
    content: FileRefContent::Text("127.0.0.1 localhost\n…".into()),
    byte_size: 412, line_count: 14, sha256: "…",
    escape: Some(EscapeInfo {
        reason: EscapeReason::Absolute,
        absolute_path: "/etc/hosts",
        depth: 0,
        byte_size: 412,
    }),
    ..Default::default()
}
```

### 17.8 Home expansion

**Input:**
```text
Append my @~/notes/todo.md to the task list
```

**Confirmation dialog** (assuming the user's home is `/home/alice`):
```text
┌─ Attach files outside the workspace? ───────────────────────────┐
│                                                                 │
│  The following 1 file(s) is OUTSIDE the workspace:              │
│                                                                 │
│    @~/notes/todo.md                                              │
│      → /home/alice/notes/todo.md                                 │
│      → 2.1 KB · 47 lines                                         │
│                                                                 │
│  These files will be sent to the LLM in your next message.       │
│                                                                 │
│  [a]pprove once   [A]pprove all escape mentions this session     │
│  [c]ancel and edit                                                 │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

**On-wire `FileRef` block:**
```rust
ContentBlock::FileRef {
    path: "~/notes/todo.md".into(),
    content: FileRefContent::Text("…".into()),
    byte_size: 2_156, line_count: 47, sha256: "…",
    escape: Some(EscapeInfo {
        reason: EscapeReason::HomeExpansion,
        absolute_path: "/home/alice/notes/todo.md",
        depth: 0,
        byte_size: 2_156,
    }),
    ..Default::default()
}
```

The `path` field preserves the **literal** form (`~/notes/todo.md`) so
the original token is reflected in the structured message. The LLM
sees a `FileRef` with a `~/` prefix, the resolver has already
canonicalised it to `/home/alice/notes/todo.md`, and the audit log
records both forms.

---

## 18. Cross-reference

| Section | 27 §F3 | 28 | 30a (this) | 30b |
|---|---|---|---|---|
| Originating requirement | ✓ | — | — | — |
| Tab completion (F22) | — | §F22 | §8.4 | §11 |
| Multimodal paste (F5) | — | — | §13.3 | §10 |
| Provider mapping | — | — | §9.4 | §12 |
| Multi-line input (F21) | — | — | §13.1 | §9 |
| Audit log (F39) | — | §F39 | §12.3 | §14 |
| Cost / doctor (F25) | — | — | §10.2 | §13 |
| Watch mode (F34) | — | §F34 | §13.6 | — |

---

## 19. Summary

`@<path>` is the single most-requested missing primitive in xaft's input
language. This PRD specifies a complete, secure, and testable design that
slots into the existing F21 multi-line input bar, the F22 tab completer,
and the F5 multimodal pipeline. Total estimated scope:

- **1 new content-block variant** in `agtrs-runtime::transport::ContentBlock`
  (`FileRef`).
- **1 new module** in `xaft-tui` (`mention.rs`, ~600 LOC including tests).
- **1 new method** on `WorkspaceStore` (`read_bytes`).
- **2 new fields** on existing structs (`RunRequest::user_message`,
  `HandoffRunParams::user_parts`).
- **3 new signals** in `xaft-agent` (`XaftMentionsResolved`,
  `XaftFileRefAttached`, `XaftFileRefNotFound`).
- **1 new TUI hint line** ("N files attached").
- **No new dependencies.**

Backward compatibility is preserved by the `user_message: Option<…>` and
`user_parts: Option<…>` fields being optional; existing callers using
`task: String` continue to work unchanged.
