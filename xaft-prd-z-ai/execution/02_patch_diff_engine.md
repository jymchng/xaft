# XAFT Patch & Diff Engine — PRD

> Document ID: XAFT-EXEC-002
> Version: 0.1.0-draft
> Status: Design Phase
> Owner: xaft-core team

---

## 1. Overview

The Patch & Diff Engine is responsible for transforming the agent's edit intent into verified, reviewable, and reversible file modifications. The pipeline flows from `FileEditor` → `EditReceipt` → unified diff (via `similar` crate) → TUI rendering. This document covers the complete diff pipeline, `DiffFormat` configuration, unified diff computation, `apply_diff` with fuzzy context matching, the fuzzy matching algorithm, and patch generation for Git.

---

## 2. Architecture

```
 ┌──────────────────────────────────────────────────────────────────────────┐
 │                       Patch & Diff Engine Pipeline                       │
 │                                                                          │
 │  ┌────────────┐   ┌────────────┐   ┌───────────────┐   ┌─────────────┐ │
 │  │ FileEditor  │──▶│ EditReceipt│──▶│ compute_      │──▶│ unified     │ │
 │  │ (replace_   │   │ (before+   │   │ unified_diff  │   │ diff output │ │
 │  │  block/     │   │  after+    │   │ (similar      │   │             │ │
 │  │  apply_diff/│   │  metadata) │   │  crate)       │   │             │ │
 │  │  multi_edit)│   │            │   │               │   │             │ │
 │  └────────────┘   └─────┬──────┘   └───────┬───────┘   └──────┬──────┘ │
 │                         │                   │                   │        │
 │                         │                   ▼                   ▼        │
 │                         │           ┌───────────────┐   ┌─────────────┐ │
 │                         │           │ apply_diff    │   │ TUI Render  │ │
 │                         │           │ (fuzzy match  │   │ (colorized  │ │
 │                         │           │  0.85 θ)      │   │  side-by-   │ │
 │                         │           └───────┬───────┘   │  side)      │ │
 │                         │                   │           └─────────────┘ │
 │                         │                   ▼                           │
 │                         │           ┌───────────────┐                   │
 │                         └──────────▶│ Git Patch     │                   │
 │                                     │ Generation    │                   │
 │                                     └───────────────┘                   │
 └──────────────────────────────────────────────────────────────────────────┘
```

---

## 3. FileEditor

### 3.1 Interface

`FileEditor` is the agent's primary tool for modifying files. It supports four edit modes, each producing an `EditReceipt`.

```rust
/// The primary file editing interface for the agent.
pub struct FileEditor {
    repo_root: PathBuf,
    diff_config: DiffFormat,
    fuzzy_threshold: f64, // default: 0.85
}

/// Receipt returned after every edit, capturing before/after state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditReceipt {
    /// Unique identifier for this edit.
    pub id: EditId,
    /// File path relative to repo root.
    pub path: PathBuf,
    /// Content before the edit (full file snapshot).
    pub before: String,
    /// Content after the edit (full file snapshot).
    pub after: String,
    /// The edit operation that was applied.
    pub operation: EditOperation,
    /// Timestamp of the edit.
    pub timestamp: DateTime<Utc>,
    /// Computed unified diff (populated lazily or eagerly).
    pub diff: Option<UnifiedDiff>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EditOperation {
    ReplaceBlock {
        line_start: usize,
        line_end: usize,
        old_content: String,
        new_content: String,
    },
    ApplyDiff {
        hunks: Vec<HunkDescriptor>,
    },
    MultiEdit {
        edits: Vec<SingleEdit>,
    },
    FullWrite {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HunkDescriptor {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub context_before: Vec<String>,
    pub removals: Vec<String>,
    pub additions: Vec<String>,
    pub context_after: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SingleEdit {
    pub line_start: usize,
    pub line_end: usize,
    pub new_content: String,
}
```

### 3.2 replace_block

Replace a contiguous block of lines identified by line numbers.

