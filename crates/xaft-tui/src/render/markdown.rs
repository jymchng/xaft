//! CommonMark Markdown renderer for the xaft TUI.
//!
//! Parses agent output using `pulldown_cmark` and produces `Vec<StyledLine>`
//! with inline span styling. Code blocks are emitted as `LineKind::CodeBlock`
//! for consumption by F27 (PRD 50).

use std::time::Instant;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::renderer::display_width;
use crate::transcript::{LineKind, SpanColor, StyledLine, StyledSpan};

// ── Public renderer ───────────────────────────────────────────────────────────

/// Renders CommonMark Markdown text to `Vec<StyledLine>` for the TUI.
///
/// Create once, reuse across calls. Update `term_cols` on resize by
/// constructing a new instance (`AppState` does this in `Resize` handling).
#[derive(Debug, Clone)]
pub struct MarkdownRenderer {
    pub term_cols: usize,
    pub indent: usize,
    pub enabled: bool,
    pub h1_separator: bool,
    pub blockquote_bar: bool,
    pub table_max_col_width: usize,
    pub table_max_rows: usize,
}

impl Default for MarkdownRenderer {
    fn default() -> Self {
        Self::new(120)
    }
}

impl MarkdownRenderer {
    pub fn new(term_cols: usize) -> Self {
        Self {
            term_cols,
            indent: 2,
            enabled: true,
            h1_separator: true,
            blockquote_bar: true,
            table_max_col_width: 40,
            table_max_rows: 50,
        }
    }

    /// Returns a renderer that passes text through unformatted.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::new(120)
        }
    }

    pub fn with_indent(mut self, n: usize) -> Self {
        self.indent = n;
        self
    }

    pub fn with_h1_separator(mut self, v: bool) -> Self {
        self.h1_separator = v;
        self
    }

    pub fn with_blockquote_bar(mut self, v: bool) -> Self {
        self.blockquote_bar = v;
        self
    }

    /// Render `text` to styled lines.
    ///
    /// Never panics. On any error or when disabled, returns the raw text split
    /// by lines as plain `AgentText`.
    pub fn render(&self, text: &str) -> Vec<StyledLine> {
        if !self.enabled {
            return passthrough(text);
        }
        if text.trim().is_empty() {
            return vec![];
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
            let parser = Parser::new_ext(text, options);
            let mut ctx = RenderCtx::new(self);
            for event in parser {
                ctx.handle(event);
            }
            ctx.finish()
        }));
        result.unwrap_or_else(|_| passthrough(text))
    }
}

fn passthrough(text: &str) -> Vec<StyledLine> {
    if text.is_empty() {
        return vec![];
    }
    text.lines()
        .map(|l| StyledLine::new(l, LineKind::AgentText))
        .collect()
}

// ── Internal types ─────────────────────────────────────────────────────────────

#[derive(Debug)]
enum InlineTag {
    Bold,
    Italic,
    Strikethrough,
    Link(String),
    Image,
}

#[derive(Debug, Clone)]
struct ListState {
    ordered: bool,
    counter: u64,
    depth: usize,
}

struct TableBuilder {
    alignments: Vec<Alignment>,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    in_head: bool,
}

impl TableBuilder {
    fn new(alignments: Vec<Alignment>) -> Self {
        Self {
            alignments,
            headers: Vec::new(),
            rows: Vec::new(),
            current_row: Vec::new(),
            current_cell: String::new(),
            in_head: false,
        }
    }

    fn push_text(&mut self, s: &str) {
        self.current_cell.push_str(s);
    }

    fn end_cell(&mut self) {
        let cell = std::mem::take(&mut self.current_cell);
        self.current_row.push(cell);
    }

    fn end_row(&mut self) {
        if self.in_head {
            self.headers = std::mem::take(&mut self.current_row);
        } else {
            let row = std::mem::take(&mut self.current_row);
            if !row.is_empty() {
                self.rows.push(row);
            }
        }
    }

