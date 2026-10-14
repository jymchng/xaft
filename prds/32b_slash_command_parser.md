# PRD 32b — Slash Command System: Parser & Autocomplete Palette

## Status: Draft
## Depends on: PRD 32a

---

## 1. Scope

This document specifies the parsing layer and the palette (autocomplete overlay)
in full implementation detail.  It is the handoff document for the engineer
implementing `slash/parser.rs` and `slash/palette.rs`.

---

## 2. Parser (`slash/parser.rs`)

### 2.1 Public API

```rust
pub struct SlashCommandParser;

impl SlashCommandParser {
    /// Parse a raw input string.
    ///
    /// Returns `None` when the input is not a slash command (does not start
    /// with `/`, or is just `/` with no trigger word yet).
    /// Returns `Some(Err(msg))` when the trigger is known but arguments are
    /// invalid.
    /// Returns `Some(Ok(cmd))` when parsing succeeds.
    pub fn parse(input: &str) -> Option<Result<SlashCommand, String>>;

    /// Return all trigger strings (sorted alphabetically) whose prefix
    /// matches `partial`.  `partial` should NOT include the leading `/`.
    pub fn completions(partial: &str) -> Vec<&'static str>;

    /// Return the single best completion when `partial` uniquely identifies
    /// one command, otherwise `None`.
    pub fn unique_completion(partial: &str) -> Option<&'static str>;
}
```

### 2.2 Parsing algorithm

```
input = input.trim()

1. If !input.starts_with('/') → return None

2. Strip leading '/'.  Split on first whitespace:
     trigger = first token (lowercased)
     args    = rest (may be empty)

3. If trigger is empty → return None   (user just typed '/')

4. Look up trigger in COMMAND_TABLE:
     - Exact match → proceed to step 5
     - No match    → return Some(Err("Unknown command: /{trigger}. ..."))

5. Call the command-specific arg parser:
     parse_args_{command}(args) → Result<SlashCommand, String>

6. Return Some(result from step 5)
```

### 2.3 `COMMAND_TABLE`

A static `phf::Map<&str, CommandMeta>` (perfect hash, zero runtime allocation).

```rust
struct CommandMeta {
    canonical: &'static str,   // e.g. "clear"
    aliases:   &'static [&'static str],  // e.g. &[]
    parse_fn:  fn(&str) -> Result<SlashCommand, String>,
    description: &'static str,
    args_hint:   Option<&'static str>,
}
```

All aliases are registered as separate entries pointing to the same `CommandMeta`.

### 2.4 Command-specific arg parsers

Each command has a `parse_args_X` function.  The contract:

- **No args expected**: return `Ok(SlashCommand::X)` regardless of trailing
  whitespace; return `Err` only if non-whitespace args are given (soft — emit
  a warning line but still execute).
- **Optional arg**: parse if present, use `None` if absent.
- **Required arg**: return `Err` with usage hint if absent.

#### Examples

```rust
fn parse_args_model(args: &str) -> Result<SlashCommand, String> {
    let name = args.trim();
    if name.is_empty() {
        return Err("/model requires a model name.\n  Usage: /model <name>".into());
    }
    Ok(SlashCommand::Model { name: name.to_string() })
}

fn parse_args_resume(args: &str) -> Result<SlashCommand, String> {
    let id = args.trim();
    Ok(SlashCommand::Resume {
        id: if id.is_empty() { None } else { Some(id.to_string()) },
    })
}

fn parse_args_rewind(args: &str) -> Result<SlashCommand, String> {
    let s = args.trim();
    if s.is_empty() {
        return Ok(SlashCommand::Rewind { msg_index: None });
    }
    s.parse::<usize>()
        .map(|n| SlashCommand::Rewind { msg_index: Some(n) })
        .map_err(|_| format!("/rewind expects a message number, got {:?}", s))
}
```

### 2.5 Alias table

| Primary | Aliases |
|---------|---------|
| `/vim` | `/vi` |
| `/emacs` | — |
| `/compact` | `/ctx` |
| `/clear` | `/cls` |
| `/cost` | `/tokens` |
| `/quit` | `/exit`, `/q` |

