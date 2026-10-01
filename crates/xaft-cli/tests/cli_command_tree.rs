//! Snapshot of the real clap command tree, kept honest against the matrix.
//!
//! WHY THIS EXISTS
//!     Commit `5f06350`'s defect is the one this file makes impossible to
//!     repeat: `RunsSubcommand::List`'s doc comment claimed bare `xaft runs` was
//!     an alias of `xaft runs list`, and the binary rejected it with a
//!     "requires a subcommand" usage error. The matrix's ✅ marks were believed
//!     because a name appeared in a doc comment, not because the parser
//!     accepted the string.
//!
//!     The fix for that class is not "look harder at args.rs" — it is to ask
//!     clap itself. `XaftCli::command()` is the exact tree the binary parses,
//!     including `#[command(alias = ...)]`, `Option<T>` subcommands and clap's
//!     kebab-casing. Everything else about a command is a comment.
//!
//! WHY A CHECKED-IN FILE
//!     `scripts/parity-row-reachability.py` needs the tree on every
//!     `--check` run, including ones where nobody has built the workspace. A
//!     generated file that nothing verifies is a lie waiting to happen, so the
//!     file is verified here: if the tree changes and this snapshot does not,
//!     `the_checked_in_cli_tree_still_matches_clap` fails. Regenerate with
//!
//!         XAFT_WRITE_CLI_TREE=1 cargo test -p xaft-cli --test cli_command_tree
//!
//!     which writes the file and passes, so the fix is one command, not an
//!     archaeology exercise.

use clap::CommandFactory;

use xaft_cli::args::XaftCli;

/// Every command path the binary accepts, one per line, deepest last.
///
/// A path is space-separated command names with no leading binary name, e.g.
/// `session list`. Parent-only paths are included, because `xaft jobs` IS a
/// command even though it requires a subcommand.
const TREE: &str = include_str!("../../../docs/cli-command-tree.txt");

/// Walk clap's own tree, depth-first, sorted, emitting `parent child` paths.
fn tree_paths(cmd: &clap::Command, prefix: &str, out: &mut Vec<String>) {
    let mut subs: Vec<&clap::Command> = cmd.get_subcommands().collect();
    subs.sort_by_key(|c| c.get_name().to_string());
    for sub in subs {
        let name = sub.get_name().to_string();
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix} {name}")
        };
        out.push(path.clone());
        tree_paths(sub, &path, out);
    }
}

/// clap's tree, as sorted lines.
fn real_tree() -> Vec<String> {
    let mut out = Vec::new();
    tree_paths(&XaftCli::command(), "", &mut out);
    out.sort();
    out
}

fn write_tree(lines: &[String]) {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/cli-command-tree.txt");
    let mut body = String::from(
        "# Generated from `XaftCli::command()` — do not edit by hand.\n\
         # Regenerate: XAFT_WRITE_CLI_TREE=1 cargo test -p xaft-cli --test cli_command_tree\n",
    );
    for line in lines {
        body.push_str(line);
        body.push('\n');
    }
    std::fs::write(path, body).expect("write docs/cli-command-tree.txt");
}

fn snapshot_lines() -> Vec<String> {
    TREE.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// The checked-in snapshot is exactly what clap builds right now.
///
/// This is the guard that makes the Python audit sound: without it,
/// `docs/cli-command-tree.txt` is just another file that can drift.
#[test]
fn the_checked_in_cli_tree_still_matches_clap() {
    let real = real_tree();

    if std::env::var("XAFT_WRITE_CLI_TREE").is_ok() {
        write_tree(&real);
        return;
    }

    let snap = snapshot_lines();
    assert_eq!(
        snap, real,
        "docs/cli-command-tree.txt is stale: clap's tree and the snapshot disagree.\n\
         regenerate with:\n    \
         XAFT_WRITE_CLI_TREE=1 cargo test -p xaft-cli --test cli_command_tree"
    );
}

/// Guard the guard: a parse that finds nothing would make the audit pass for
/// the wrong reason, which is exactly how `/usage` survived until G5.
#[test]
fn the_cli_tree_snapshot_is_not_empty_or_trivial() {
    let snap = snapshot_lines();
    assert!(
        snap.len() >= 40,
        "only {} command paths parsed from the snapshot — the walker is broken",
        snap.len()
    );
    for expected in [
        "run",
        "config show",
        "config profiles",
        "session list",
        "session export",
        "session inspect",
        "jobs list",
        "jobs purge",
        "runs list",
        "mcp auth",
        "mcp logout",
        "workflows list",
        "workflows run",
    ] {
        assert!(
            snap.iter().any(|l| l == expected),
            "`{expected}` is missing from the clap tree snapshot"
        );
    }
}

/// The specific defect `5f06350` shipped: a doc comment advertising a bare
/// form the parser rejects. Bare `xaft runs` resolves to `RunsCommand`, whose
/// subcommand is `Option<RunsSubcommand>` — so the bare path is `runs`, and
/// `runs list` must still exist alongside it. Both are asserted because
/// "fixed the bare form" and "broke the explicit form" are one edit apart.
#[test]
fn bare_and_explicit_runs_forms_both_exist() {
    let snap = snapshot_lines();
    assert!(snap.iter().any(|l| l == "runs"), "bare `runs` is gone");
    assert!(
        snap.iter().any(|l| l == "runs list"),
        "`runs list` disappeared; the bare alias must not have replaced it"
    );
}
