# XAFT Git Integration — PRD

> Document ID: XAFT-EXEC-001
> Version: 0.1.0-draft
> Status: Design Phase
> Owner: xaft-core team

---

## 1. Overview

Git is `xaft`'s primary mechanism for safety and reversibility. Every mutation the agent makes is tracked through Git, enabling atomic commits, per-step rollback, and clean isolation via worktrees. This document specifies `GitRepo`, `WorktreeGuard`, the branch→edit→verify→commit/restore pattern, hook policies, commit message generation, agent lifecycle integration, Git signals, and TUI presentation.

---

## 2. Architecture

```
 ┌─────────────────────────────────────────────────────────────────────┐
 │                       xaft Git Integration                          │
 │                                                                     │
 │  ┌────────────┐    ┌────────────────┐    ┌───────────────────────┐ │
 │  │  GitRepo   │───▶│ WorktreeGuard  │───▶│  Agent Lifecycle      │ │
 │  │  (shared   │    │ (isolated      │    │  on_step_start()      │ │
 │  │   handle)  │    │  working tree) │    │  on_step_finish()     │ │
 │  └─────┬──────┘    └───────┬────────┘    │  on_plan_complete()   │ │
 │        │                   │             └───────────────────────┘ │
 │        ▼                   ▼                                       │
 │  ┌────────────┐    ┌────────────────┐                              │
 │  │ Git Signals│    │ HookPolicy     │                              │
 │  │ (event     │    │ (Respect/Skip/ │                              │
 │  │  bus)      │    │  Warn)         │                              │
 │  └────────────┘    └────────────────┘                              │
 │                                                                     │
 │  ┌────────────────────────────────────────────────────────────────┐ │
 │  │              Branch→Edit→Verify→Commit/Restore                │ │
 │  └────────────────────────────────────────────────────────────────┘ │
 └─────────────────────────────────────────────────────────────────────┘
```

---

## 3. GitRepo — Shared Repository Handle

### 3.1 Data Model

```rust
/// A shared handle to the underlying Git repository.
/// Provides all Git operations needed by xaft.
pub struct GitRepo {
    /// Path to the repository root (contains .git/).
    root: PathBuf,
    /// Underlying git2::Repository handle.
    repo: Repository,
    /// Configuration for this repo's Git behavior.
    config: GitRepoConfig,
}

#[derive(Debug, Clone)]
pub struct GitRepoConfig {
    /// Name to use for the author of xaft commits.
    pub author_name: String,         // default: "xaft"
    /// Email to use for the author of xaft commits.
    pub author_email: String,        // default: "xaft@local"
    /// Naming pattern for xaft branches.
    pub branch_prefix: String,       // default: "xaft/"
    /// Whether to sign commits (GPG/SSH).
    pub sign_commits: bool,          // default: false
    /// Maximum number of xaft branches to retain.
    pub max_branches: usize,         // default: 50
    /// Default hook policy.
    pub hook_policy: HookPolicy,     // default: Respect
}
```

### 3.2 Core Operations