```rust
impl FileEditor {
    /// Replace lines [line_start, line_end) with new_content.
    /// Returns an EditReceipt with before/after snapshots.
    pub fn replace_block(
        &self,
        path: &Path,
        line_start: usize,
        line_end: usize,
        new_content: &str,
    ) -> Result<EditReceipt, EditError> {
        let full_path = self.repo_root.join(path);
        let before = fs::read_to_string(&full_path)
            .map_err(|e| EditError::FileRead { path: path.to_path_buf(), source: e })?;

        let lines: Vec<&str> = before.lines().collect();
        if line_start > lines.len() || line_end > lines.len() {
            return Err(EditError::LineOutOfRange {
                path: path.to_path_buf(),
                line_start,
                line_end,
                total_lines: lines.len(),
            });
        }

        let old_content: String = lines[line_start..line_end].join("\n");

        // Construct after content
        let mut after_lines: Vec<String> = lines[..line_start]
            .iter()
            .map(|l| l.to_string())
            .collect();
        after_lines.extend(new_content.lines().map(|l| l.to_string()));
        after_lines.extend(
            lines[line_end..]
                .iter()
                .map(|l| l.to_string())
        );
        let after = after_lines.join("\n");

        // Write the modified content
        fs::write(&full_path, &after)
            .map_err(|e| EditError::FileWrite { path: path.to_path_buf(), source: e })?;

        let receipt = EditReceipt {
            id: EditId::new(),
            path: path.to_path_buf(),
            before,
            after,
            operation: EditOperation::ReplaceBlock {
                line_start,
                line_end,
                old_content,
                new_content: new_content.to_string(),
            },
            timestamp: Utc::now(),
            diff: None, // computed lazily
        };

        Ok(receipt)
    }
}
```

### 3.3 apply_diff

Apply a unified-diff-style specification with fuzzy context matching.

```rust
impl FileEditor {
    /// Apply a diff specification with fuzzy context matching.
    /// If the exact context lines don't match, falls back to fuzzy matching
    /// with a similarity threshold (default: 0.85).
    pub fn apply_diff(
        &self,
        path: &Path,
        hunks: &[HunkDescriptor],
    ) -> Result<EditReceipt, EditError> {
        let full_path = self.repo_root.join(path);
        let before = fs::read_to_string(&full_path)?;
        let lines: Vec<&str> = before.lines().collect();

        let mut after_lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();

        // Apply hunks in reverse order to preserve line numbers
        let mut sorted_hunks: Vec<&HunkDescriptor> = hunks.iter().collect();
        sorted_hunks.sort_by(|a, b| b.old_start.cmp(&a.old_start));

        for hunk in sorted_hunks {
            // Try exact match first
            let match_result = self.find_hunk_context(&after_lines, hunk);

            let (start, end) = match match_result {
                HunkMatch::Exact { start, end } => (start, end),
                HunkMatch::Fuzzy { start, end, score } => {
                    if score < self.fuzzy_threshold {
                        return Err(EditError::FuzzyMatchFailed {
                            path: path.to_path_buf(),
                            hunk_old_start: hunk.old_start,
                            score,
                            threshold: self.fuzzy_threshold,
                        });
                    }
                    tracing::info!(
                        "Fuzzy match for hunk at line {}: score={:.3}",
                        hunk.old_start, score
                    );
                    (start, end)
                }
                HunkMatch::None => {
                    return Err(EditError::HunkNotFound {
                        path: path.to_path_buf(),
                        hunk_old_start: hunk.old_start,
                    });
                }
            };

            // Replace the matched region
            let replacement: Vec<String> = hunk.additions
                .iter()
                .chain(hunk.context_after.iter())
                .cloned()
                .collect();

            after_lines.splice(start..end, replacement);
        }

        let after = after_lines.join("\n");
        fs::write(&full_path, &after)?;

        Ok(EditReceipt {
            id: EditId::new(),
            path: path.to_path_buf(),
            before,
            after,
            operation: EditOperation::ApplyDiff {
                hunks: hunks.to_vec(),
            },
            timestamp: Utc::now(),
            diff: None,
        })
    }
}
```

### 3.4 multi_edit

Apply multiple non-overlapping edits to a file in a single operation.

