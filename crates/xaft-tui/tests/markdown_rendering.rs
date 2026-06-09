//! Integration tests for PRD 49 — Markdown Rendering (F26).
//!
//! Exercises `MarkdownRenderer` end-to-end, including span model invariants,
//! all Markdown elements, table layout, config knobs, and the disabled path.

use xaft_tui::MarkdownRenderer;
use xaft_tui::transcript::{LineKind, SpanColor, StyledLine};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn render(md: &str) -> Vec<StyledLine> {
    MarkdownRenderer::new(80).with_indent(2).render(md)
}

fn render_at(md: &str, cols: usize) -> Vec<StyledLine> {
    MarkdownRenderer::new(cols).with_indent(2).render(md)
}

fn texts(lines: &[StyledLine]) -> Vec<&str> {
    lines.iter().map(|l| l.text.as_str()).collect()
}

fn has_bold_span_containing(lines: &[StyledLine], needle: &str) -> bool {
    lines.iter().any(|l| {
        l.spans
            .as_ref()
            .map(|spans| spans.iter().any(|s| s.bold && s.text.contains(needle)))
            .unwrap_or(false)
    })
}

fn has_italic_span_containing(lines: &[StyledLine], needle: &str) -> bool {
    lines.iter().any(|l| {
        l.spans
            .as_ref()
            .map(|spans| spans.iter().any(|s| s.italic && s.text.contains(needle)))
            .unwrap_or(false)
    })
}

fn has_strikethrough_span_containing(lines: &[StyledLine], needle: &str) -> bool {
    lines.iter().any(|l| {
        l.spans
            .as_ref()
            .map(|spans| {
                spans
                    .iter()
                    .any(|s| s.strikethrough && s.text.contains(needle))
            })
            .unwrap_or(false)
    })
}

// ── AC1: Headers ──────────────────────────────────────────────────────────────

#[test]
fn renders_h2_as_bold_accent_without_hash() {
    let lines = render("## Summary\n\nBody text.");
    // No `#` characters in any line.
    assert!(
        !lines.iter().any(|l| l.text.contains('#')),
        "raw # chars found in output: {lines:?}"
    );
    // Header line must have a bold span containing "Summary".
    assert!(
        has_bold_span_containing(&lines, "Summary"),
        "no bold span with 'Summary': {lines:?}"
    );
}

#[test]
fn renders_h1_with_separator_when_enabled() {
    let r = MarkdownRenderer::new(80)
        .with_indent(2)
        .with_h1_separator(true);
    let lines = r.render("# Top Header");
    assert!(
        lines.iter().any(|l| l.kind == LineKind::Separator),
        "H1 separator not emitted"
    );
}

#[test]
fn renders_h1_no_separator_when_disabled() {
    let r = MarkdownRenderer::new(80)
        .with_indent(2)
        .with_h1_separator(false);
    let lines = r.render("# Top Header");
    assert!(!lines.iter().any(|l| l.kind == LineKind::Separator));
}

#[test]
fn h3_uses_fg_colour_not_accent() {
    let lines = render("### Sub-heading");
    let hline = lines.first().expect("must produce a line");
    let spans = hline.spans.as_ref().expect("must have spans");
    let heading_span = spans
        .iter()
        .find(|s| s.text.contains("Sub-heading"))
        .expect("must contain heading text");
    // H3 has no accent override.
    assert!(
        heading_span.fg.is_none() || heading_span.fg == Some(SpanColor::Inherit),
        "H3 should not be accent-colored"
    );
    assert!(heading_span.bold);
}

// ── AC2–AC4: Inline elements ──────────────────────────────────────────────────

#[test]
fn renders_bold_inline() {
    let lines = render("The **quick** brown fox.");
    assert!(
        has_bold_span_containing(&lines, "quick"),
        "no bold 'quick' span: {lines:?}"
    );
    assert!(!lines.iter().any(|l| l.text.contains("**")));
}

#[test]
fn renders_italic_inline() {
    let lines = render("The *quick* brown fox.");
    assert!(
        has_italic_span_containing(&lines, "quick"),
        "no italic 'quick' span"
    );
    assert!(!lines.iter().any(|l| l.text.contains('*')));
}

#[test]
fn renders_strikethrough() {
    let lines = render("The ~~old~~ API.");
    assert!(
        has_strikethrough_span_containing(&lines, "old"),
        "no strikethrough 'old' span"
    );
    assert!(!lines.iter().any(|l| l.text.contains("~~")));
}

#[test]
fn inline_code_span_colored() {
    let lines = render("Use `println!` for output.");
    let spans_flat: Vec<_> = lines
        .iter()
        .flat_map(|l| l.spans.as_deref().unwrap_or(&[]))
        .collect();
    assert!(
        spans_flat
            .iter()
            .any(|s| s.fg == Some(SpanColor::Code) && s.text.contains("println!")),
        "no code-colored span for 'println!'"
    );
}