```rust
impl GitRepo {
    /// Open an existing repository at the given path.
    pub fn open(root: &Path) -> Result<Self, GitError> {
        let repo = Repository::discover(root)?;
        let config = GitRepoConfig::default();
        Ok(Self { root: root.to_path_buf(), repo, config })
    }

    /// Create a new branch from the current HEAD.
    pub fn create_branch(&self, name: &str) -> Result<Branch, GitError> {
        let full_name = format!("{}{}", self.config.branch_prefix, name);
        let head = self.repo.head()?;
        let target_commit = head.peel_to_commit()?;
        let branch = self.repo.branch(&full_name, &target_commit, false)?;
        Ok(branch)
    }

    /// Switch to a branch.
    pub fn checkout(&self, branch_name: &str) -> Result<(), GitError> {
        let full_name = format!("{}{}", self.config.branch_prefix, branch_name);
        let branch = self.repo.find_branch(&full_name, BranchType::Local)?;
        let target = branch.get().peel_to_commit()?;
        self.repo.checkout_tree(&target.into_object(), None)?;
        self.repo.set_head(&format!("refs/heads/{}", full_name))?;
        Ok(())
    }

    /// Stage all changes in the working tree.
    pub fn stage_all(&self) -> Result<Index, GitError> {
        let mut index = self.repo.index()?;
        index.add_all(["*"], IndexAddOption::DEFAULT, None)?;
        index.write()?;
        Ok(index)
    }

    /// Stage specific paths.
    pub fn stage_paths(&self, paths: &[&Path]) -> Result<Index, GitError> {
        let mut index = self.repo.index()?;
        for path in paths {
            index.add_path(path)?;
        }
        index.write()?;
        Ok(index)
    }

    /// Commit staged changes with the given message.
    pub fn commit(&self, message: &str) -> Result<Oid, GitError> {
        let mut index = self.repo.index()?;
        let tree_id = index.write_tree()?;
        let tree = self.repo.find_tree(tree_id)?;

        let head = self.repo.head()?;
        let parent = head.peel_to_commit()?;

        let sig = Signature::now(
            &self.config.author_name,
            &self.config.author_email,
        )?;

        let oid = self.repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            message,
            &tree,
            &[&parent],
        )?;

        Ok(oid)
    }

    /// Restore the working tree to the state at HEAD (discard all changes).
    pub fn restore(&self) -> Result<(), GitError> {
        let head = self.repo.head()?;
        let target = head.peel_to_commit()?;
        self.repo.checkout_tree(&target.into_object(), None)?;
        self.repo.cleanup_state()?;
        Ok(())
    }

    /// Get the current status of the working tree.
    pub fn status(&self) -> Result<GitStatus, GitError> {
        let statuses = self.repo.statuses(None)?;
        let mut result = GitStatus::default();

        for entry in &statuses {
            let path = PathBuf::from(entry.path().unwrap_or(""));
            match entry.status() {
                s if s.is_wt_new()     => result.untracked.push(path),
                s if s.is_wt_modified() => result.modified.push(path),
                s if s.is_wt_deleted()  => result.deleted.push(path),
                s if s.is_index_new()   => result.staged_new.push(path),
                s if s.is_index_modified() => result.staged_modified.push(path),
                s if s.is_index_deleted()  => result.staged_deleted.push(path),
                _ => {}
            }
        }
        Ok(result)
    }

    /// Check if the working tree is clean (no modifications).
    pub fn is_clean(&self) -> Result<bool, GitError> {
        let status = self.status()?;
        Ok(status.is_clean())
    }
}

#[derive(Debug, Default)]
pub struct GitStatus {
    pub untracked: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
    pub staged_new: Vec<PathBuf>,
    pub staged_modified: Vec<PathBuf>,
    pub staged_deleted: Vec<PathBuf>,
}

impl GitStatus {
    pub fn is_clean(&self) -> bool {
        self.untracked.is_empty()
            && self.modified.is_empty()
            && self.deleted.is_empty()
            && self.staged_new.is_empty()
            && self.staged_modified.is_empty()
            && self.staged_deleted.is_empty()
    }
}
```

---

## 4. WorktreeGuard — Isolated Working Trees

### 4.1 Purpose

A `WorktreeGuard` creates an isolated Git worktree so that `xaft` can make changes without affecting the user's main working tree. When the guard is dropped, the worktree is cleaned up.

```
 ┌───────────────────────────────────────────────────────────┐
 │                   Repository Layout                       │
 │                                                           │
 │  /my-project/                  (main working tree)        │
 │    ├── .git/                   (shared git objects)       │
 │    ├── src/                                                │
 │    └── Cargo.toml                                          │
 │                                                           │
 │  /tmp/xaft-worktree-abc123/   (isolated worktree)        │
 │    ├── .git  → /my-project/.git/worktrees/wt-abc123/     │
 │    ├── src/                     (copy, linked to .git)    │
 │    └── Cargo.toml               (copy, linked to .git)   │
 │                                                           │
 └───────────────────────────────────────────────────────────┘
```

### 4.2 Implementation