```rust
impl FileEditor {
    /// Apply multiple edits to a single file in one operation.
    /// Edits are specified by line ranges and are applied in reverse order.
    pub fn multi_edit(
        &self,
        path: &Path,
        edits: &[SingleEdit],
    ) -> Result<EditReceipt, EditError> {
        let full_path = self.repo_root.join(path);
        let before = fs::read_to_string(&full_path)?;
        let lines: Vec<&str> = before.lines().collect();

        // Validate: no overlapping ranges
        let mut sorted_edits: Vec<&SingleEdit> = edits.iter().collect();
        sorted_edits.sort_by_key(|e| e.line_start);

        for window in sorted_edits.windows(2) {
            if window[0].line_end > window[1].line_start {
                return Err(EditError::OverlappingEdits {
                    edit_a: (window[0].line_start, window[0].line_end),
                    edit_b: (window[1].line_start, window[1].line_end),
                });
            }
        }

        // Apply edits in reverse order
        let mut after_lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        for edit in sorted_edits.iter().rev() {
            let new_lines: Vec<String> = edit.new_content
                .lines()
                .map(|l| l.to_string())
                .collect();
            after_lines.splice(edit.line_start..edit.line_end, new_lines);
        }

        let after = after_lines.join("\n");
        fs::write(&full_path, &after)?;

        Ok(EditReceipt {
            id: EditId::new(),
            path: path.to_path_buf(),
            before,
            after,
            operation: EditOperation::MultiEdit {
                edits: edits.to_vec(),
            },
            timestamp: Utc::now(),
            diff: None,
        })
    }
}
```

### 3.5 commit and rollback

```rust
impl FileEditor {
    /// Commit all pending edits to the Git repository.
    pub fn commit(
        &self,
        receipts: &[EditReceipt],
        message: &str,
    ) -> Result<Oid, EditError> {
        let repo = GitRepo::open(&self.repo_root)?;
        repo.stage_all()?;
        let oid = repo.commit(message)?;
        Ok(oid)
    }

    /// Rollback all changes described by the receipts.
    pub fn rollback(&self, receipts: &[EditReceipt]) -> Result<(), EditError> {
        // Apply in reverse order
        for receipt in receipts.iter().rev() {
            let full_path = self.repo_root.join(&receipt.path);
            fs::write(&full_path, &receipt.before)?;
        }
        Ok(())
    }
}
```

---

## 4. DiffFormat Configuration

### 4.1 Configuration Model

```rust
/// Configuration for how diffs are computed and displayed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffFormat {
    /// Number of context lines to show around changes.
    pub context_lines: usize,          // default: 3

    /// Output format for diff display.
    pub output: DiffOutput,            // default: Unified

    /// Whether to show line numbers.
    pub show_line_numbers: bool,       // default: true

    /// Maximum line length before truncation.
    pub max_line_length: usize,        // default: 120

    /// Whether to treat whitespace changes as significant.
    pub ignore_whitespace: bool,       // default: false

    /// Whether to treat case changes as significant.
    pub ignore_case: bool,             // default: false

    /// Color scheme for TUI rendering.
    pub color_scheme: DiffColorScheme, // default: Default
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DiffOutput {
    /// Standard unified diff format.
    Unified,
    /// Side-by-side diff format.
    SideBySide { width: usize }, // default width: 160
    /// Stat-only format (just file names and change counts).
    Stat,
    /// JSON-structured diff.
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffColorScheme {
    pub addition_fg: Color,
    pub addition_bg: Option<Color>,
    pub deletion_fg: Color,
    pub deletion_bg: Option<Color>,
    pub context_fg: Color,
    pub hunk_header_fg: Color,
    pub file_header_fg: Color,
}

impl Default for DiffColorScheme {
    fn default() -> Self {
        Self {
            addition_fg: Color::Green,
            addition_bg: None,
            deletion_fg: Color::Red,
            deletion_bg: None,
            context_fg: Color::DarkGray,
            hunk_header_fg: Color::Cyan,
            file_header_fg: Color::Yellow,
        }
    }
}
```

### 4.2 TOML Configuration

```toml
# .xaft.toml

[diff]
context_lines = 3
output = "unified"                # unified | side_by_side | stat | json
show_line_numbers = true
max_line_length = 120
ignore_whitespace = false
ignore_case = false

[diff.side_by_side]
width = 160

[diff.color_scheme]
addition_fg = "green"
deletion_fg = "red"
context_fg = "dark_gray"
hunk_header_fg = "cyan"
file_header_fg = "yellow"
```

---

## 5. compute_unified_diff — Using the `similar` Crate

### 5.1 Why `similar`

The `similar` crate is a Rust port of Python's `difflib` / `patience-diff`. It provides:

- **O(ND) diff algorithm** — Efficient for typical source code changes.
- **Patience diff** — Better alignment of structured code (functions, blocks).
- **Unified diff output** — Direct rendering to standard unified format.
- **Hunk-level access** — Structured access to individual change regions.