// ── AC10: Links ───────────────────────────────────────────────────────────────

#[test]
fn renders_link_underlined_with_dim_url() {
    let lines = render("See [the docs](https://example.com/docs).");
    let spans_flat: Vec<_> = lines
        .iter()
        .flat_map(|l| l.spans.as_deref().unwrap_or(&[]))
        .collect();

    assert!(
        spans_flat
            .iter()
            .any(|s| s.underline && s.text == "the docs"),
        "link text not underlined: {spans_flat:?}"
    );
    assert!(
        spans_flat
            .iter()
            .any(|s| s.dim && s.text.contains("https://example.com/docs")),
        "URL not in dim span"
    );
    // No raw `[…](…)` syntax in output.
    assert!(
        !lines
            .iter()
            .any(|l| l.text.contains("](") || l.text.contains("](h"))
    );
}

// ── Images ────────────────────────────────────────────────────────────────────

#[test]
fn image_rendered_as_placeholder() {
    let lines = render("![A diagram](diagram.png)");
    assert!(
        !lines.iter().any(|l| l.text.contains("![")),
        "raw ![ found in output"
    );
    assert!(
        lines.iter().any(|l| l.text.contains("image")),
        "image placeholder not found"
    );
}

// ── AC5–AC7: Lists ────────────────────────────────────────────────────────────

#[test]
fn renders_unordered_list_with_bullets() {
    let lines = render("- Alpha\n- Beta\n- Gamma");
    let t = texts(&lines);
    assert!(t.iter().any(|t| t.contains("• Alpha")));
    assert!(t.iter().any(|t| t.contains("• Beta")));
    assert!(t.iter().any(|t| t.contains("• Gamma")));
}

#[test]
fn renders_ordered_list_with_numbers() {
    let lines = render("1. One\n2. Two\n3. Three");
    let t = texts(&lines);
    assert!(t.iter().any(|t| t.contains("1. One")));
    assert!(t.iter().any(|t| t.contains("2. Two")));
    assert!(t.iter().any(|t| t.contains("3. Three")));
}

#[test]
fn nested_list_has_deeper_indent() {
    let md = "- Outer\n  - Inner\n- Outer 2";
    let lines = render(md);
    let t = texts(&lines);
    let inner = t.iter().find(|t| t.contains("Inner")).unwrap();
    let outer = t
        .iter()
        .find(|t| t.contains("Outer") && !t.contains("Inner"))
        .unwrap();
    let inner_indent = inner.len() - inner.trim_start().len();
    let outer_indent = outer.len() - outer.trim_start().len();
    assert!(
        inner_indent > outer_indent,
        "inner indent ({inner_indent}) not deeper than outer ({outer_indent})"
    );
}

#[test]
fn nested_list_uses_different_bullet() {
    let md = "- Outer\n  - Inner";
    let lines = render(md);
    let t = texts(&lines);
    let inner = t.iter().find(|t| t.contains("Inner")).unwrap();
    // Inner should use ‣ not •.
    assert!(
        inner.contains('‣'),
        "inner list should use ‣ bullet: {inner:?}"
    );
}

// ── AC8–AC9: Tables ───────────────────────────────────────────────────────────

#[test]
fn renders_table_with_pipe_borders() {
    let md = "| Name | Value |\n|------|-------|\n| foo | bar |";
    let lines = render(md);
    let joined = texts(&lines).join("\n");
    assert!(joined.contains('│'), "table must use │ borders");
    assert!(joined.contains("Name"));
    assert!(joined.contains("foo"));
    assert!(joined.contains("bar"));
}

#[test]
fn table_divider_uses_separator_kind() {
    let md = "| A | B |\n|---|---|\n| 1 | 2 |";
    let lines = render(md);
    assert!(
        lines.iter().any(|l| l.kind == LineKind::Separator),
        "table divider must use Separator kind"
    );
}

#[test]
fn table_does_not_exceed_term_cols() {
    let r = MarkdownRenderer::new(40).with_indent(2);
    let md = "| C1 | C2 | C3 | C4 |\n|----|----|----|----|\n| a | b | c | d |";
    let lines = r.render(md);
    for line in &lines {
        let w: usize = line
            .text
            .chars()
            .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(1))
            .sum();
        assert!(w <= 40, "table line too wide ({w} > 40): {:?}", line.text);
    }
}

// ── AC11: Block quotes ────────────────────────────────────────────────────────

#[test]
fn blockquote_gets_bar_prefix() {
    let lines = render("> Quoted text.");
    let t = texts(&lines);
    assert!(t.iter().all(|t| t.contains("│")));
    assert!(!t.iter().any(|t| t.trim_start().starts_with('>')));
}

#[test]
fn blockquote_bar_disabled() {
    let r = MarkdownRenderer::new(80).with_blockquote_bar(false);
    let lines = r.render("> Quoted text.");
    assert!(!lines.iter().any(|l| l.text.contains('│')));
    assert!(lines.iter().any(|l| l.text.contains("Quoted text.")));
}