```rust
/// RAII guard for an isolated Git worktree.
/// Creates the worktree on construction; removes it on drop.
pub struct WorktreeGuard {
    /// Path to the worktree directory.
    worktree_path: PathBuf,
    /// Name of the worktree within Git.
    worktree_name: String,
    /// The branch checked out in this worktree.
    branch_name: String,
    /// Reference to the parent repository.
    repo: GitRepo,
    /// Whether to merge changes back on successful completion.
    merge_on_success: bool,
    /// Whether the guard has been explicitly released.
    released: bool,
}

impl WorktreeGuard {
    /// Create a new isolated worktree.
    pub async fn create(
        repo: &GitRepo,
        branch_name: &str,
        config: WorktreeConfig,
    ) -> Result<Self, WorktreeError> {
        let worktree_name = format!("wt-{}", Uuid::new_v4().as_simple());
        let worktree_path = config.base_dir.join(&worktree_name);

        // Create the branch from current HEAD
        repo.create_branch(branch_name)?;

        // Create the worktree
        let git2_repo = &repo.repo;
        let worktree = git2_repo.worktree(
            &worktree_name,
            &worktree_path,
            Some(&WorktreeAddOptions::new().reference(Some(
                &git2_repo.find_branch(
                    &format!("{}{}", repo.config.branch_prefix, branch_name),
                    BranchType::Local,
                )?.into_reference()
            ))),
        )?;

        Ok(Self {
            worktree_path,
            worktree_name,
            branch_name: branch_name.to_string(),
            repo: repo.clone(),
            merge_on_success: config.merge_on_success,
            released: false,
        })
    }

    /// Get the path to the worktree (for passing to tools).
    pub fn path(&self) -> &Path {
        &self.worktree_path
    }

    /// Explicitly release the worktree, optionally merging the branch back.
    pub async fn release(mut self, outcome: WorktreeOutcome) -> Result<(), WorktreeError> {
        self.released = true;
        match outcome {
            WorktreeOutcome::Success => {
                if self.merge_on_success {
                    self.merge_branch()?;
                }
                self.cleanup()?;
            }
            WorktreeOutcome::Discard => {
                self.cleanup()?;
            }
        }
        Ok(())
    }

    fn merge_branch(&self) -> Result<(), WorktreeError> {
        // Switch to main branch and merge the xaft branch
        let full_branch = format!("{}{}", self.repo.config.branch_prefix, self.branch_name);
        // ... merge logic using git2 ...
        Ok(())
    }

    fn cleanup(&self) -> Result<(), WorktreeError> {
        // Prune the worktree
        self.repo.repo.worktree_prune(&self.worktree_name)?;
        // Remove the worktree directory
        if self.worktree_path.exists() {
            fs::remove_dir_all(&self.worktree_path)?;
        }
        // Optionally delete the xaft branch
        // ...
        Ok(())
    }
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        if !self.released {
            // Safety cleanup: discard worktree changes
            tracing::warn!(
                "WorktreeGuard dropped without explicit release; discarding changes"
            );
            let _ = self.cleanup();
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorktreeConfig {
    /// Base directory for worktree creation.
    pub base_dir: PathBuf,           // default: std::env::temp_dir()
    /// Whether to merge the branch back on success.
    pub merge_on_success: bool,      // default: true
    /// Whether to delete the xaft branch after merge.
    pub delete_branch_after_merge: bool, // default: true
}

pub enum WorktreeOutcome {
    /// Changes were successful; merge back to original branch.
    Success,
    /// Discard all changes; clean up worktree.
    Discard,
}
```

---

## 5. Branch→Edit→Verify→Commit/Restore Pattern

### 5.1 Pattern Flow

```
 ┌──────────────────────────────────────────────────────────────────────┐
 │                                                                      │
 │   ┌──────────┐     ┌──────────┐     ┌──────────┐     ┌──────────┐ │
 │   │  Branch  │────▶│   Edit   │────▶│  Verify  │────▶│  Commit  │ │
 │   │  (from   │     │ (apply   │     │  (test   │     │  (atomic │ │
 │   │   HEAD)  │     │  changes)│     │  suite)  │     │  commit) │ │
 │   └──────────┘     └────┬─────┘     └────┬─────┘     └──────────┘ │
 │                         │                 │                         │
 │                    edit failed       verify failed                  │
 │                         │                 │                         │
 │                         ▼                 ▼                         │
 │                   ┌───────────┐     ┌───────────┐                  │
 │                   │  Restore  │     │  Restore  │                  │
 │                   │ (git      │     │ (git      │                  │
 │                   │  checkout)│     │  checkout)│                  │
 │                   └───────────┘     └───────────┘                  │
 │                                                                      │
 └──────────────────────────────────────────────────────────────────────┘
```

