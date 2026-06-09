//! Lightweight CLI table builder for slash command output.

use crate::transcript::{LineKind, StyledLine};

// ── CliTable ──────────────────────────────────────────────────────────────────

/// A simple table builder for tabular slash command output.
pub struct CliTable {
    headers: Vec<String>,
    rows: Vec<RowKind>,
    col_widths: Vec<usize>,
}

enum RowKind {
    Data(Vec<String>),
    Separator,
}

impl CliTable {
    /// Create a new table with the given header columns.
    pub fn new(headers: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let headers: Vec<String> = headers.into_iter().map(|h| h.into()).collect();
        let col_widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
        Self {
            headers,
            rows: Vec::new(),
            col_widths,
        }
    }

    /// Add a data row.
    pub fn push_row(&mut self, row: impl IntoIterator<Item = impl Into<String>>) {
        let row: Vec<String> = row.into_iter().map(|c| c.into()).collect();
        // Update column widths.
        for (i, cell) in row.iter().enumerate() {
            if i < self.col_widths.len() {
                self.col_widths[i] = self.col_widths[i].max(cell.len());
            }
        }
        self.rows.push(RowKind::Data(row));
    }

    /// Add a horizontal separator row.
    pub fn push_separator(&mut self) {
        self.rows.push(RowKind::Separator);
    }

    /// Render the table to a list of `StyledLine`s.
    ///
    /// Column widths are `max(header.len(), max_value.len()) + 2`.
    pub fn render(&self, cols: u16) -> Vec<StyledLine> {
        let max_width = cols as usize;
        let widths: Vec<usize> = self.col_widths.iter().map(|&w| w + 2).collect();

        let mut lines = Vec::new();

        // Header line.
        let header_text = self.format_row(&self.headers, &widths, max_width);
        lines.push(StyledLine::new(
            format!("  {}", header_text),
            LineKind::AgentText,
        ));

        // Separator under header.
        let sep_len: usize = widths.iter().sum::<usize>().min(max_width);
        lines.push(StyledLine::new(
            format!("  {}", "─".repeat(sep_len)),
            LineKind::System,
        ));

        // Data rows.
        for row_kind in &self.rows {
            match row_kind {
                RowKind::Data(row) => {
                    let row_text = self.format_row(row, &widths, max_width);
                    lines.push(StyledLine::new(format!("  {}", row_text), LineKind::System));
                }
                RowKind::Separator => {
                    let sep_len: usize = widths.iter().sum::<usize>().min(max_width);
                    lines.push(StyledLine::new(
                        format!("  {}", "─".repeat(sep_len)),
                        LineKind::System,
                    ));
                }
            }
        }

        lines
    }

    fn format_row(&self, cells: &[String], widths: &[usize], max_width: usize) -> String {
        let mut s = String::new();
        for (i, cell) in cells.iter().enumerate() {
            let w = widths.get(i).copied().unwrap_or(cell.len() + 2);
            // Pad or truncate cell.
            if cell.len() + 1 < w {
                s.push_str(cell);
                s.push_str(&" ".repeat(w - cell.len()));
            } else {
                // Truncate with ellipsis if cell is too wide.
                let truncated = if cell.len() >= w.saturating_sub(1) && w > 3 {
                    format!("{}…", &cell[..w.saturating_sub(2)])
                } else {
                    cell.clone()
                };
                s.push_str(&truncated);
                s.push(' ');
            }
            if s.len() >= max_width {
                s.truncate(max_width);
                break;
            }
        }
        s.trim_end().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_renders_header_and_rows() {
        let mut t = CliTable::new(["NAME", "VALUE"]);
        t.push_row(["foo", "bar"]);
        let lines = t.render(80);
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("NAME")));
        assert!(texts.iter().any(|t| t.contains("foo")));
    }

    #[test]
    fn table_separator_row() {
        let mut t = CliTable::new(["A", "B"]);
        t.push_row(["x", "y"]);
        t.push_separator();
        t.push_row(["TOTAL", "99"]);
        let lines = t.render(80);
        // Should have at least 5 lines: header, sep, data, separator, total
        assert!(lines.len() >= 5);
    }
}
