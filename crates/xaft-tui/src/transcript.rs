//! Transcript line types and helper functions for the conversational renderer.

use std::time::Instant;

// ── Line types ────────────────────────────────────────────────────────────────

/// A fully-styled line ready for the incremental renderer.
#[derive(Debug, Clone)]
pub struct StyledLine {
    pub text: String,
    pub kind: LineKind,
    pub timestamp: Instant,
    pub agent: Option<String>,
}

impl StyledLine {
    pub fn new(text: impl Into<String>, kind: LineKind) -> Self {
        Self {
            text: text.into(),
            kind,
            timestamp: Instant::now(),
            agent: None,
        }
    }

    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }
}

/// Semantic kind of a transcript line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    AgentText,
    AgentMarker,
    ToolCall,
    ToolResult,
    System,
    Error,
    Success,
    UserMessage,
    Separator,
    DiffAdd,
    DiffRemove,
    DiffContext,
}

/// Styling hint for streaming tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineStyle {
    Normal,
    Dim,
}

// ── RenderMutation ────────────────────────────────────────────────────────────

/// Commands produced by `AppState::handle_event`, consumed by `IncrementalRenderer`.
#[derive(Debug, Clone)]
pub enum RenderMutation {
    /// Append a permanent line to the transcript.
    CommitLine(StyledLine),
    /// Append a fragment to the current streaming line.
    StreamToken { fragment: String, style: LineStyle },
    /// Close the current streaming line (make it permanent).
    FlushStream,
    /// Replace ephemeral region. `None` clears it entirely.
    SetEphemeral(Option<crate::ephemeral::EphemeralState>),
    /// Redraw the prompt line.
    UpdatePrompt(crate::prompt::PromptState),
    /// Terminal was resized.
    Resize { cols: u16, rows: u16 },
    /// Initiate graceful shutdown.
    Shutdown,
}

// ── Inline diff ───────────────────────────────────────────────────────────────

/// Build inline diff lines from an `edit_file` or `write_file` tool call.
///
/// Returns a `Vec<StyledLine>` ready for `RenderMutation::CommitLine`.
pub fn build_file_diff_lines(
    tool_name: &str,
    input: &serde_json::Value,
    term_cols: u16,
) -> Vec<StyledLine> {
    let ts = Instant::now();
    const PREFIX_WIDTH: usize = 13;
    let content_width = (term_cols as usize).saturating_sub(PREFIX_WIDTH).max(30);

    match tool_name {
        "edit_file" => {
            let old = input
                .get("old_content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new = input
                .get("new_content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if old.is_empty() && new.is_empty() {
                return vec![];
            }

            let diff = similar::TextDiff::from_lines(old, new);
            let mut added = 0usize;
            let mut removed = 0usize;
            for change in diff.iter_all_changes() {
                match change.tag() {
                    similar::ChangeTag::Insert => added += 1,
                    similar::ChangeTag::Delete => removed += 1,
                    similar::ChangeTag::Equal => {}
                }
            }
            if added == 0 && removed == 0 {
                return vec![];
            }

            let summary = match (added, removed) {
                (a, 0) => format!("  ⎿  Added {} line{}", a, if a == 1 { "" } else { "s" }),
                (0, r) => format!("  ⎿  Removed {} line{}", r, if r == 1 { "" } else { "s" }),
                (a, r) => format!(
                    "  ⎿  Added {} line{}, removed {} line{}",
                    a,
                    if a == 1 { "" } else { "s" },
                    r,
                    if r == 1 { "" } else { "s" }
                ),
            };

            let mut out = vec![StyledLine {
                text: summary,
                kind: LineKind::ToolResult,
                timestamp: ts,
                agent: None,
            }];

            const CONTEXT_LINES: usize = 3;
            const MAX_CHANGED_LINES: usize = 30;
            let mut changed_shown = 0usize;
            let mut capped = false;

            'outer: for group in diff.grouped_ops(CONTEXT_LINES) {
                for op in &group {
                    for change in diff.iter_changes(op) {
                        let is_changed = change.tag() != similar::ChangeTag::Equal;
                        if is_changed {
                            if changed_shown >= MAX_CHANGED_LINES {
                                let remaining = added + removed - changed_shown;
                                out.push(StyledLine {
                                    text: format!(
                                        "      … {} more change{}",
                                        remaining,
                                        if remaining == 1 { "" } else { "s" }
                                    ),
                                    kind: LineKind::System,
                                    timestamp: ts,
                                    agent: None,
                                });
                                capped = true;
                                break 'outer;
                            }
                            changed_shown += 1;
                        }

                        let (lineno, marker, kind) = match change.tag() {
                            similar::ChangeTag::Delete => {
                                (change.old_index().map(|i| i + 1), '-', LineKind::DiffRemove)
                            }
                            similar::ChangeTag::Insert => {
                                (change.new_index().map(|i| i + 1), '+', LineKind::DiffAdd)
                            }
                            similar::ChangeTag::Equal => (
                                change.old_index().map(|i| i + 1),
                                ' ',
                                LineKind::DiffContext,
                            ),
                        };

                        let lineno_str = lineno
                            .map(|n| format!("{n:>4}"))
                            .unwrap_or_else(|| "    ".to_string());
                        let content = change.value().trim_end_matches('\n');

                        for (i, chunk) in
                            diff_line_chunks(content, content_width).iter().enumerate()
                        {
                            let text = if i == 0 {
                                format!("      {lineno_str} {marker} {chunk}")
                            } else {
                                format!("           {marker} {chunk}")
                            };
                            out.push(StyledLine {
                                text,
                                kind,
                                timestamp: ts,
                                agent: None,
                            });
                        }
                    }
                }
            }
            let _ = capped;
            out
        }

        "write_file" => {
            let content = input.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let lines: Vec<&str> = content.lines().collect();
            let count = lines.len();
            if count == 0 {
                return vec![];
            }

            let mut out = vec![StyledLine {
                text: format!(
                    "  ⎿  Added {} line{}",
                    count,
                    if count == 1 { "" } else { "s" }
                ),
                kind: LineKind::ToolResult,
                timestamp: ts,
                agent: None,
            }];

            const PREVIEW: usize = 5;
            for (i, line) in lines.iter().take(PREVIEW).enumerate() {
                out.push(StyledLine {
                    text: format!("      {:>4} + {line}", i + 1),
                    kind: LineKind::DiffAdd,
                    timestamp: ts,
                    agent: None,
                });
            }
            if count > PREVIEW {
                out.push(StyledLine {
                    text: format!("       … {} more lines", count - PREVIEW),
                    kind: LineKind::ToolResult,
                    timestamp: ts,
                    agent: None,
                });
            }
            out
        }

        _ => vec![],
    }
}