### 5.2 Per-Step Implementation

```rust
/// Executor that applies the branch→edit→verify→commit/restore pattern
/// for each plan step.
pub struct GitAwareStepExecutor {
    repo: GitRepo,
    worktree: Option<WorktreeGuard>,
    commit_strategy: CommitStrategy,
    verify_command: Option<String>,
}

#[derive(Debug, Clone)]
pub enum CommitStrategy {
    /// Commit after every successful step.
    PerStep,
    /// Commit after all steps complete.
    PerPlan,
    /// Commit after every N steps.
    EveryN(usize),
    /// Never commit (changes stay in working tree).
    Never,
}

impl GitAwareStepExecutor {
    pub async fn execute_step(
        &mut self,
        step: &Step,
    ) -> Result<StepResult, StepError> {
        let working_dir = self.worktree
            .as_ref()
            .map(|wt| wt.path().to_path_buf())
            .unwrap_or_else(|| self.repo.root.clone());

        // ── BRANCH (implicit: we're already on the xaft branch) ──

        // ── EDIT ──
        let edit_result = self.apply_edit(step, &working_dir).await;
        if let Err(e) = edit_result {
            // Restore to clean state
            self.repo.restore()?;
            return Ok(StepResult::Failed {
                error: e.to_string(),
                recoverable: true,
            });
        }

        // ── VERIFY ──
        if let Some(ref cmd) = self.verify_command {
            let verify_result = self.run_verify(cmd, &working_dir).await;
            if let Err(e) = verify_result {
                // Restore: verification failed
                self.repo.restore()?;
                return Ok(StepResult::Failed {
                    error: format!("Verification failed: {}", e),
                    recoverable: true,
                });
            }
        }

        // ── COMMIT ──
        match self.commit_strategy {
            CommitStrategy::PerStep => {
                let message = self.generate_commit_message(step);
                self.repo.stage_all()?;
                self.repo.commit(&message)?;
            }
            CommitStrategy::EveryN(n) => {
                self.step_count += 1;
                if self.step_count % n == 0 {
                    let message = self.generate_batch_commit_message();
                    self.repo.stage_all()?;
                    self.repo.commit(&message)?;
                }
            }
            _ => {} // PerPlan and Never: don't commit now
        }

        Ok(StepResult::Success(StepOutput::default()))
    }

    async fn apply_edit(
        &self,
        step: &Step,
        working_dir: &Path,
    ) -> Result<(), StepError> {
        match &step.tool {
            ToolHint::FileEdit { path } => {
                // FileEditor handles the actual editing
                Ok(())
            }
            ToolHint::ShellCommand { command } => {
                // ShellCommand executes in the working directory
                Ok(())
            }
            _ => Err(StepError::UnsupportedTool),
        }
    }
}
```

---

## 6. HeuristicMessageGenerator

### 6.1 Commit Message Generation

`xaft` generates structured commit messages from plan steps, not from LLM free-text. This ensures messages are deterministic and searchable.