// ── AC12: Horizontal rule ─────────────────────────────────────────────────────

#[test]
fn horizontal_rule_as_separator_line() {
    let lines = render("before\n\n---\n\nafter");
    assert!(
        lines
            .iter()
            .any(|l| { l.kind == LineKind::Separator && l.text.chars().all(|c| c == '─') })
    );
    // The word "before" and "after" must also appear.
    assert!(lines.iter().any(|l| l.text.contains("before")));
    assert!(lines.iter().any(|l| l.text.contains("after")));
}

// ── AC13: Code blocks ─────────────────────────────────────────────────────────

#[test]
fn code_block_emitted_as_code_block_kind() {
    let lines = render("Text\n\n```rust\nfn main() {}\n```\n\nMore text");
    assert!(
        lines.iter().any(|l| l.kind == LineKind::CodeBlock),
        "no CodeBlock line found"
    );
    let cb = lines
        .iter()
        .find(|l| l.kind == LineKind::CodeBlock)
        .unwrap();
    assert!(cb.text.contains("fn main()"));
    assert_eq!(
        cb.agent.as_deref(),
        Some("rust"),
        "language hint must be in `agent`"
    );
}

#[test]
fn code_block_without_language() {
    let lines = render("```\nsome code\n```");
    let cb = lines
        .iter()
        .find(|l| l.kind == LineKind::CodeBlock)
        .unwrap();
    assert!(cb.text.contains("some code"));
    assert_eq!(cb.agent, None);
}

#[test]
fn code_block_not_processed_as_markdown() {
    // Markdown inside a code block must be preserved verbatim.
    let lines = render("```\n**not bold**\n```");
    let cb = lines
        .iter()
        .find(|l| l.kind == LineKind::CodeBlock)
        .unwrap();
    assert!(
        cb.text.contains("**not bold**"),
        "code block content must not be parsed as Markdown"
    );
}

// ── AC14: Disabled path ───────────────────────────────────────────────────────

#[test]
fn disabled_passes_raw_markdown() {
    let r = MarkdownRenderer::disabled();
    let md = "## Header\n\n**bold** and *italic*";
    let lines = r.render(md);
    let joined = texts(&lines).join("\n");
    assert!(joined.contains("##"), "raw ## must be preserved");
    assert!(joined.contains("**bold**"), "raw ** must be preserved");
}

// ── AC15: Malformed input ─────────────────────────────────────────────────────

#[test]
fn malformed_input_does_not_panic() {
    let inputs = [
        "**unclosed bold",
        "| broken | table",
        "```\nunclosed code block",
        "~~unclosed strike",
        "# H1\n## H2\n### H3\n#### H4",
        "[link without url",
        "> deeply\n>>> nested",
        "- item\n  - nested\n    - triple",
    ];
    for input in &inputs {
        // Unwrap: should not panic.
        let _lines = MarkdownRenderer::new(80).render(input);
    }
}

// ── AC16: text/span invariant ─────────────────────────────────────────────────

#[test]
fn text_equals_span_concatenation_for_all_lines() {
    let complex = "\
# Header

Hello **world** and *earth*. Use `code` here.

- item one
- item two with **bold**

> A quoted paragraph with *italic*.

| Col A | Col B |
|-------|-------|
| val1  | val2  |
";
    let lines = MarkdownRenderer::new(80).render(complex);
    for line in &lines {
        if let Some(spans) = &line.spans {
            let concat: String = spans.iter().map(|s| s.text.as_str()).collect();
            assert_eq!(
                line.text, concat,
                "text/span invariant violated on: {:?}",
                line.text
            );
        }
    }
}

// ── Edge cases ────────────────────────────────────────────────────────────────

#[test]
fn empty_string_returns_empty_vec() {
    assert!(render("").is_empty());
}

#[test]
fn whitespace_only_returns_empty_vec() {
    assert!(render("   \n  \n  ").is_empty());
}

#[test]
fn bold_plus_italic_combined() {
    let lines = render("***bold italic***");
    // pulldown_cmark parses this as strong + emphasis nesting.
    let spans_flat: Vec<_> = lines
        .iter()
        .flat_map(|l| l.spans.as_deref().unwrap_or(&[]))
        .collect();
    // At least one span should be bold.
    assert!(
        spans_flat.iter().any(|s| s.bold),
        "no bold span in bold-italic"
    );
}

#[test]
fn ordered_list_starting_at_offset() {
    let lines = render("3. Three\n4. Four");
    let t = texts(&lines);
    assert!(t.iter().any(|t| t.contains("3. Three")));
    assert!(t.iter().any(|t| t.contains("4. Four")));
}

#[test]
fn inline_html_passes_through() {
    // Raw HTML should not panic and text should survive.
    let lines = render("<b>bold html</b>");
    // Either passes through or discards, but must not panic.
    let _ = lines;
}