// ── Tool call formatting ──────────────────────────────────────────────────────

/// Format `◆ ToolName(arg)` for the conversation stream.
pub fn format_tool_call_inline(
    tool_name: &str,
    input: &serde_json::Value,
    max_len: usize,
) -> String {
    let pascal = to_pascal_case(tool_name);
    let arg = match input {
        serde_json::Value::Object(map) => {
            let preferred = ["path", "command", "pattern", "url", "query", "content"];
            let val = preferred
                .iter()
                .find_map(|k| map.get(*k))
                .or_else(|| map.values().next());
            match val {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            }
        }
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if arg.is_empty() {
        format!("◆ {pascal}()")
    } else {
        let budget = max_len.saturating_sub(pascal.len() + 4);
        let truncated = if arg.len() > budget && budget > 2 {
            format!("{}…", &arg[..budget.saturating_sub(1)])
        } else {
            arg
        };
        format!("◆ {pascal}({truncated})")
    }
}

/// First key=value preview of a JSON input, truncated to `max_len`.
pub fn input_preview(input: &serde_json::Value, max_len: usize) -> String {
    let s = match input {
        serde_json::Value::Object(map) => {
            if let Some((k, v)) = map.iter().next() {
                let val = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                format!("{k}={val}")
            } else {
                "{}".into()
            }
        }
        other => other.to_string(),
    };
    if s.len() > max_len {
        format!("{}…", &s[..max_len.saturating_sub(1)])
    } else {
        s
    }
}

/// `snake_case` → `PascalCase`.
pub fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut c = word.chars();
            match c.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect()
}

/// Split `text` into chunks of at most `width` chars.
fn diff_line_chunks(text: &str, width: usize) -> Vec<String> {
    if width == 0 || text.is_empty() {
        return vec![text.to_string()];
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        return vec![text.to_string()];
    }
    chars.chunks(width).map(|c| c.iter().collect()).collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_pascal_case_converts() {
        assert_eq!(to_pascal_case("list_files"), "ListFiles");
        assert_eq!(to_pascal_case("edit_file"), "EditFile");
        assert_eq!(to_pascal_case("bash_exec"), "BashExec");
        assert_eq!(to_pascal_case("grep"), "Grep");
    }

    #[test]
    fn input_preview_truncates() {
        let val = serde_json::json!({"path": "a".repeat(100)});
        let preview = input_preview(&val, 20);
        assert!(preview.chars().count() <= 22);
    }

    #[test]
    fn format_tool_call_no_args() {
        let call = format_tool_call_inline("list_files", &serde_json::json!({}), 60);
        assert!(call.contains("ListFiles()"));
    }

    #[test]
    fn build_file_diff_edit_returns_lines() {
        let input = serde_json::json!({
            "path": "src/main.py",
            "old_content": "import random\n",
            "new_content": "import secrets\n"
        });
        let lines = build_file_diff_lines("edit_file", &input, 120);
        assert!(!lines.is_empty());
        let has_summary = lines
            .iter()
            .any(|l| l.kind == LineKind::ToolResult && l.text.contains("⎿"));
        let has_remove = lines.iter().any(|l| l.kind == LineKind::DiffRemove);
        let has_add = lines.iter().any(|l| l.kind == LineKind::DiffAdd);
        assert!(has_summary, "no summary line");
        assert!(has_remove, "no remove line");
        assert!(has_add, "no add line");
    }

    #[test]
    fn build_file_diff_write_returns_summary() {
        let input = serde_json::json!({
            "path": "src/new.py",
            "content": "line1\nline2\nline3\n"
        });
        let lines = build_file_diff_lines("write_file", &input, 120);
        let has_summary = lines
            .iter()
            .any(|l| l.text.contains("⎿") && l.text.contains("3"));
        assert!(has_summary);
    }

    #[test]
    fn build_file_diff_caps_changed_lines() {
        let old: String = (0..40).map(|i| format!("old line {i}\n")).collect();
        let new: String = (0..40).map(|i| format!("new line {i}\n")).collect();
        let input = serde_json::json!({
            "path": "big.py",
            "old_content": old,
            "new_content": new
        });
        let lines = build_file_diff_lines("edit_file", &input, 120);
        let changed = lines
            .iter()
            .filter(|l| l.kind == LineKind::DiffAdd || l.kind == LineKind::DiffRemove)
            .count();
        assert!(
            changed <= 30,
            "changed lines must be capped at 30, got {changed}"
        );
    }
}
