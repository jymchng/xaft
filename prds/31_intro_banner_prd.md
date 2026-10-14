# PRD 31 — Introduction Banner Component

**Status:** Proposal
**Target:** next minor release
**Primary crate:** `xaft-tui`
**Touched crates:** `xaft-tui`, `xaft-config`

---

## 1. Background

When xaft starts without an initial task argument (interactive mode), the
transcript area is empty and the user sees nothing above the prompt border.
There is no branding, no version indicator, and no contextual hints.

Claude Code greets the user with a branded header and a short help line.
xaft should do the same: show an ASCII banner on first launch, give the
user orientation, then clear it the moment the first message is committed
to the transcript.

---

## 2. Goal

Display a self-clearing introduction banner in the transcript area when
xaft starts in interactive mode (no initial task). The banner must:

1. Feature the product name **xaft** as a large ASCII block-text logo.
2. Show the version string and a one-line tagline.
3. List the three most important key bindings so a new user is not stuck.
4. Disappear automatically when the first transcript line is committed
   (i.e. the user sends their first message or a task starts).
5. Never appear when xaft is invoked with an initial task string.

---

## 3. Visual Design

### 3.1 ASCII logo

```
██╗  ██╗ █████╗ ███████╗████████╗
╚██╗██╔╝██╔══██╗██╔════╝╚══██╔══╝
 ╚███╔╝ ███████║█████╗     ██║
 ██╔██╗ ██╔══██║██╔══╝     ██║
██╔╝ ██╗██║  ██║██║        ██║
╚═╝  ╚═╝╚═╝  ╚═╝╚═╝        ╚═╝
```

Six rows, ~38 columns. Rendered in `theme.warning` (yellow) to match the
border colour and give a warm first impression.

### 3.2 Full banner layout

```
                                              (blank line)
  ██╗  ██╗ █████╗ ███████╗████████╗
  ╚██╗██╔╝██╔══██╗██╔════╝╚══██╔══╝
   ╚███╔╝ ███████║█████╗     ██║
   ██╔██╗ ██╔══██║██╔══╝     ██║
  ██╔╝ ██╗██║  ██║██║        ██║
  ╚═╝  ╚═╝╚═╝  ╚═╝╚═╝        ╚═╝
                                              (blank line)
  autonomous coding agent · v{VERSION}        (dim, italic)
                                              (blank line)
  Enter → send   Shift+Enter → newline   Ctrl+C → cancel
                                              (blank line)
```

Colours:

| Element | Colour |
|---------|--------|
| ASCII logo | `theme.warning` |
| Tagline + version | `theme.dim` |
| Key binding keys | `theme.warning` |
| Key binding descriptions | `theme.dim` |

Total height: ~11 lines (logo 6 + blanks 4 + tagline 1).

### 3.3 Minimum terminal width

If `term_cols < 42` the logo is replaced with the plain text `xaft` in
`theme.warning` + bold, so the banner degrades gracefully on narrow
terminals.

---

## 4. Behaviour

### 4.1 Show condition

The banner is committed as a special `LineKind::Banner` block to the
transcript at startup **only when** `task.is_empty()` (no initial task
string was supplied to `TuiApp::run`).

### 4.2 Auto-clear

`LineKind::Banner` lines are never re-rendered after a real transcript
line (any `LineKind` that is not `Banner`) is committed. The renderer
skips `Banner` lines in all output once `transcript_has_real_lines` is
`true`.

Simpler alternative: `Banner` lines are committed to `output_lines` in
`AppState`. When the first real line is committed, all leading `Banner`
lines are drained from `output_lines`. The renderer has no special logic.

**Chosen approach:** drain on first real commit (simpler, no renderer
changes).

### 4.3 No banner on task start

When `--task "…"` is supplied at the CLI, `task.is_empty()` is `false`
and the banner is never pushed.

### 4.4 Resume sessions

When `--resume <id>` is used and the session has prior conversation
history, the banner is also suppressed (existing transcript lines are
present, so `output_lines` is non-empty before the user types anything).

---

## 5. Implementation