### 5.2 Implementation

```rust
use similar::{TextDiff, ChangeTag};

/// Compute a unified diff between two strings using the `similar` crate.
pub fn compute_unified_diff(
    before: &str,
    after: &str,
    config: &DiffFormat,
) -> UnifiedDiff {
    let mut diff_opts = TextDiff::configure();
    diff_opts = diff_opts.context_lines(config.context_lines);

    if config.ignore_whitespace {
        diff_opts = diff_opts.algorithm(similar::Algorithm::Patience);
    }

    let text_diff = diff_opts.diff_lines(before, after);

    let mut hunks: Vec<DiffHunk> = Vec::new();

    for hunk in text_diff.unified_diff().iter_hunks() {
        let mut changes: Vec<DiffChange> = Vec::new();

        for change in hunk.iter_changes() {
            let tag = match change.tag() {
                ChangeTag::Equal => DiffChangeTag::Context,
                ChangeTag::Delete => DiffChangeTag::Delete,
                ChangeTag::Insert => DiffChangeTag::Insert,
            };

            changes.push(DiffChange {
                tag,
                old_line: change.old_index().map(|i| i + 1),
                new_line: change.new_index().map(|i| i + 1),
                content: change.to_string_lossy().to_string(),
            });
        }

        hunks.push(DiffHunk {
            old_start: hunk.old_range().start,
            old_count: hunk.old_range().end - hunk.old_range().start,
            new_start: hunk.new_range().start,
            new_count: hunk.new_range().end - hunk.new_range().start,
            changes,
        });
    }

    // Compute summary stats
    let mut insertions = 0;
    let mut deletions = 0;
    for hunk in &hunks {
        for change in &hunk.changes {
            match change.tag {
                DiffChangeTag::Insert => insertions += 1,
                DiffChangeTag::Delete => deletions += 1,
                DiffChangeTag::Context => {}
            }
        }
    }

    UnifiedDiff {
        hunks,
        insertions,
        deletions,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedDiff {
    pub hunks: Vec<DiffHunk>,
    pub insertions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffHunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub changes: Vec<DiffChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffChange {
    pub tag: DiffChangeTag,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffChangeTag {
    Context,
    Delete,
    Insert,
}
```

### 5.3 Rendering to String

```rust
impl UnifiedDiff {
    /// Render the diff as a standard unified diff string.
    pub fn to_unified_string(&self, path: &Path) -> String {
        let mut output = String::new();

        output.push_str(&format!("--- a/{}\n", path.display()));
        output.push_str(&format!("+++ b/{}\n", path.display()));

        for hunk in &self.hunks {
            output.push_str(&format!(
                "@@ -{},{} +{},{} @@\n",
                hunk.old_start, hunk.old_count,
                hunk.new_start, hunk.new_count
            ));

            for change in &hunk.changes {
                let prefix = match change.tag {
                    DiffChangeTag::Context => " ",
                    DiffChangeTag::Delete  => "-",
                    DiffChangeTag::Insert  => "+",
                };
                output.push_str(&format!("{}{}\n", prefix, change.content));
            }
        }

        output
    }
}
```

---

## 6. Fuzzy Matching Algorithm

### 6.1 Problem Statement

When the LLM generates a diff, the context lines may not exactly match the file content (due to whitespace, minor edits, or stale context). The fuzzy matcher finds the best location to apply each hunk.

### 6.2 Algorithm Overview