```rust
pub struct HeuristicMessageGenerator {
    template: CommitTemplate,
}

#[derive(Debug, Clone)]
pub struct CommitTemplate {
    /// Template for step commits.
    pub step_template: String,   // default: "xaft({step_id}): {description}"
    /// Template for batch commits.
    pub batch_template: String,  // default: "xaft(batch {batch_id}): steps {step_range}"
    /// Template for plan completion commits.
    pub plan_template: String,   // default: "xaft(plan {plan_id}): {intent_goal}"
    /// Maximum subject line length.
    pub max_subject_len: usize,  // default: 72
    /// Whether to include a machine-readable trailer.
    pub include_trailer: bool,   // default: true
}

impl HeuristicMessageGenerator {
    pub fn generate_step_message(
        &self,
        step: &Step,
        plan_id: &PlanId,
        diff_summary: &DiffSummary,
    ) -> String {
        let mut subject = self.template.step_template
            .replace("{step_id}", &step.id.to_string())
            .replace("{description}", &step.description);

        // Truncate to max subject length
        subject.truncate(self.template.max_subject_len);

        let mut body = String::new();

        // Add diff stats
        body.push_str(&format!(
            "\nFiles changed: {} insertions(+), {} deletions(-)",
            diff_summary.insertions, diff_summary.deletions
        ));

        // Add affected file list
        for file in &diff_summary.files {
            body.push_str(&format!("\n  {}", file.display()));
        }

        // Add machine-readable trailer
        if self.template.include_trailer {
            body.push_str(&format!(
                "\n\nXaft-Plan-Id: {}\
                 \nXaft-Step-Id: {}\
                 \nXaft-Version: {}",
                plan_id, step.id, env!("CARGO_PKG_VERSION")
            ));
        }

        format!("{}\n{}", subject, body)
    }

    pub fn generate_plan_message(
        &self,
        plan: &Plan,
        intent: &Intent,
    ) -> String {
        let mut subject = self.template.plan_template
            .replace("{plan_id}", &plan.id.to_string())
            .replace("{intent_goal}", &intent.goal);

        subject.truncate(self.template.max_subject_len);

        let mut body = String::from("\nPlan steps:\n");
        for (i, step) in plan.steps.iter().enumerate() {
            body.push_str(&format!("  {}. {}\n", i + 1, step.description));
        }

        if self.template.include_trailer {
            body.push_str(&format!(
                "\nXaft-Plan-Id: {}\
                 \nXaft-Steps: {}\
                 \nXaft-Intent-Hash: {:016x}",
                plan.id,
                plan.steps.len(),
                plan.intent_hash,
            ));
        }

        format!("{}\n{}", subject, body)
    }
}

#[derive(Debug, Default)]
pub struct DiffSummary {
    pub files: Vec<PathBuf>,
    pub insertions: usize,
    pub deletions: usize,
}
```

### 6.2 Message Examples

```
xaft(step-4): Add /api/login endpoint in src/api/auth.rs

Files changed: 23 insertions(+), 0 deletions(-)
  src/api/auth.rs
  src/api/mod.rs

Xaft-Plan-Id: 7f3a2b1c
Xaft-Step-Id: step-4
Xaft-Version: 0.1.0
```

```
xaft(plan 7f3a2b1c): Add JWT authentication to /api endpoints

Plan steps:
  1. Add `jsonwebtoken` to Cargo.toml
  2. Create src/middleware/jwt.rs
  3. Integrate JWT middleware in src/api/mod.rs
  4. Add /api/login endpoint in src/api/auth.rs
  5. Add integration tests in tests/api_jwt.rs
  6. Run `cargo test` to verify

Xaft-Plan-Id: 7f3a2b1c
Xaft-Steps: 6
Xaft-Intent-Hash: a1b2c3d4e5f67890
```

---

## 7. HookPolicy

Git hooks can interfere with `xaft`'s automated commits. The `HookPolicy` controls how hooks are handled.

### 7.1 Policy Options

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookPolicy {
    /// Respect all Git hooks. If a hook fails, the operation fails.
    Respect,

    /// Skip all Git hooks during xaft operations.
    /// Hooks are restored after xaft finishes.
    Skip,

    /// Run hooks but only warn on failure (don't abort the operation).
    /// Capture hook output for display in the TUI.
    Warn,
}
```

### 7.2 Hook Interception

```
 ┌──────────────────────────────────────────────────────────┐
 │                  Hook Interception Flow                   │
 │                                                          │
 │  xaft commit request                                    │
 │         │                                                │
 │         ▼                                                │
 │  ┌─────────────────┐                                    │
 │  │ Check HookPolicy│                                    │
 │  └────────┬────────┘                                    │
 │           │                                              │
 │     ┌─────┼──────────┬─────────────┐                    │
 │     ▼     ▼          ▼             ▼                    │
 │  Respect   Skip       Warn                            │
 │     │     │          │                                 │
 │     ▼     ▼          ▼                                 │
 │  Run     Temporarily  Run hook                         │
 │  hooks   disable     capture result                    │
 │  normally hooks      │                                 │
 │     │     │     ┌────┴────┐                            │
 │     │     │     │         │                            │
 │     │     │   Success   Failure                        │
 │     │     │     │         │                            │
 │     │     │     ▼         ▼                            │
 │     │     │  Continue   Log warning                    │
 │     │     │             Continue                       │
 │     │     │                                             │
 │     │     ▼                                             │
 │     │  Re-enable hooks                                  │
 │     ▼                                                   │
 │   Hook result determines operation outcome              │
 └──────────────────────────────────────────────────────────┘