### 5.1 New type: `IntroBanner`

```
crates/xaft-tui/src/intro.rs   (new file)
```

```rust
pub struct IntroBanner {
    pub lines: Vec<StyledLine>,   // pre-built, ready to commit
}

impl IntroBanner {
    pub fn build(version: &str, term_cols: u16, theme: &Theme) -> Self { … }
}
```

`build` produces a `Vec<StyledLine>` using `LineKind::Banner`. The logo
rows use `theme.warning`; the tagline and hints use `theme.dim` and
`theme.warning` for key names.

The version string is sourced from `env!("CARGO_PKG_VERSION")` at the
call site in `app.rs`.

### 5.2 `LineKind::Banner`

Add variant to `transcript.rs`:

```rust
pub enum LineKind {
    // … existing variants …
    Banner,
}
```

`style_for_kind(Banner, theme)` returns `theme.fg` (neutral); the actual
colours are baked into each `StyledLine` at build time.

### 5.3 Push on startup in `app.rs`

```rust
// After `AppState::new(task.clone())`, before `init_prompt`:
if task.is_empty() {
    let banner = IntroBanner::build(
        env!("CARGO_PKG_VERSION"),
        renderer.term_cols(),
        &self.theme,
    );
    for line in banner.lines {
        state.mutations.push(RenderMutation::CommitLine(line));
    }
}
```

`renderer.term_cols()` requires a new `pub fn term_cols(&self) -> u16`
accessor on `IncrementalRenderer`.

### 5.4 Auto-drain in `AppState`

Add `banner_line_count: usize` to `AppState` (set to `banner.lines.len()`
at startup, or `0` if no banner was shown).

In `commit_line` (or wherever `output_lines` is appended):

```rust
if line.kind != LineKind::Banner && self.banner_line_count > 0 {
    // drain leading banner lines so they don't re-render on resize
    self.output_lines.drain(..self.banner_line_count);
    self.banner_line_count = 0;
}
```

This keeps `output_lines` clean: once a real line arrives the banner
entries are gone and will never be replayed.

### 5.5 `StyledLine` coloured spans

Currently `StyledLine` holds a single `(text, kind)` pair. To render
mixed-colour rows (key name in yellow + description in dim on the same
line), either:

- **Option A** — split into two `StyledLine` entries and print them
  consecutively without a newline between them (requires renderer
  change to support `no_newline` lines).
- **Option B** — pre-format with ANSI escape codes embedded in the
  `text` field (quick and dependency-free, but bypasses the theme
  system).
- **Option C** — use separate lines for key names and descriptions
  (simplest, no renderer change).

**Chosen:** Option C. Each hint occupies its own line; bold/colour is
set by `LineKind`. The loss of inline mixed colour is acceptable for
the MVP.

---

## 6. Configuration

Add to `XaftConfig` / `[tui]` section:

```toml
[tui]
show_intro_banner = true   # default: true
```

`show_intro_banner = false` suppresses the banner entirely (useful for
scripted environments even without `--task`).

`Default` impl returns `true`.

---

## 7. Acceptance Criteria

| # | Criterion |
|---|-----------|
| 1 | `xaft` (no args) displays the ASCII logo + tagline + hints above the prompt border on first launch. |
| 2 | Logo renders in yellow; tagline and hints render in dim. |
| 3 | On terminals narrower than 42 columns, plain `xaft` text is shown instead of the logo. |
| 4 | The banner disappears as soon as the user sends their first message. |
| 5 | `xaft --task "…"` shows no banner. |
| 6 | `xaft --resume <id>` shows no banner when prior conversation lines exist. |
| 7 | `show_intro_banner = false` in config suppresses the banner. |
| 8 | Unit test: `IntroBanner::build` returns the expected line count and `LineKind::Banner` for all lines. |
| 9 | Unit test: drain logic removes banner lines on first real commit. |

---

## 8. Out of Scope

- Animated or blinking logo (terminal compatibility issues).
- Interactive "what do you want to do today?" prompt inside the banner.
- Persistent session stats (total cost, sessions) shown in the banner.