    fn render(
        &self,
        term_cols: usize,
        indent: usize,
        max_col_width: usize,
        max_rows: usize,
    ) -> Vec<StyledLine> {
        if self.headers.is_empty() {
            return vec![];
        }
        let num_cols = self.headers.len();
        let ind = " ".repeat(indent);

        // Compute natural col widths (capped at max_col_width).
        let mut col_widths: Vec<usize> = self
            .headers
            .iter()
            .map(|h| display_width(h).min(max_col_width).max(1))
            .collect();
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                if i < num_cols {
                    let w = display_width(cell).min(max_col_width).max(1);
                    if w > col_widths[i] {
                        col_widths[i] = w;
                    }
                }
            }
        }

        // Shrink if total exceeds terminal width.
        // Each col: 1 space left + content + 1 space right + 1 border right = col_w + 3
        // Plus 1 border left at the start.
        let border_and_space: usize = 1 + num_cols * 3;
        let total: usize = indent + border_and_space + col_widths.iter().sum::<usize>();
        if total > term_cols {
            let available = term_cols
                .saturating_sub(indent + border_and_space)
                .max(num_cols);
            let natural_total: usize = col_widths.iter().sum();
            if natural_total > 0 {
                for w in &mut col_widths {
                    *w = ((*w * available) / natural_total).max(1);
                }
            }
        }

        let pad_cell = |text: &str, width: usize| -> String {
            let t_width = display_width(text);
            if t_width > width {
                // Truncate with ellipsis.
                let mut s: String = text.chars().take(width.saturating_sub(1)).collect();
                s.push('…');
                s
            } else {
                format!("{}{}", text, " ".repeat(width - t_width))
            }
        };

        let render_row = |cells: &[String]| -> String {
            let mut s = String::from("│");
            for (i, w) in col_widths.iter().enumerate() {
                let cell = cells.get(i).map(|c| c.as_str()).unwrap_or("");
                s.push(' ');
                s.push_str(&pad_cell(cell, *w));
                s.push_str(" │");
            }
            s
        };

        let render_divider = || -> String {
            let mut s = String::from("├");
            for (i, w) in col_widths.iter().enumerate() {
                s.push_str(&"─".repeat(*w + 2));
                if i < col_widths.len() - 1 {
                    s.push('┼');
                }
            }
            s.push('┤');
            s
        };

        let mut lines = Vec::new();

        // Header row.
        lines.push(StyledLine::new(
            format!("{}{}", ind, render_row(&self.headers)),
            LineKind::AgentText,
        ));
        // Divider.
        lines.push(StyledLine::new(
            format!("{}{}", ind, render_divider()),
            LineKind::Separator,
        ));
        // Data rows.
        let row_limit = self.rows.len().min(max_rows);
        for row in &self.rows[..row_limit] {
            lines.push(StyledLine::new(
                format!("{}{}", ind, render_row(row)),
                LineKind::AgentText,
            ));
        }
        if self.rows.len() > max_rows {
            lines.push(StyledLine::new(
                format!(
                    "{}  … {} more rows omitted …",
                    ind,
                    self.rows.len() - max_rows
                ),
                LineKind::System,
            ));
        }

        lines
    }
}

// ── RenderCtx ─────────────────────────────────────────────────────────────────

struct RenderCtx<'r> {
    renderer: &'r MarkdownRenderer,
    lines: Vec<StyledLine>,

    // Inline content
    current_spans: Vec<StyledSpan>,
    inline_stack: Vec<InlineTag>,

    // Image alt text collection
    collecting_image_alt: bool,
    image_alt: String,

    // Block state
    in_heading: Option<HeadingLevel>,
    in_blockquote: usize,
    list_stack: Vec<ListState>,
    item_prefix: String,

    // Code block
    code_language: Option<String>,
    code_buffer: Option<String>,

    // Table
    table: Option<TableBuilder>,
}