```
 ┌───────────────────────────────────────────────────────────────────┐
 │                    Fuzzy Matching Pipeline                        │
 │                                                                   │
 │  Input: file_lines[], hunk.context_before[], hunk.context_after[]│
 │                                                                   │
 │  Step 1: Normalize                                                │
 │  ┌─────────────────────────────────────────────────────────────┐ │
 │  │ - Strip trailing whitespace                                  │ │
 │  │ - Normalize internal whitespace (collapse multiple spaces)   │ │
 │  │ - Lowercase (if ignore_case is set)                          │ │
 │  │ - Remove blank lines at boundaries                           │ │
 │  └─────────────────────────────────────────────────────────────┘ │
 │                                                                   │
 │  Step 2: Exact Match Search                                       │
 │  ┌─────────────────────────────────────────────────────────────┐ │
 │  │ Scan file for exact match of normalized context              │ │
 │  │ If found → return HunkMatch::Exact                           │ │
 │  └─────────────────────────────────────────────────────────────┘ │
 │                                                                   │
 │  Step 3: Sliding Window Fuzzy Match                               │
 │  ┌─────────────────────────────────────────────────────────────┐ │
 │  │ For each window position in file:                            │ │
 │  │   Compute similarity(context, window) using                  │ │
 │  │   ratio of matching lines / total lines                      │ │
 │  │   Score = weighted_avg(context_before_score,                 │ │
 │  │                        context_after_score)                  │ │
 │  │ If best score >= threshold (0.85) → HunkMatch::Fuzzy         │ │
 │  └─────────────────────────────────────────────────────────────┘ │
 │                                                                   │
 │  Step 4: No Match                                                 │
 │  ┌─────────────────────────────────────────────────────────────┐ │
 │  │ Return HunkMatch::None → EditError::HunkNotFound             │ │
 │  └─────────────────────────────────────────────────────────────┘ │
 └───────────────────────────────────────────────────────────────────┘
```

### 6.3 Implementation

```rust
/// Result of attempting to locate a hunk in the file.
pub enum HunkMatch {
    Exact { start: usize, end: usize },
    Fuzzy { start: usize, end: usize, score: f64 },
    None,
}

impl FileEditor {
    /// Find the location in the file where a hunk should be applied.
    fn find_hunk_context(
        &self,
        file_lines: &[String],
        hunk: &HunkDescriptor,
    ) -> HunkMatch {
        let context = hunk.context_before.iter()
            .chain(hunk.removals.iter())
            .chain(hunk.context_after.iter())
            .cloned()
            .collect::<Vec<String>>();

        let normalized_context: Vec<String> = context
            .iter()
            .map(|l| normalize_line(l))
            .collect();

        let context_len = normalized_context.len();
        if context_len == 0 {
            // No context: fall back to line number hint
            let start = hunk.old_start.saturating_sub(1);
            let end = start + hunk.old_count;
            return HunkMatch::Exact { start, end };
        }

        // Step 2: Exact match search
        let normalized_file: Vec<String> = file_lines
            .iter()
            .map(|l| normalize_line(l))
            .collect();

        for (i, window) in normalized_file.windows(context_len).enumerate() {
            if window == &normalized_context[..] {
                return HunkMatch::Exact {
                    start: i,
                    end: i + context_len,
                };
            }
        }

        // Step 3: Sliding window fuzzy match
        let mut best_score: f64 = 0.0;
        let mut best_pos: usize = 0;

        for (i, window) in normalized_file.windows(context_len).enumerate() {
            let score = line_similarity(&normalized_context, window);
            if score > best_score {
                best_score = score;
                best_pos = i;
            }
        }

        if best_score >= self.fuzzy_threshold {
            HunkMatch::Fuzzy {
                start: best_pos,
                end: best_pos + context_len,
                score: best_score,
            }
        } else {
            HunkMatch::None
        }
    }
}

/// Normalize a line for comparison.
fn normalize_line(line: &str) -> String {
    let mut s = line.trim_end().to_string();
    // Collapse multiple internal spaces to one
    let re = regex::Regex::new(r" {2,}").unwrap();
    s = re.replace_all(&s, " ").to_string();
    s
}

/// Compute similarity between two sequences of lines.
/// Uses a weighted line-by-line comparison with Levenshtein distance.
fn line_similarity(a: &[String], b: &[String]) -> f64 {
    if a.len() != b.len() {
        return 0.0;
    }

    let mut total_similarity: f64 = 0.0;

    for (line_a, line_b) in a.iter().zip(b.iter()) {
        let line_score = if line_a == line_b {
            1.0
        } else {
            // Use Levenshtein-based similarity ratio
            let edit_dist = levenshtein_distance(line_a, line_b);
            let max_len = line_a.len().max(line_b.len()).max(1);
            1.0 - (edit_dist as f64 / max_len as f64)
        };
        total_similarity += line_score;
    }

    total_similarity / a.len() as f64
}

/// Compute Levenshtein distance between two strings.
fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_len = a.chars().count();
    let b_len = b.chars().count();

    if a_len == 0 { return b_len; }
    if b_len == 0 { return a_len; }

    let mut matrix = vec![vec![0; b_len + 1]; a_len + 1];

    for (i, row) in matrix.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=b_len {
        matrix[0][j] = j;
    }

    for (i, ca) in a.chars().enumerate() {
        for (j, cb) in b.chars().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            matrix[i + 1][j + 1] = (matrix[i][j + 1] + 1)
                .min(matrix[i + 1][j] + 1)
                .min(matrix[i][j] + cost);
        }
    }

    matrix[a_len][b_len]
}
```

