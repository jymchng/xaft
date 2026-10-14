# PRD 39 — `/diff`, `/commit`, `/pr` Commands: Git Operations

## Status: Draft
## Depends on: PRD 32a–32e, CLAUDE.md §Safety invariants (Git isolation)

---

## 1. Problem

All three git-operation commands return `"not yet connected to runtime."`.
They share a common dependency: access to the session's git worktree branch
(`xaft/session-<uuid>`), which is where agent edits land. None have access
to working-dir path or git context today.

The user expects these commands to work like their shell equivalents, scoped
to the session worktree branch.

---

## 2. Goals

### `/diff [path]`

Show the diff between the session worktree branch and its base commit (the
commit that existed on `HEAD` when the session started). If `[path]` is
given, scope to that file.

### `/commit [message]`

Commit all staged changes on the session worktree branch. If no `[message]`
is given, open an inline single-line edit field (same style as `/config`
inline edit from PRD 34 Feature B) to type the message. Confirm with
`Enter`, cancel with `Esc`.

### `/pr [title]`

Create a GitHub pull request from the session branch to the repository's
default branch. If `[title]` is omitted, use the session's task as the
title. Requires `gh` CLI in PATH and a remote origin.

---

## 3. Non-Goals

- Push to remote without user confirmation (always require `requires_confirmation = true`).
- Supporting non-GitHub remotes for `/pr` in v1.
- Cherry-pick or rebase UI.
- Multi-file staging UI.

---

## 4. Technical Design

### 4.1 `CommandContext` addition: `session_branch`

```rust
pub session_branch: Option<String>,   // "xaft/session-<uuid>"
pub working_dir: PathBuf,             // already present
```

`AppState::build_command_context` populates `session_branch` from
`AppState::session_id` (formatted as the branch name convention).

### 4.2 `DiffHandler`

```rust
fn is_async(&self) -> bool { true }

async fn execute_async(&self, ctx: CommandContext) -> CommandResult {
    let branch = ctx.session_branch
        .ok_or_else(|| CommandResult::Error("No active session branch.".into()))?;
    let path_filter = if ctx.args.trim().is_empty() {
        None
    } else {
        Some(ctx.args.trim().to_string())
    };
    // Run: git diff <base>...<branch> [-- path]
    let base = format!("{branch}~1");
    let mut cmd = tokio::process::Command::new("git");
    cmd.current_dir(&ctx.working_dir)
       .args(["diff", &base, &branch]);
    if let Some(ref p) = path_filter { cmd.args(["--", p]); }
    let out = cmd.output().await?;
    let diff_text = String::from_utf8_lossy(&out.stdout);
    if diff_text.is_empty() {
        return CommandResult::Lines(vec!["No changes.".into()]);
    }
    // Use build_file_diff_lines for syntax-coloured output
    let lines = build_file_diff_lines(&diff_text, ctx.terminal_cols);
    CommandResult::StyledLines(lines)
}
```

### 4.3 `CommitHandler`

Steps:
1. Stage all changes in `working_dir` (`git add -A`).
2. If no message arg: emit `XaftInlinePromptRequested { prompt: "Commit message:" }`;
   the TUI shows the inline edit; on confirm the runtime re-invokes commit.
3. Run `git commit -m <message>` on the session branch.
4. Emit `XaftCommitCreated` signal (already defined per CLAUDE.md).
5. Return `CommandResult::Lines` with the short hash.

`CommitHandler::requires_confirmation() → true` so the approval gate runs
before the `git add` step.

### 4.4 `PrHandler`

Steps:
1. Check `gh` is in PATH (`which gh`); error if not found.
2. Check remote origin exists (`git remote get-url origin`); error if not.
3. Push the session branch to origin (`git push -u origin <branch>`).
4. Run `gh pr create --title <title> --body "Created by xaft session <id>"
   --base <default_branch>`.
5. Parse the PR URL from output; return it as a clickable line.

`PrHandler::requires_confirmation() → true`.

### 4.5 `requires_confirmation` on `SlashHandler`

Add method to `SlashHandler` trait with default `false`:
```rust
fn requires_confirmation(&self) -> bool { false }
```

`CommitHandler` and `PrHandler` override to `true`.

`AppState::handle_slash_parse_result` checks `requires_confirmation()`
before executing; if true, pushes an `ApprovalQueue` request (same flow
as tool approval gate). On approval, execution continues; on denial,
prints `"Cancelled."`.

---

## 5. Implementation Steps

1. Add `session_branch` to `CommandContext`.
2. Populate it in `AppState::build_command_context`.
3. Add `requires_confirmation` method to `SlashHandler` trait.
4. Implement `DiffHandler::execute_async` using `build_file_diff_lines`.
5. Define `XaftInlinePromptRequested` signal for inline message input.
6. Implement `CommitHandler` (stage → confirm → commit → signal).
7. Implement `PrHandler` (push → gh pr create → return URL).
8. Wire approval flow for confirmed handlers.
9. Tests (see §6).

---

## 6. Tests

```
test diff_handler_no_changes_prints_no_changes
test diff_handler_with_changes_returns_styled_diff
test diff_handler_path_filter_scopes_output
test diff_handler_no_session_returns_error
test commit_handler_requires_confirmation
test commit_handler_commits_on_session_branch
test commit_handler_empty_message_emits_inline_prompt
test pr_handler_requires_confirmation
test pr_handler_no_gh_cli_returns_error
test pr_handler_no_remote_returns_error
```