```

### 7.3 Implementation

```rust
pub struct HookInterceptor {
    policy: HookPolicy,
    hooks_dir: PathBuf,
    backup_dir: PathBuf,
}

impl HookInterceptor {
    /// Temporarily disable hooks (for Skip policy).
    pub fn disable_hooks(&self) -> Result<(), HookError> {
        if self.policy != HookPolicy::Skip {
            return Ok(());
        }

        if !self.backup_dir.exists() {
            fs::create_dir_all(&self.backup_dir)?;
        }

        // Move each hook file to backup
        for entry in fs::read_dir(&self.hooks_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            // Only move standard hook names
            if is_standard_hook(&name_str) {
                let src = entry.path();
                let dst = self.backup_dir.join(&name);
                fs::rename(&src, &dst)?;
                tracing::debug!("Disabled hook: {}", name_str);
            }
        }
        Ok(())
    }

    /// Re-enable hooks after operation.
    pub fn restore_hooks(&self) -> Result<(), HookError> {
        if self.policy != HookPolicy::Skip {
            return Ok(());
        }

        for entry in fs::read_dir(&self.backup_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let src = entry.path();
            let dst = self.hooks_dir.join(&name);
            fs::rename(&src, &dst)?;
            tracing::debug!("Restored hook: {:?}", name);
        }
        Ok(())
    }

    /// Run a hook and handle the result according to policy.
    pub fn run_hook(&self, hook_name: &str, args: &[&str]) -> Result<HookResult, HookError> {
        let hook_path = self.hooks_dir.join(hook_name);

        if !hook_path.exists() {
            return Ok(HookResult::NoHook);
        }

        match self.policy {
            HookPolicy::Skip => Ok(HookResult::Skipped),
            HookPolicy::Respect => {
                let output = Command::new(&hook_path)
                    .args(args)
                    .output()?;

                if output.status.success() {
                    Ok(HookResult::Success {
                        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                    })
                } else {
                    Err(HookError::HookFailed {
                        hook: hook_name.to_string(),
                        exit_code: output.status.code(),
                        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                    })
                }
            }
            HookPolicy::Warn => {
                let output = Command::new(&hook_path)
                    .args(args)
                    .output();

                match output {
                    Ok(out) if out.status.success() => {
                        Ok(HookResult::Success {
                            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
                        })
                    }
                    Ok(out) => {
                        tracing::warn!(
                            "Git hook '{}' failed (continuing due to Warn policy): {}",
                            hook_name,
                            String::from_utf8_lossy(&out.stderr)
                        );
                        Ok(HookResult::FailedButContinued {
                            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
                        })
                    }
                    Err(e) => {
                        tracing::warn!("Git hook '{}' execution error: {}", hook_name, e);
                        Ok(HookResult::FailedButContinued {
                            stderr: e.to_string(),
                        })
                    }
                }
            }
        }
    }
}

fn is_standard_hook(name: &str) -> bool {
    matches!(
        name,
        "pre-commit" | "prepare-commit-msg" | "commit-msg"
        | "post-commit" | "pre-push" | "pre-rebase"
    )
}
```

---

## 8. Agent Lifecycle Integration

### 8.1 Lifecycle Hooks

```rust
/// Integration points between the Git system and the agent lifecycle.
pub trait GitLifecycle: Send + Sync {
    /// Called before a step begins execution.
    /// Returns a checkpoint that can be rolled back to.
    fn on_step_start(&self, step: &Step) -> Result<Checkpoint, GitError>;

    /// Called after a step completes.
    fn on_step_finish(
        &self,
        step: &Step,
        result: &StepResult,
        checkpoint: Checkpoint,
    ) -> Result<(), GitError>;

    /// Called when the entire plan completes successfully.
    fn on_plan_complete(&self, plan: &Plan, intent: &Intent) -> Result<(), GitError>;

    /// Called when the plan is aborted (user or error).
    fn on_plan_abort(&self, plan: &Plan, completed_steps: &[StepId]) -> Result<(), GitError>;
}