### 6.4 Sliding Window Visualization

```
 File content (normalized):
 ┌────┬────┬────┬────┬────┬────┬────┬────┬────┬────┐
 │ L0 │ L1 │ L2 │ L3 │ L4 │ L5 │ L6 │ L7 │ L8 │ L9 │
 └────┴────┴────┴────┴────┴────┴────┴────┴────┴────┘

 Hunk context (3 lines):
 ┌────┬────┬────┐
 │ C0 │ C1 │ C2 │
 └────┴────┴────┘

 Window positions:
 Position 0: [L0, L1, L2] → similarity = 0.42
 Position 1: [L1, L2, L3] → similarity = 0.67
 Position 2: [L2, L3, L4] → similarity = 0.91  ← best, > 0.85 threshold
 Position 3: [L3, L4, L5] → similarity = 0.58
 ...

 Result: HunkMatch::Fuzzy { start: 2, end: 5, score: 0.91 }
```

### 6.5 Weighted Context Matching

The context_before and context_after lines are weighted more heavily than the removals, because they represent anchor points that are more likely to be stable.

```rust
/// Compute weighted similarity giving more weight to context lines.
fn weighted_hunk_similarity(
    hunk: &HunkDescriptor,
    window: &[String],
) -> f64 {
    let ctx_before_len = hunk.context_before.len();
    let removals_len = hunk.removals.len();
    let ctx_after_len = hunk.context_after.len();

    let total_len = ctx_before_len + removals_len + ctx_after_len;
    if total_len == 0 || window.len() != total_len {
        return 0.0;
    }

    let mut weighted_sum: f64 = 0.0;
    let mut total_weight: f64 = 0.0;

    let normalized_ctx_before: Vec<String> = hunk.context_before
        .iter().map(|l| normalize_line(l)).collect();
    let normalized_removals: Vec<String> = hunk.removals
        .iter().map(|l| normalize_line(l)).collect();
    let normalized_ctx_after: Vec<String> = hunk.context_after
        .iter().map(|l| normalize_line(l)).collect();

    let normalized_window: Vec<String> = window
        .iter().map(|l| normalize_line(l)).collect();

    let mut idx = 0;

    // Context before: weight = 1.5
    for (i, ctx_line) in normalized_ctx_before.iter().enumerate() {
        let score = line_similarity_single(ctx_line, &normalized_window[idx]);
        weighted_sum += score * 1.5;
        total_weight += 1.5;
        idx += 1;
    }

    // Removals: weight = 1.0
    for (i, rem_line) in normalized_removals.iter().enumerate() {
        let score = line_similarity_single(rem_line, &normalized_window[idx]);
        weighted_sum += score * 1.0;
        total_weight += 1.0;
        idx += 1;
    }

    // Context after: weight = 1.5
    for (i, ctx_line) in normalized_ctx_after.iter().enumerate() {
        let score = line_similarity_single(ctx_line, &normalized_window[idx]);
        weighted_sum += score * 1.5;
        total_weight += 1.5;
        idx += 1;
    }

    weighted_sum / total_weight
}

fn line_similarity_single(a: &str, b: &str) -> f64 {
    if a == b { return 1.0; }
    let dist = levenshtein_distance(a, b);
    let max_len = a.len().max(b.len()).max(1);
    1.0 - (dist as f64 / max_len as f64)
}
```

---

## 7. Patch Generation for Git

### 7.1 From EditReceipts to Git Patch