> `/quit` is not in the main command set (it's handled at the event loop level)
> but the parser must recognise `/exit` and `/q` and return a dedicated
> `SlashCommand::Quit` variant so the event loop can handle it cleanly.

### 2.6 Completions algorithm

```rust
pub fn completions(partial: &str) -> Vec<&'static str> {
    let p = partial.to_ascii_lowercase();
    COMMAND_TABLE
        .entries()
        .filter(|(k, _)| k.starts_with(p.as_str()))
        .map(|(k, _)| *k)
        .sorted()
        .collect()
}
```

`unique_completion` returns `Some` only when exactly one entry matches.

---

## 3. Palette (`slash/palette.rs`)

### 3.1 State

```rust
#[derive(Debug, Clone)]
pub struct SlashPalette {
    /// The text currently typed after '/' (no leading slash).
    pub partial: String,
    /// Filtered, sorted list of matching trigger strings.
    pub candidates: Vec<&'static str>,
    /// Index of the highlighted candidate (0-based).
    pub selected: usize,
    /// Per-candidate descriptions (parallel to `candidates`).
    pub descriptions: Vec<&'static str>,
    /// Per-candidate args hints (parallel to `candidates`).
    pub args_hints: Vec<Option<&'static str>>,
}
```

### 3.2 Lifecycle

```
AppState.slash_palette: Option<SlashPalette>
```

| Event | Action |
|-------|--------|
| Input bar content becomes `/` (or more) | `slash_palette = Some(SlashPalette::new(partial))` |
| User types more chars | `slash_palette.as_mut()?.update(new_partial)` |
| User types `Tab` | `palette.select_next()` |
| User types `Shift+Tab` | `palette.select_prev()` |
| User types `↑` | `palette.select_prev()` |
| User types `↓` | `palette.select_next()` |
| User types `Enter` | `slash_palette = None`; fire command |
| User types `Escape` | `slash_palette = None`; keep input text |
| Input bar loses `/` prefix | `slash_palette = None` |

### 3.3 `SlashPalette::new(partial)`

```rust
impl SlashPalette {
    pub fn new(partial: &str) -> Self {
        let candidates = SlashCommandParser::completions(partial);
        let descriptions = candidates.iter()
            .map(|t| COMMAND_TABLE[*t].description)
            .collect();
        let args_hints = candidates.iter()
            .map(|t| COMMAND_TABLE[*t].args_hint)
            .collect();
        Self {
            partial: partial.to_string(),
            candidates,
            selected: 0,
            descriptions,
            args_hints,
        }
    }

    pub fn update(&mut self, partial: &str) {
        *self = Self::new(partial);
    }

    pub fn selected_trigger(&self) -> Option<&&'static str> {
        self.candidates.get(self.selected)
    }

    pub fn select_next(&mut self) {
        if self.candidates.is_empty() { return; }
        self.selected = (self.selected + 1) % self.candidates.len();
    }

    pub fn select_prev(&mut self) {
        if self.candidates.is_empty() { return; }
        self.selected = self.selected
            .checked_sub(1)
            .unwrap_or(self.candidates.len() - 1);
    }
}
```

### 3.4 Rendering the palette

The palette is rendered by `IncrementalRenderer` as ephemeral lines immediately
**above** the input bar (between the transcript and the input bar), following the
same approach as the `@`-mention autocomplete.

Max 8 candidates shown; a `▼ N more` indicator appears when the list is longer.

#### Row format

```
  /clear    Reset the current session transcript
  /compact  Manually trigger context compaction
▶ /config   Open the config editor              ← highlighted row
  /cost     Show per-agent token/cost table
  /commit   Commit session changes to git
```

Leading `▶` marks the selected row.  The trigger word is left-padded to the
longest candidate width + 2 spaces.  Descriptions are dimmed.

#### Column widths

```rust
let trigger_width = candidates.iter().map(|t| t.len()).max().unwrap_or(0) + 3;
// "/clear   " → "/clear" + padding
let desc_col = trigger_width;
```

#### Colour

| Element | Style |
|---------|-------|
| Selected row prefix `▶` | `theme.accent` |
| Selected trigger | bold `theme.text` |
| Unselected trigger | `theme.text` |
| Description | `theme.dim` |
| Args hint (shown after description) | italic `theme.dim` |

### 3.5 Tab completion behaviour

When `Tab` is pressed:
1. If `palette.candidates.len() == 1`, the input bar text is replaced with
   `/{candidate} ` (trailing space ready for arg input).
2. If multiple candidates, `select_next()` is called (cycle through).
3. When `Enter` is pressed with a highlighted candidate and partial text that is
   a prefix of that candidate, the trigger is completed to the full canonical
   form before parsing.

---

## 4. History Ring

```rust
pub struct SlashHistory {
    ring: VecDeque<String>,
    capacity: usize,
    cursor: Option<usize>,   // None = not browsing history
}

impl SlashHistory {
    pub fn push(&mut self, cmd: String);
    pub fn prev(&mut self, current: &str) -> Option<&str>;
    pub fn next(&mut self) -> Option<&str>;
    pub fn reset_cursor(&mut self);
}
```

History is stored on `AppState`.  It is initialised empty and never persisted.

- `push` is called after a command executes successfully.
- `prev` / `next` are bound to `↑` / `↓` **when palette mode is active**.
- `reset_cursor` is called when the user types any non-navigation character.

---

## 5. Integration with `AppState`

### 5.1 New fields on `AppState`

```rust
pub slash_palette: Option<SlashPalette>,
pub slash_history: SlashHistory,
```

### 5.2 `handle_input_submitted` changes

```rust
fn handle_input_submitted(&mut self, text: String) {
    let trimmed = text.trim();
    if trimmed.starts_with('/') {
        match SlashCommandParser::parse(trimmed) {
            None => { /* bare '/', ignore */ }
            Some(Err(msg)) => {
                self.push_error_line(msg);
            }
            Some(Ok(cmd)) => {
                self.execute_slash_command(cmd, trimmed);
            }
        }
        self.slash_palette = None;
        return;
    }
    // … existing agent dispatch path …
}
```

### 5.3 `execute_slash_command`

```rust
fn execute_slash_command(&mut self, cmd: SlashCommand, raw: &str) {
    let ctx = self.build_command_context();
    let handler = self.registry.get(&cmd);
    self.push_separator(raw);

    if handler.is_async() {
        let fut = handler.execute_async(ctx);
        self.spawn_command_future(fut);
    } else {
        let result = handler.execute(ctx);
        self.apply_command_result(result);
    }

    self.slash_history.push(raw.to_string());
    // Signal emission happens inside apply_command_result / spawn_command_future.
}
```

---

## 6. Edge Cases

| Input | Expected behaviour |
|-------|--------------------|
| `/` alone | Palette opens with all commands; Enter does nothing |
| `/ help` (space before word) | Treated as plain text, forwarded to agent |
| `/CLEAR` | Lowercased → matches `/clear` |
| `/model` (no name, Enter) | `Err` returned, usage hint printed, no execution |
| `/rewind abc` | `Err("expects a message number")`, no execution |
| `/vim /emacs` | Parser reads `vim` as trigger, `/emacs` as args; args-parser for vim ignores args (soft) |
| Input bar has newlines | Slash command detection only applies to single-line input. Multi-line → plain text |

---

## 7. Unit Test Cases (parser-level)

See PRD 32e for the full test plan. Parser-specific cases:

| Test | Input | Expected |
|------|-------|---------|
| `parse_slash_help` | `/help` | `Ok(SlashCommand::Help)` |
| `parse_slash_model_ok` | `/model claude-3-5-sonnet` | `Ok(SlashCommand::Model { name: "claude-3-5-sonnet" })` |
| `parse_slash_model_missing` | `/model` | `Err(contains "/model requires")` |
| `parse_slash_rewind_number` | `/rewind 5` | `Ok(SlashCommand::Rewind { msg_index: Some(5) })` |
| `parse_slash_rewind_bad` | `/rewind foo` | `Err(contains "expects a message number")` |
| `parse_slash_unknown` | `/blorp` | `Err(contains "Unknown command")` |
| `parse_plain_text` | `hello` | `None` |
| `parse_bare_slash` | `/` | `None` |
| `parse_case_insensitive` | `/CLEAR` | `Ok(SlashCommand::Clear)` |
| `completions_empty` | `""` | all 21+ triggers |
| `completions_c` | `"c"` | `["clear", "commit", "compact", "config", "cost"]` |
| `completions_cl` | `"cl"` | `["clear"]` |
| `unique_completion_hit` | `"cl"` | `Some("clear")` |
| `unique_completion_miss` | `"c"` | `None` |