/// A snapshot of repository state at a point in time.
#[derive(Debug, Clone)]
pub struct Checkpoint {
    /// The commit SHA at the time of checkpoint creation.
    pub commit_sha: Oid,
    /// Staged files snapshot (for restoring staged state).
    pub staged_files: Vec<PathBuf>,
    /// Timestamp of checkpoint creation.
    pub created_at: DateTime<Utc>,
}

pub struct DefaultGitLifecycle {
    repo: GitRepo,
    commit_strategy: CommitStrategy,
    message_gen: HeuristicMessageGenerator,
}

impl GitLifecycle for DefaultGitLifecycle {
    fn on_step_start(&self, step: &Step) -> Result<Checkpoint, GitError> {
        let head = self.repo.repo.head()?;
        let commit = head.peel_to_commit()?;

        // Capture current staged state
        let status = self.repo.status()?;
        let staged_files = status.staged_new
            .iter()
            .chain(status.staged_modified.iter())
            .chain(status.staged_deleted.iter())
            .cloned()
            .collect();

        Ok(Checkpoint {
            commit_sha: commit.id(),
            staged_files,
            created_at: Utc::now(),
        })
    }

    fn on_step_finish(
        &self,
        step: &Step,
        result: &StepResult,
        checkpoint: Checkpoint,
    ) -> Result<(), GitError> {
        match result {
            StepResult::Success(_) => {
                if matches!(self.commit_strategy, CommitStrategy::PerStep) {
                    let message = self.message_gen.generate_step_message(
                        step,
                        &PlanId::default(), // filled in by caller
                        &DiffSummary::default(),
                    );
                    self.repo.stage_all()?;
                    self.repo.commit(&message)?;
                }
            }
            StepResult::Failed { .. } => {
                // Restore to checkpoint
                self.repo.restore()?;
                tracing::info!(
                    "Restored to checkpoint {} after step failure",
                    checkpoint.commit_sha
                );
            }
            StepResult::NeedsReplan { .. } => {
                // Keep changes staged but don't commit
                // Replanner will see the current state
            }
        }
        Ok(())
    }

    fn on_plan_complete(&self, plan: &Plan, intent: &Intent) -> Result<(), GitError> {
        if matches!(self.commit_strategy, CommitStrategy::PerPlan) {
            let message = self.message_gen.generate_plan_message(plan, intent);
            self.repo.stage_all()?;
            self.repo.commit(&message)?;
        }
        Ok(())
    }

    fn on_plan_abort(&self, plan: &Plan, completed_steps: &[StepId]) -> Result<(), GitError> {
        // Restore to the state before the plan started
        self.repo.restore()?;
        tracing::info!(
            "Plan aborted; restored repository. Completed steps: {:?}",
            completed_steps
        );
        Ok(())
    }
}
```

---

## 9. Git Signals

### 9.1 Signal Types

```rust
/// Events emitted by the Git integration layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GitSignal {
    /// A branch was created.
    BranchCreated {
        name: String,
        from_commit: String,
    },

    /// A commit was made.
    CommitCreated {
        sha: String,
        message: String,
        files_changed: Vec<PathBuf>,
        insertions: usize,
        deletions: usize,
    },

    /// Working tree was restored to a checkpoint.
    Restored {
        to_commit: String,
        reason: String,
    },

    /// A worktree was created.
    WorktreeCreated {
        path: PathBuf,
        branch: String,
    },

    /// A worktree was removed.
    WorktreeRemoved {
        path: PathBuf,
        merged: bool,
    },

    /// A hook was executed.
    HookExecuted {
        name: String,
        result: HookResult,
    },

    /// Merge conflict detected.
    MergeConflict {
        files: Vec<PathBuf>,
    },

    /// Repository status changed.
    StatusChanged {
        from: GitStatus,
        to: GitStatus,
    },
}
```

### 9.2 Signal Bus Integration

```rust
pub struct GitSignalBus {
    sender: broadcast::Sender<GitSignal>,
}