```rust
/// Generate a Git-compatible patch from a list of edit receipts.
pub fn generate_git_patch(
    receipts: &[EditReceipt],
    config: &DiffFormat,
) -> Result<String, PatchError> {
    let mut patch = String::new();

    for receipt in receipts {
        let diff = receipt.diff.as_ref()
            .ok_or(PatchError::DiffNotComputed { path: receipt.path.clone() })?;

        // Git patch header
        patch.push_str(&format!("diff --git a/{} b/{}\n", receipt.path.display(), receipt.path.display()));

        // File mode (if applicable)
        patch.push_str(&format!("--- a/{}\n", receipt.path.display()));
        patch.push_str(&format!("+++ b/{}\n", receipt.path.display()));

        // Hunks
        for hunk in &diff.hunks {
            patch.push_str(&format!(
                "@@ -{},{} +{},{} @@\n",
                hunk.old_start, hunk.old_count,
                hunk.new_start, hunk.new_count
            ));

            for change in &hunk.changes {
                let prefix = match change.tag {
                    DiffChangeTag::Context => " ",
                    DiffChangeTag::Delete  => "-",
                    DiffChangeTag::Insert  => "+",
                };

                // Handle "no newline at end of file"
                let content = if change.content.ends_with('\n') {
                    change.content.trim_end_matches('\n').to_string()
                } else {
                    format!("{}\n\\ No newline at end of file\n", change.content)
                };

                patch.push_str(&format!("{}{}", prefix, content));
            }
        }
    }

    Ok(patch)
}
```

### 7.2 Apply a Git Patch

```rust
/// Apply a Git-compatible patch string to the working tree.
pub fn apply_git_patch(
    repo_root: &Path,
    patch: &str,
    options: &PatchApplyOptions,
) -> Result<Vec<EditReceipt>, PatchError> {
    let patch_path = repo_root.join(".xaft").join("temp.patch");
    fs::write(&patch_path, patch)?;

    let mut cmd = Command::new("git");
    cmd.arg("apply");

    if options.check {
        cmd.arg("--check");
    }
    if options.stat {
        cmd.arg("--stat");
    }
    if options.whitespace.is_some() {
        cmd.arg(format!("--whitespace={}", options.whitespace.unwrap()));
    }
    if options.reverse {
        cmd.arg("--reverse");
    }
    if options.context_lines != 3 {
        cmd.arg(format!("-U{}", options.context_lines));
    }

    cmd.arg(&patch_path);
    cmd.current_dir(repo_root);

    let output = cmd.output()?;

    if !output.status.success() {
        return Err(PatchError::ApplyFailed {
            exit_code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    // Clean up temp patch
    let _ = fs::remove_file(&patch_path);

    Ok(vec![]) // TODO: reconstruct receipts from applied changes
}

#[derive(Debug, Clone)]
pub struct PatchApplyOptions {
    pub check: bool,              // --check: just test if patch applies
    pub stat: bool,               // --stat: show diffstat instead of applying
    pub whitespace: Option<String>, // --whitespace: nowarn|warn|fix|error|error-all
    pub reverse: bool,            // --reverse: apply patch in reverse
    pub context_lines: usize,     // -U<n>: number of context lines
}
```

---

## 8. TUI Diff Rendering

### 8.1 Unified Diff View

```
┌─ Diff: src/api/auth.rs ──────────────────────────────────────────────┐
│                                                                      │
│ @@ -38,6 +38,18 @@ pub fn routes() -> Router {                     │
│  38 │  })                                                           │
│  39 │ }                                                             │
│  40 │                                                               │
│  41 │ +    pub async fn login(                                      │
│  42 │ +        State(state): State<AppState>,                       │
│  43 │ +        Json(payload): Json<LoginRequest>,                   │
│  44 │ +    ) -> Result<Json<TokenResponse>, ApiError> {             │
│  45 │ +        let claims = Claims {                                │
│  46 │ +            sub: payload.username.clone(),                   │
│  47 │ +            exp: (Utc::now() + Duration::hours(24))          │
│  48 │ +                .timestamp() as usize,                       │
│  49 │ +        };                                                   │
│  50 │ +        let token = jwt::create_token(                       │
│  51 │ +            &claims.sub,                                     │
│  52 │ +            &state.jwt_secret,                               │
│  53 │ +        )?;                                                  │
│  54 │ +        Ok(Json(TokenResponse { token }))                    │
│  55 │ +    }                                                        │
│  56 │                                                               │
│  57 │  #[cfg(test)]                                                 │
│                                                                      │
│  18 insertions(+) 0 deletions(-)                                    │
│                                                                      │
│  [↑↓] Scroll   [S] Side-by-Side   [A] Accept   [R] Reject          │
└──────────────────────────────────────────────────────────────────────┘
```

### 8.2 Side-by-Side Diff View