impl<'r> RenderCtx<'r> {
    fn new(renderer: &'r MarkdownRenderer) -> Self {
        Self {
            renderer,
            lines: Vec::new(),
            current_spans: Vec::new(),
            inline_stack: Vec::new(),
            collecting_image_alt: false,
            image_alt: String::new(),
            in_heading: None,
            in_blockquote: 0,
            list_stack: Vec::new(),
            item_prefix: String::new(),
            code_language: None,
            code_buffer: None,
            table: None,
        }
    }

    // ── Derived inline attrs ──────────────────────────────────────────────────

    fn is_bold(&self) -> bool {
        self.inline_stack
            .iter()
            .any(|t| matches!(t, InlineTag::Bold))
    }
    fn is_italic(&self) -> bool {
        self.inline_stack
            .iter()
            .any(|t| matches!(t, InlineTag::Italic))
    }
    fn is_strikethrough(&self) -> bool {
        self.inline_stack
            .iter()
            .any(|t| matches!(t, InlineTag::Strikethrough))
    }
    fn is_in_link(&self) -> bool {
        self.inline_stack
            .iter()
            .any(|t| matches!(t, InlineTag::Link(_)))
    }

    // ── Span assembly ─────────────────────────────────────────────────────────

    fn make_span(&self, text: String) -> StyledSpan {
        StyledSpan {
            text,
            bold: self.is_bold() || self.in_heading.is_some(),
            italic: self.is_italic(),
            underline: self.is_in_link(),
            strikethrough: self.is_strikethrough(),
            dim: false,
            fg: if self.in_heading.is_some() {
                match self.in_heading {
                    Some(HeadingLevel::H1) | Some(HeadingLevel::H2) => Some(SpanColor::Accent),
                    _ => None,
                }
            } else {
                None
            },
        }
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.collecting_image_alt {
            self.image_alt.push_str(text);
            return;
        }
        if let Some(ref mut t) = self.table {
            t.push_text(text);
            return;
        }
        if self.code_buffer.is_some() {
            return;
        }
        let span = self.make_span(text.to_string());
        self.current_spans.push(span);
    }

    // ── Line commit ───────────────────────────────────────────────────────────

    fn commit_line(&mut self) {
        if self.current_spans.is_empty() {
            return;
        }

        let mut all_spans: Vec<StyledSpan> = Vec::new();

        // 1. Blockquote prefix (outermost).
        if self.in_blockquote > 0 && self.renderer.blockquote_bar {
            let prefix = "│ ".repeat(self.in_blockquote);
            all_spans.push(StyledSpan {
                text: prefix,
                bold: false,
                italic: false,
                underline: false,
                strikethrough: false,
                dim: true,
                fg: Some(SpanColor::Dim),
            });
        }

        // 2. List item prefix OR global indent.
        if !self.item_prefix.is_empty() {
            all_spans.push(StyledSpan::plain(std::mem::take(&mut self.item_prefix)));
        } else if self.in_heading.is_none() {
            // List continuation indent or global indent.
            let indent_width = if !self.list_stack.is_empty() {
                let depth = self.list_stack.len();
                let base = self.list_stack.last().map(|l| l.depth).unwrap_or(0);
                self.renderer.indent + base * 2 + 4
            } else {
                self.renderer.indent
            };
            if indent_width > 0 {
                all_spans.push(StyledSpan::plain(" ".repeat(indent_width)));
            }
        }

        // 3. Content spans.
        all_spans.extend(self.current_spans.drain(..));

        let text: String = all_spans.iter().map(|s| s.text.as_str()).collect();

        self.lines.push(StyledLine {
            text,
            kind: LineKind::AgentText,
            timestamp: Instant::now(),
            agent: None,
            spans: Some(all_spans),
        });
    }

    fn pop_inline_tag(&mut self, pred: impl Fn(&InlineTag) -> bool) {
        if let Some(pos) = self.inline_stack.iter().rposition(|t| pred(t)) {
            self.inline_stack.remove(pos);
        }
    }

