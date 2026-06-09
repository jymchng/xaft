//! Markdown and code-block rendering subsystem.

#[cfg(feature = "markdown")]
pub mod markdown;

#[cfg(feature = "markdown")]
pub use markdown::MarkdownRenderer;

/// A no-op renderer returned when the `markdown` feature is disabled.
#[cfg(not(feature = "markdown"))]
pub struct MarkdownRenderer {
    enabled: bool,
}

#[cfg(not(feature = "markdown"))]
impl MarkdownRenderer {
    pub fn new(_term_cols: usize) -> Self {
        Self { enabled: false }
    }
    pub fn disabled() -> Self {
        Self { enabled: false }
    }
    pub fn with_indent(self, _n: usize) -> Self {
        self
    }
    pub fn render(&self, text: &str) -> Vec<crate::transcript::StyledLine> {
        use crate::transcript::{LineKind, StyledLine};
        text.lines()
            .map(|l| StyledLine::new(l, LineKind::AgentText))
            .collect()
    }
}