```
┌─ Diff: src/api/auth.rs (side-by-side) ───────────────────────────────┐
│                                                                      │
│      LEFT (before)            │       RIGHT (after)                  │
│ ─────────────────────────────┼────────────────────────────────────── │
│  38 │  })                    │  38 │  })                             │
│  39 │ }                      │  39 │ }                              │
│  40 │                        │  40 │                                │
│  41 │                        │  41 │ +    pub async fn login(        │
│     │                        │  42 │ +        State(state): State<   │
│     │                        │  43 │ +        Json(payload): Json<   │
│     │                        │  44 │ +    ) -> Result<Json<Token    │
│     │                        │  45 │ +        let claims = Claims {  │
│     │                        │  46 │ +            sub: payload.user │
│     │                        │  47 │ +            exp: (Utc::now()  │
│     │                        │  48 │ +                .timestamp()   │
│     │                        │  49 │ +        };                    │
│     │                        │  50 │ +        let token = jwt::cre  │
│     │                        │  51 │ +            &claims.sub,      │
│     │                        │  52 │ +            &state.jwt_secret │
│     │                        │  53 │ +        )?;                   │
│     │                        │  54 │ +        Ok(Json(TokenRespons  │
│     │                        │  55 │ +    }                         │
│  42 │                        │  56 │                                │
│  43 │  #[cfg(test)]          │  57 │  #[cfg(test)]                  │
│                                                                      │
│  [U] Unified   [S] Stat   [J] JSON   [Esc] Close                   │
└──────────────────────────────────────────────────────────────────────┘
```

---

## 9. EditReceipt Lifecycle

```
 ┌─────────────────────────────────────────────────────────────┐
 │                                                             │
 │  FileEditor.replace_block()                                 │
 │  FileEditor.apply_diff()                                    │
 │  FileEditor.multi_edit()    ──▶  EditReceipt               │
 │                                     │                       │
 │                              ┌──────┼──────┐               │
 │                              ▼      ▼      ▼               │
 │                         Compute  Store   Emit               │
 │                          Diff    in      Signal              │
 │                                  History                    │
 │                              │                               │
 │                              ▼                               │
 │                     ┌─────────────────┐                     │
 │                     │  EditHistory    │                     │
 │                     │  (Vec<Receipt>) │                     │
 │                     └────────┬────────┘                     │
 │                              │                               │
 │                     ┌────────┼────────┐                     │
 │                     ▼        ▼        ▼                     │
 │                  Commit   Rollback   Display                │
 │                  (Git)    (restore   (TUI)                  │
 │                           from                            │
 │                           receipt.before)                   │
 └─────────────────────────────────────────────────────────────┘
```

---

## 10. Configuration Summary

```toml
# .xaft.toml

[editor]
# Fuzzy matching threshold (0.0–1.0)
fuzzy_threshold = 0.85

[editor.diff]
context_lines = 3
output = "unified"
show_line_numbers = true
max_line_length = 120
ignore_whitespace = false
ignore_case = false

[editor.patch]
# Whether to verify patches after generation
verify = true
# Default whitespace handling for git apply
whitespace = "warn"
```

---

## 11. Error Taxonomy

| Error                              | Code   | Recovery                                    |
|------------------------------------|--------|---------------------------------------------|
| `EditError::FileRead`             | D-001  | Check file exists and is readable            |
| `EditError::LineOutOfRange`       | D-002  | Re-read file to get current line count       |
| `EditError::OverlappingEdits`     | D-003  | Split into separate non-overlapping edits    |
| `EditError::FuzzyMatchFailed`     | D-004  | Lower threshold or re-read file for context  |
| `EditError::HunkNotFound`         | D-005  | Re-read file; use full context lines         |
| `PatchError::DiffNotComputed`     | D-006  | Call compute_unified_diff on receipt first   |
| `PatchError::ApplyFailed`         | D-007  | Try with --whitespace=fix; otherwise manual  |

---

## 12. Future Considerations

1. **Structural diff** — AST-aware diffing that understands code structure (functions, classes) rather than just text lines.
2. **Semantic diff** — Use LLM to generate human-readable summaries of what changed and why.
3. **Diff-based caching** — Cache diff computations keyed by (before_hash, after_hash).
4. **Three-way merge** — Support merge-base-aware diffs when working in shared branches.
5. **Binary diff** — Handle binary file changes (images, compiled artifacts) with meaningful metadata.