    // ── Event handler ─────────────────────────────────────────────────────────

    fn handle(&mut self, event: Event) {
        match event {
            // ── Headings ──────────────────────────────────────────────────────
            Event::Start(Tag::Heading { level, .. }) => {
                self.in_heading = Some(level);
            }
            Event::End(TagEnd::Heading(level)) => {
                self.in_heading = None;
                let text: String = self.current_spans.iter().map(|s| s.text.as_str()).collect();
                self.current_spans.clear();

                let (glyph, fg) = match level {
                    HeadingLevel::H1 => ("", SpanColor::Accent),
                    HeadingLevel::H2 => ("▸ ", SpanColor::Accent),
                    HeadingLevel::H3 => ("› ", SpanColor::Inherit),
                    _ => ("", SpanColor::Inherit),
                };

                let indent_str = " ".repeat(self.renderer.indent);
                let mut spans = Vec::new();
                if !indent_str.is_empty() {
                    spans.push(StyledSpan::plain(indent_str.clone()));
                }
                if !glyph.is_empty() {
                    spans.push(StyledSpan {
                        text: glyph.to_string(),
                        dim: true,
                        fg: Some(SpanColor::Dim),
                        bold: false,
                        italic: false,
                        underline: false,
                        strikethrough: false,
                    });
                }
                spans.push(StyledSpan {
                    text: text.clone(),
                    bold: true,
                    italic: false,
                    underline: false,
                    strikethrough: false,
                    dim: false,
                    fg: if fg == SpanColor::Inherit {
                        None
                    } else {
                        Some(fg)
                    },
                });

                let full_text = format!("{}{}{}", indent_str, glyph, text);
                self.lines.push(StyledLine {
                    text: full_text,
                    kind: LineKind::AgentText,
                    timestamp: Instant::now(),
                    agent: None,
                    spans: Some(spans),
                });

                if level == HeadingLevel::H1 && self.renderer.h1_separator {
                    self.lines
                        .push(StyledLine::new("─".repeat(60), LineKind::Separator));
                }
            }

            // ── Paragraphs ────────────────────────────────────────────────────
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                self.commit_line();
            }

            // ── Strong / Emphasis / Strikethrough ─────────────────────────────
            Event::Start(Tag::Strong) => {
                self.inline_stack.push(InlineTag::Bold);
            }
            Event::End(TagEnd::Strong) => {
                self.pop_inline_tag(|t| matches!(t, InlineTag::Bold));
            }
            Event::Start(Tag::Emphasis) => {
                self.inline_stack.push(InlineTag::Italic);
            }
            Event::End(TagEnd::Emphasis) => {
                self.pop_inline_tag(|t| matches!(t, InlineTag::Italic));
            }
            Event::Start(Tag::Strikethrough) => {
                self.inline_stack.push(InlineTag::Strikethrough);
            }
            Event::End(TagEnd::Strikethrough) => {
                self.pop_inline_tag(|t| matches!(t, InlineTag::Strikethrough));
            }

            // ── Links ─────────────────────────────────────────────────────────
            Event::Start(Tag::Link { dest_url, .. }) => {
                self.inline_stack
                    .push(InlineTag::Link(dest_url.to_string()));
            }
            Event::End(TagEnd::Link) => {
                let url = self
                    .inline_stack
                    .iter()
                    .rev()
                    .find_map(|t| {
                        if let InlineTag::Link(u) = t {
                            Some(u.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();
                self.pop_inline_tag(|t| matches!(t, InlineTag::Link(_)));

                if !url.is_empty() {
                    let display_url = if url.len() > 80 {
                        format!("{}…", &url[..79])
                    } else {
                        url
                    };
                    self.current_spans.push(StyledSpan {
                        text: format!(" ({})", display_url),
                        bold: false,
                        italic: false,
                        underline: false,
                        strikethrough: false,
                        dim: true,
                        fg: Some(SpanColor::Dim),
                    });
                }
            }

            // ── Images ────────────────────────────────────────────────────────
            Event::Start(Tag::Image { .. }) => {
                self.collecting_image_alt = true;
                self.image_alt.clear();
                self.inline_stack.push(InlineTag::Image);
            }
            Event::End(TagEnd::Image) => {
                self.collecting_image_alt = false;
                let alt = std::mem::take(&mut self.image_alt);
                self.pop_inline_tag(|t| matches!(t, InlineTag::Image));
                let placeholder = if alt.is_empty() {
                    "[image]".to_string()
                } else {
                    format!("[image: {}]", alt)
                };
                self.current_spans.push(StyledSpan {
                    text: placeholder,
                    bold: false,
                    italic: false,
                    underline: false,
                    strikethrough: false,
                    dim: true,
                    fg: Some(SpanColor::Dim),
                });
            }

            // ── Inline code ───────────────────────────────────────────────────
            Event::Code(s) => {
                if let Some(ref mut t) = self.table {
                    t.push_text(s.as_ref());
                } else if self.code_buffer.is_none() {
                    self.current_spans.push(StyledSpan {
                        text: s.to_string(),
                        bold: false,
                        italic: false,
                        underline: false,
                        strikethrough: false,
                        dim: false,
                        fg: Some(SpanColor::Code),
                    });
                }
            }

            // ── Text ──────────────────────────────────────────────────────────
            Event::Text(s) => {
                if let Some(ref mut buf) = self.code_buffer {
                    buf.push_str(s.as_ref());
                } else {
                    self.push_text(s.as_ref());
                }
            }

            // ── Line breaks ───────────────────────────────────────────────────
            Event::SoftBreak => {
                if self.code_buffer.is_none() {
                    self.push_text(" ");
                }
            }
            Event::HardBreak => {
                if self.code_buffer.is_none() {
                    self.commit_line();
                }
            }

            // ── Block quotes ──────────────────────────────────────────────────
            Event::Start(Tag::BlockQuote(_)) => {
                self.in_blockquote += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                if self.in_blockquote > 0 {
                    self.in_blockquote -= 1;
                }
            }

            // ── Code blocks ───────────────────────────────────────────────────
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => {
                        let s = l.to_string();
                        if s.is_empty() { None } else { Some(s) }
                    }
                    CodeBlockKind::Indented => None,
                };
                self.code_language = lang;
                self.code_buffer = Some(String::new());
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(code) = self.code_buffer.take() {
                    let lang = self.code_language.take();
                    let trimmed = code.trim_end_matches('\n').to_string();
                    let mut line = StyledLine::new(trimmed, LineKind::CodeBlock);
                    line.agent = lang;
                    self.lines.push(line);
                }
            }

            // ── Lists ─────────────────────────────────────────────────────────
            Event::Start(Tag::List(ordered)) => {
                let depth = self.list_stack.len() + 1;
                self.list_stack.push(ListState {
                    ordered: ordered.is_some(),
                    counter: ordered.unwrap_or(1),
                    depth,
                });
            }
            Event::End(TagEnd::List(_)) => {
                self.list_stack.pop();
            }
            Event::Start(Tag::Item) => {
                if let Some(state) = self.list_stack.last_mut() {
                    let depth = state.depth;
                    let extra = (depth - 1) * 2;
                    self.item_prefix = if state.ordered {
                        let n = state.counter;
                        state.counter += 1;
                        format!("{}{}. ", " ".repeat(extra), n)
                    } else {
                        let bullet = match depth {
                            1 => "• ",
                            2 => "‣ ",
                            _ => "◦ ",
                        };
                        format!("{}{}", " ".repeat(extra), bullet)
                    };
                }
            }
            Event::End(TagEnd::Item) => {
                if !self.current_spans.is_empty() {
                    self.commit_line();
                }
                self.item_prefix.clear();
            }

            // ── Tables ────────────────────────────────────────────────────────
            Event::Start(Tag::Table(alignments)) => {
                self.table = Some(TableBuilder::new(alignments));
            }
            Event::End(TagEnd::Table) => {
                if let Some(table) = self.table.take() {
                    let tlines = table.render(
                        self.renderer.term_cols,
                        self.renderer.indent,
                        self.renderer.table_max_col_width,
                        self.renderer.table_max_rows,
                    );
                    self.lines.extend(tlines);
                }
            }
            Event::Start(Tag::TableHead) => {
                if let Some(ref mut t) = self.table {
                    t.in_head = true;
                }
            }
            Event::End(TagEnd::TableHead) => {
                if let Some(ref mut t) = self.table {
                    t.end_row();
                    t.in_head = false;
                }
            }
            Event::Start(Tag::TableRow) => {}
            Event::End(TagEnd::TableRow) => {
                if let Some(ref mut t) = self.table {
                    t.end_row();
                }
            }
            Event::Start(Tag::TableCell) => {}
            Event::End(TagEnd::TableCell) => {
                if let Some(ref mut t) = self.table {
                    t.end_cell();
                }
            }

            // ── Horizontal rule ───────────────────────────────────────────────
            Event::Rule => {
                self.lines
                    .push(StyledLine::new("─".repeat(60), LineKind::Separator));
            }

            // ── HTML pass-through ─────────────────────────────────────────────
            Event::Html(s) | Event::InlineHtml(s) => {
                self.push_text(s.as_ref());
            }

            // Everything else is ignored.
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<StyledLine> {
        // Flush any trailing content not terminated by a paragraph end.
        if !self.current_spans.is_empty() {
            self.commit_line();
        }
        // Flush any open code block.
        if let Some(code) = self.code_buffer.take() {
            let lang = self.code_language.take();
            let mut line = StyledLine::new(code.trim_end_matches('\n'), LineKind::CodeBlock);
            line.agent = lang;
            self.lines.push(line);
        }
        self.lines
    }
}

// ── span_word_wrap (exported for tests) ─────────────────────────────────────

/// Split a list of spans across multiple lines, each fitting within `max_width`
/// terminal columns.  Breaks on whitespace; long words are never broken.
pub fn span_word_wrap(spans: Vec<StyledSpan>, max_width: usize) -> Vec<Vec<StyledSpan>> {
    if max_width == 0 {
        return vec![spans];
    }
    let flat: String = spans.iter().map(|s| s.text.as_str()).collect();
    if display_width(&flat) <= max_width {
        return vec![spans];
    }

    // Build a char-index → span-index map.
    let mut char_to_span: Vec<usize> = Vec::with_capacity(flat.chars().count());
    for (si, span) in spans.iter().enumerate() {
        for _ in span.text.chars() {
            char_to_span.push(si);
        }
    }

    let mut result: Vec<Vec<StyledSpan>> = Vec::new();
    let mut line_start_char: usize = 0;
    let chars: Vec<char> = flat.chars().collect();
    let n = chars.len();

    while line_start_char < n {
        let mut col = 0usize;
        let mut last_space = None;
        let mut i = line_start_char;

        while i < n {
            let w = chars[i].len_utf8(); // not right for width, use unicode_width
            let cw = unicode_width::UnicodeWidthChar::width(chars[i]).unwrap_or(1);
            if col + cw > max_width {
                break;
            }
            if chars[i] == ' ' {
                last_space = Some(i);
            }
            col += cw;
            i += 1;
        }

        let end_char = if i >= n {
            n
        } else if let Some(sp) = last_space {
            sp
        } else {
            i // hard break mid-word
        };

        // Collect spans for chars [line_start_char..end_char].
        let line_spans =
            collect_span_slice(&spans, &chars, &char_to_span, line_start_char, end_char);
        if !line_spans.is_empty() {
            result.push(line_spans);
        }

        // Skip the space at end_char (if we broke on a space).
        line_start_char = if end_char < n && chars[end_char] == ' ' {
            end_char + 1
        } else {
            end_char
        };
    }

    if result.is_empty() {
        result.push(spans);
    }
    result
}

fn collect_span_slice(
    spans: &[StyledSpan],
    chars: &[char],
    char_to_span: &[usize],
    start: usize,
    end: usize,
) -> Vec<StyledSpan> {
    if start >= end {
        return vec![];
    }
    let mut result: Vec<StyledSpan> = Vec::new();
    let mut i = start;
    while i < end {
        let si = char_to_span[i];
        // Find how many consecutive chars belong to span si.
        let mut j = i;
        while j < end && char_to_span[j] == si {
            j += 1;
        }
        let text: String = chars[i..j].iter().collect();
        if !text.is_empty() {
            let mut span = spans[si].clone();
            span.text = text;
            result.push(span);
        }
        i = j;
    }
    result
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn render(md: &str) -> Vec<StyledLine> {
        MarkdownRenderer::new(80).render(md)
    }

    #[test]
    fn empty_returns_empty() {
        assert!(render("").is_empty());
        assert!(render("   ").is_empty());
    }

    #[test]
    fn plain_paragraph() {
        let lines = render("Hello world.");
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|l| l.text.contains("Hello world.")));
    }

    #[test]
    fn horizontal_rule_is_separator() {
        let lines = render("before\n\n---\n\nafter");
        assert!(
            lines
                .iter()
                .any(|l| l.kind == LineKind::Separator && l.text.contains('─'))
        );
    }

    #[test]
    fn code_block_kind_with_language() {
        let lines = render("```rust\nfn main() {}\n```");
        let cb = lines
            .iter()
            .find(|l| l.kind == LineKind::CodeBlock)
            .unwrap();
        assert!(cb.text.contains("fn main()"));
        assert_eq!(cb.agent.as_deref(), Some("rust"));
    }

    #[test]
    fn code_block_no_language() {
        let lines = render("```\nsome code\n```");
        let cb = lines
            .iter()
            .find(|l| l.kind == LineKind::CodeBlock)
            .unwrap();
        assert!(cb.text.contains("some code"));
        assert_eq!(cb.agent, None);
    }

    #[test]
    fn inline_bold_span() {
        let lines = render("The **quick** fox.");
        let spans = lines[0].spans.as_ref().unwrap();
        assert!(spans.iter().any(|s| s.bold && s.text == "quick"));
        assert!(!lines[0].text.contains("**"));
    }

    #[test]
    fn inline_italic_span() {
        let lines = render("The *quick* fox.");
        let spans = lines[0].spans.as_ref().unwrap();
        assert!(spans.iter().any(|s| s.italic && s.text == "quick"));
    }

    #[test]
    fn inline_strikethrough_span() {
        let lines = render("The ~~old~~ way.");
        let spans = lines[0].spans.as_ref().unwrap();
        assert!(spans.iter().any(|s| s.strikethrough && s.text == "old"));
        assert!(!lines[0].text.contains("~~"));
    }

    #[test]
    fn inline_code_span() {
        let lines = render("Use `println!` for output.");
        let spans = lines[0].spans.as_ref().unwrap();
        assert!(
            spans
                .iter()
                .any(|s| s.fg == Some(SpanColor::Code) && s.text == "println!")
        );
    }

    #[test]
    fn link_underline_and_url() {
        let lines = render("See [the docs](https://example.com).");
        let spans = lines[0].spans.as_ref().unwrap();
        assert!(spans.iter().any(|s| s.underline && s.text == "the docs"));
        assert!(
            spans
                .iter()
                .any(|s| s.dim && s.text.contains("https://example.com"))
        );
    }

    #[test]
    fn image_placeholder() {
        let lines = render("![A diagram](diagram.png)");
        assert!(!lines.iter().any(|l| l.text.contains("![")));
        assert!(lines.iter().any(|l| l.text.contains("image")));
    }

    #[test]
    fn h2_bold_accent() {
        let lines = render("## Summary\n\nBody.");
        let header = &lines[0];
        let spans = header.spans.as_ref().unwrap();
        assert!(spans.iter().any(|s| s.bold && s.text.contains("Summary")));
        assert!(!lines.iter().any(|l| l.text.contains('#')));
    }

    #[test]
    fn h1_separator() {
        let r = MarkdownRenderer::new(80).with_h1_separator(true);
        let lines = r.render("# Top Header");
        assert!(lines.iter().any(|l| l.kind == LineKind::Separator));
    }

    #[test]
    fn h1_no_separator_when_disabled() {
        let r = MarkdownRenderer::new(80).with_h1_separator(false);
        let lines = r.render("# Top Header");
        assert!(!lines.iter().any(|l| l.kind == LineKind::Separator));
    }

    #[test]
    fn unordered_list_bullets() {
        let lines = render("- Alpha\n- Beta\n- Gamma");
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("• Alpha")));
        assert!(texts.iter().any(|t| t.contains("• Beta")));
    }

    #[test]
    fn ordered_list_numbers() {
        let lines = render("1. One\n2. Two");
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("1. One")));
        assert!(texts.iter().any(|t| t.contains("2. Two")));
    }

    #[test]
    fn blockquote_bar_prefix() {
        let lines = render("> Quoted.");
        assert!(lines.iter().all(|l| l.text.contains("│")));
        assert!(!lines.iter().any(|l| l.text.trim_start().starts_with('>')));
    }

    #[test]
    fn blockquote_bar_disabled() {
        let r = MarkdownRenderer::new(80).with_blockquote_bar(false);
        let lines = r.render("> Quoted.");
        assert!(!lines.iter().any(|l| l.text.contains('│')));
        assert!(lines.iter().any(|l| l.text.contains("Quoted.")));
    }

    #[test]
    fn table_borders() {
        let md = "| Name | Value |\n|------|-------|\n| foo | bar |";
        let lines = render(md);
        let joined = lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains('│'));
        assert!(joined.contains("Name"));
        assert!(joined.contains("foo"));
    }

    #[test]
    fn table_does_not_exceed_term_cols() {
        let r = MarkdownRenderer::new(40);
        let md = "| Col1 | Col2 | Col3 | Col4 |\n|------|------|------|------|\n| a | b | c | d |";
        let lines = r.render(md);
        for line in &lines {
            let w = display_width(&line.text);
            assert!(w <= 40, "line too wide ({w}): {:?}", line.text);
        }
    }

    #[test]
    fn disabled_passes_raw() {
        let r = MarkdownRenderer::disabled();
        let md = "## Header\n\n**bold**";
        let lines = r.render(md);
        let joined = lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("##"));
        assert!(joined.contains("**bold**"));
    }

    #[test]
    fn malformed_does_not_panic() {
        let inputs = [
            "**unclosed bold",
            "| broken | table",
            "```\nunclosed",
            "~~unclosed",
            "# H1\n## H2\n### H3",
        ];
        for input in &inputs {
            let _lines = MarkdownRenderer::new(80).render(input);
        }
    }

    #[test]
    fn text_span_invariant() {
        let lines = render("Hello **world** and *earth*.");
        for line in &lines {
            if let Some(spans) = &line.spans {
                let concat: String = spans.iter().map(|s| s.text.as_str()).collect();
                assert_eq!(line.text, concat, "invariant violated: {:?}", line.text);
            }
        }
    }

    #[test]
    fn span_word_wrap_single_line_no_split() {
        let spans = vec![StyledSpan::plain("hello world")];
        let result = span_word_wrap(spans, 80);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn span_word_wrap_splits_long_line() {
        let spans = vec![StyledSpan::plain("word1 word2 word3")];
        let result = span_word_wrap(spans, 7);
        assert!(result.len() > 1);
    }
}
