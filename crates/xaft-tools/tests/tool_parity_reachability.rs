//! Reachability guard for section 6 of `docs/parity-matrix.md`.
//!
//! G5 found that `/usage` was marked ✅ because the name appeared somewhere in
//! the source, while the only string a user could type answered "Unknown
//! command". Section 6 has the same shape of risk and none of the proof: a
//! tool row can be marked ✅ because a name is *mentioned* in a source file
//! while the registry the agent actually calls never registers it.
//!
//! So this test derives the ✅ set from the matrix file itself rather than
//! from a hand-copied list, and requires every name it finds to resolve in a
//! registry built by `ToolRegistryBuilder`. A row that flips to ✅ without the
//! tool being registered fails here; a tool that is registered but whose row
//! is not ✅ does not, because extra tools are not a parity failure.
//!
//! The deliberate exception is `apply_diff`, which agenthicc exposes under
//! its own name and xaft implements as `patch_file`. The matrix row records
//! that mapping, and the row's xaft column names the xaft tool, which is what
//! this test reads.
//!
//! WHY THERE IS ALSO A SNAPSHOT FILE
//!     `scripts/parity-row-reachability.py` has to compare the matrix's ✅ rows
//!     against the registered set on every run, including runs where nobody has
//!     built the workspace. It could grep for `const TOOL_NAME = "..."` — and
//!     that would be wrong, because row 78 `mcp_connection_guide` is registered
//!     correctly while its `name()` returns a bare literal rather than a const,
//!     so the grep reports a live tool as missing. A false alarm in the exact
//!     place the check exists to be accurate is worse than no check.
//!
//!     So the registered set is snapshotted here, from the registry an agent
//!     can actually be handed, and `the_checked_in_tool_names_still_match_the_registry`
//!     fails the moment the two disagree. Regenerate with
//!
//!         XAFT_WRITE_TOOL_REGISTRY=1 cargo test -p xaft-tools --test tool_parity_reachability

use std::sync::Arc;

use agtrs_runtime::tool::Tool;
use tempfile::TempDir;
use xaft_tools::{FsWorkspaceStore, ToolRegistryBuilder};

/// The matrix, embedded at build time. `include_str!` is relative to this
/// file, so the test fails to compile if the matrix is moved — which is the
/// behaviour we want, not a silent skip.
const MATRIX: &str = include_str!("../../../docs/parity-matrix.md");

/// agenthicc tool name -> the xaft tool name, where they differ and the
/// matrix's xaft column does not say so in a parseable way.
const ALIASES: &[(&str, &str)] = &[];

/// xaft tool names that are *supersets* of an agenthicc tool rather than
/// renames of it, so the matrix's xaft cell names more than one tool.
const MULTI: &[&str] = &[];

/// The registered tool names, snapshotted from a real registry.
///
/// `scripts/parity-row-reachability.py` reads this file, so it is verified
/// against the registry below rather than trusted. See the module doc for why
/// this is not a `const TOOL_NAME` grep.
const REGISTERED: &str = include_str!("../../../docs/tool-registry.txt");

/// Pull the xaft tool name out of a matrix row's fourth column.
///
/// Rows look like:
/// `| 3 | `batch_copy` | `tools/fs/agent_tools.py` | \u2014 | \u274c |`
/// so the xaft cell is the one before the status glyph.
fn xaft_cell(row: &str) -> Option<&str> {
    let cells: Vec<&str> = row.split('|').map(str::trim).collect();
    // split gives ["", "#", "tool", "source", "xaft", "status", ""]
    if cells.len() < 6 {
        return None;
    }
    let status = cells[cells.len() - 2];
    if !status.contains('✅') {
        return None;
    }
    Some(cells[cells.len() - 3])
}

/// Tool names named in a matrix cell, e.g. `` `patch_file` (renamed) ``.
fn names_in(cell: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = cell;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        match after.find('`') {
            Some(end) => {
                out.push(after[..end].to_string());
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    out
}

/// Every xaft tool name section 6 marks as done.
fn matrix_done_tools() -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut in_section_6 = false;
    for line in MATRIX.lines() {
        if line.starts_with("## 6. Built-in tools") {
            in_section_6 = true;
            continue;
        }
        if in_section_6 && line.starts_with("## 7.") {
            break;
        }
        if !in_section_6 || !line.starts_with('|') {
            continue;
        }
        let Some(cell) = xaft_cell(line) else {
            continue;
        };
        let number: usize = line
            .split('|')
            .nth(1)
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(0);
        for name in names_in(cell) {
            out.push((number, name));
        }
    }
    out
}

/// The registered names, sorted, as snapshot lines.
fn snapshot_lines() -> Vec<String> {
    let mut v: Vec<String> = REGISTERED
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    v.sort();
    v
}

/// The checked-in snapshot is exactly what the registry holds right now.
///
/// This is what makes the Python audit sound: without it,
/// `docs/tool-registry.txt` is just another file that can drift, and the
/// audit would report a missing tool against a stale list.
#[test]
fn the_checked_in_tool_names_still_match_the_registry() {
    let registry = coder_registry();
    let mut real = registry.names();
    real.sort();

    if std::env::var("XAFT_WRITE_TOOL_REGISTRY").is_ok() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/tool-registry.txt");
        let mut body = String::from(
            "# Generated from a `ToolRegistryBuilder` coder registry — do not edit by hand.\n\
             # Regenerate: XAFT_WRITE_TOOL_REGISTRY=1 cargo test -p xaft-tools --test tool_parity_reachability\n",
        );
        for name in &real {
            body.push_str(name);
            body.push('\n');
        }
        std::fs::write(path, body).expect("write docs/tool-registry.txt");
        return;
    }

    assert_eq!(
        snapshot_lines(),
        real,
        "docs/tool-registry.txt is stale: the registry and the snapshot disagree.\n\
         regenerate with:\n    \
         XAFT_WRITE_TOOL_REGISTRY=1 cargo test -p xaft-tools --test tool_parity_reachability"
    );
}