impl GitSignalBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    pub fn emit(&self, signal: GitSignal) {
        let _ = self.sender.send(signal);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<GitSignal> {
        self.sender.subscribe()
    }
}
```

---

## 10. TUI Presentation

### 10.1 Git Status Panel

```
┌─ Git ────────────────────────────────────────────────────────────────┐
│                                                                      │
│  Branch: xaft/jwt-auth-7f3a2b1c                                     │
│  Base:   main (a1b2c3d)                                             │
│  Status: 2 commits ahead, 0 behind                                  │
│                                                                      │
│  ┌─ Commits ─────────────────────────────────────────────────────┐  │
│  │ abc1234 xaft(step-1): Add `jsonwebtoken` to Cargo.toml       │  │
│  │ def5678 xaft(step-2): Create src/middleware/jwt.rs           │  │
│  └───────────────────────────────────────────────────────────────┘  │
│                                                                      │
│  ┌─ Unstaged Changes ───────────────────────────────────────────┐  │
│  │ M  src/api/mod.rs                                            │  │
│  │ M  src/api/auth.rs                                           │  │
│  └───────────────────────────────────────────────────────────────┘  │
│                                                                      │
│  Worktree: /tmp/xaft-worktree-e4f5g6h7                              │
│  Hook Policy: Warn                                                   │
│                                                                      │
│  [L] Show Log   [D] Show Diff   [R] Restore   [M] Merge to Main    │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

### 10.2 Diff View within Git Panel

```
┌─ Git Diff (abc1234..def5678) ────────────────────────────────────────┐
│                                                                      │
│  src/middleware/jwt.rs (new file)                                    │
│  ────────────────────────────────────                                │
│  + 1  use jsonwebtoken::{encode, decode, Header, Validation};        │
│  + 2  use serde::{Serialize, Deserialize};                           │
│  + 3                                                                 │
│  + 4  #[derive(Serialize, Deserialize)]                              │
│  + 5  pub struct Claims {                                            │
│  + 6      pub sub: String,                                           │
│  + 7      pub exp: usize,                                            │
│  + 8  }                                                              │
│  + 9                                                                 │
│  +10  pub fn create_token(sub: &str, secret: &str) -> Result<..> {  │
│  +11      let claims = Claims {                                      │
│  +12          sub: sub.to_string(),                                  │
│  +13          exp: (Utc::now() + Duration::hours(24)).timestamp().. │
│  +14      };                                                         │
│  +15      encode(&Header::default(), &claims, secret.as_bytes())     │
│  +16  }                                                              │
│                                                                      │
│  3 files changed, 42 insertions(+), 2 deletions(-)                  │
│                                                                      │
│  [↑↓] Scroll   [Tab] Next File   [Esc] Close                       │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

---

## 11. Configuration

```toml
# .xaft.toml

[git]
author_name = "xaft"
author_email = "xaft@local"
branch_prefix = "xaft/"
sign_commits = false
max_branches = 50
hook_policy = "respect"           # respect | skip | warn

[git.worktree]
enabled = true
base_dir = ""                     # empty = system temp dir
merge_on_success = true
delete_branch_after_merge = true

[git.commit]
strategy = "per_step"             # per_step | per_plan | every_n | never
step_template = "xaft({step_id}): {description}"
plan_template = "xaft(plan {plan_id}): {intent_goal}"
max_subject_len = 72
include_trailer = true

[git.lifecycle]
auto_restore_on_failure = true
auto_commit_on_success = true
checkpoint_on_step_start = true
```

---

## 12. Error Taxonomy

| Error                              | Code   | Recovery                                    |
|------------------------------------|--------|---------------------------------------------|
| `GitError::NotARepository`        | G-001  | Prompt user to initialize or specify path   |
| `GitError::DirtyWorkingTree`      | G-002  | Stash or commit changes before xaft starts  |
| `GitError::BranchExists`          | G-003  | Generate unique branch name with UUID       |
| `GitError::WorktreeCreation`      | G-004  | Fall back to in-place editing               |
| `GitError::MergeConflict`         | G-005  | Present conflict to user; suggest manual    |
| `HookError::HookFailed`           | G-006  | Depends on HookPolicy (block/warn/skip)     |
| `GitError::RestoreFailed`         | G-007  | Alert user; manual intervention required    |

---

## 13. Future Considerations

1. **Git-absorb integration** — Auto-squash xaft commits into logical units.
2. **Branch PR automation** — Auto-create pull requests for xaft branches.
3. **Multi-repo support** — Coordinated changes across multiple repositories.
4. **Git LFS awareness** — Handle large file storage during xaft operations.
5. **Signed commit chains** — End-to-end verification of xaft-generated history.