/// Guard the guard: the snapshot must actually name the tools the matrix
/// claims, or the audit reads an empty list and passes for the wrong reason.
#[test]
fn the_tool_registry_snapshot_is_not_trivial() {
    let snap = snapshot_lines();
    assert!(
        snap.len() >= 30,
        "only {} tool names parsed from the snapshot — the parser is broken",
        snap.len()
    );
    for expected in [
        "read_file",
        "batch_read",
        "bash_exec",
        "mcp_connection_guide",
    ] {
        assert!(
            snap.iter().any(|l| l == expected),
            "`{expected}` is missing from the tool registry snapshot"
        );
    }
}

/// A coder registry built over a real git repository, so the git rows the
/// matrix marks done are actually resolved rather than skipped.
fn coder_registry() -> xaft_tools::ToolRegistry {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    // GitRepo::open only succeeds inside a real repository.
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&root)
        .status()
        .expect("git init");
    // Leak the TempDir so the store outlives the registry for the test's duration.
    std::mem::forget(tmp);
    ToolRegistryBuilder::new(root)
        .with_shell()
        .build_coder()
        .expect("build coder registry")
}

/// Every tool the parity matrix marks done in section 6 must actually be
/// registered in a registry an agent can be given.
///
/// This is the tool-surface twin of G5's `every_agenthicc_command_the_matrix_marks_done_actually_parses`.
#[test]
fn every_tool_the_matrix_marks_done_is_registered() {
    let registry = coder_registry();
    let registered = registry.names();

    let mut missing = Vec::new();
    for (row, name) in matrix_done_tools() {
        let name = ALIASES
            .iter()
            .find(|(a, _)| *a == name)
            .map(|(_, b)| b.to_string())
            .unwrap_or(name);
        if MULTI.contains(&name.as_str()) {
            continue;
        }
        if !registered.contains(&name) {
            missing.push(format!("row {row}: `{name}`"));
        }
    }

    assert!(
        missing.is_empty(),
        "the matrix marks these tools done but the registry does not register them:\n  {}\n\n\
         a row is only ✅ if the agent can actually call the tool",
        missing.join("\n  ")
    );
}

/// A tool registered under a name the matrix does not know about is not a
/// parity failure, but a *renamed* tool whose old name is still documented
/// anywhere is a lie. `read_many` was renamed to `batch_read` in this change;
/// this test pins that the old name is gone rather than left behind as a
/// second, undocumented spelling.
#[test]
fn the_renamed_read_many_is_not_still_registered() {
    let registry = coder_registry();
    assert!(
        registry.get("read_many").is_none(),
        "`read_many` was renamed to `batch_read`; registering both would leave two \
         near-identical tools with one undocumented name"
    );
    assert!(registry.get("batch_read").is_some());
}

/// Guard the guard: if the parser ever stops finding rows, this test would
/// pass vacuously. Assert it found a plausible number of them.
#[test]
fn the_matrix_parser_actually_finds_section_6_rows() {
    let done = matrix_done_tools();
    assert!(
        done.len() >= 30,
        "only {} done tool rows parsed out of section 6 — the parser is broken, \
         and the reachability test above would pass for the wrong reason",
        done.len()
    );
    assert!(
        done.iter().any(|(_, n)| n == "read_file"),
        "sanity: read_file should be among the parsed names"
    );
}

/// The new tools must be reachable through the registry, not merely exist as
/// types — that is the distinction this file exists to enforce.
#[test]
fn the_new_parity_tools_are_in_the_coder_registry() {
    let registry = coder_registry();
    for name in [
        "batch_read",
        "batch_write",
        "batch_delete",
        "batch_move",
        "batch_copy",
        "checksum_file",
        "touch_file",
        "truncate_file",
        "file_exists",
        "read_lines",
        "mcp_connection_guide",
    ] {
        assert!(registry.get(name).is_some(), "{name} is not registered");
    }
}

/// Destructive batch tools must sit behind the approval gate, the same way
/// `delete_file` does. A batch tool that deletes N files without asking is a
/// strictly worse version of the single-file one.
#[test]
fn destructive_batch_tools_require_confirmation() {
    let tmp = TempDir::new().unwrap();
    let store =
        Arc::new(FsWorkspaceStore::new(tmp.path())) as Arc<dyn agtrs_workspace::WorkspaceStore>;
    assert!(
        xaft_tools::BatchDeleteTool::new(store, tmp.path()).requires_confirmation(),
        "batch_delete removes files and must require confirmation"
    );
    let store =
        Arc::new(FsWorkspaceStore::new(tmp.path())) as Arc<dyn agtrs_workspace::WorkspaceStore>;
    assert!(
        xaft_tools::TruncateFileTool::new(store, tmp.path()).requires_confirmation(),
        "truncate_file destroys data and must require confirmation"
    );
}
