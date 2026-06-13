//! `@`-mention file/directory trigger handler.
//!
//! Migrated from `AppState::refresh_autocomplete()` and
//! `AppState::autocomplete_complete()` in `state.rs`.

use std::path::PathBuf;
use std::sync::Arc;

use agtrs_workspace::WorkspaceStore;
use xaft_config::MentionConfig;

use crate::trigger::{MatchItem, MatchKind, TriggerContext, TriggerHandler};

/// Trigger handler for `@`-mention file/directory completion.
///
/// Constructed with no workspace in `AppState::new()` (returns empty matches).
/// `AppState::init_mention()` replaces it via `TriggerRegistry::replace()`
/// with a fully-initialised handler once the workspace root is known.
pub struct MentionTriggerHandler {
    workspace: Option<Arc<dyn WorkspaceStore>>,
    workspace_root: Option<PathBuf>,
    #[allow(dead_code)]
    config: MentionConfig,
}

impl MentionTriggerHandler {
    /// Construct with no workspace (placeholder; `AppState::init_mention()`
    /// replaces this via `TriggerRegistry::replace()`).
    pub fn new(config: MentionConfig) -> Self {
        Self {
            workspace: None,
            workspace_root: None,
            config,
        }
    }

    /// Construct with a fully initialised workspace.
    pub fn with_workspace(
        workspace: Arc<dyn WorkspaceStore>,
        workspace_root: PathBuf,
        config: MentionConfig,
    ) -> Self {
        Self {
            workspace: Some(workspace),
            workspace_root: Some(workspace_root),
            config,
        }
    }
}

impl TriggerHandler for MentionTriggerHandler {
    fn trigger_char(&self) -> char {
        '@'
    }

    /// Populate candidates by listing the directory implied by `ctx.dir_prefix`.
    ///
    /// Exact logic migrated from `AppState::refresh_autocomplete()`.
    fn matches(&self, ctx: &TriggerContext<'_>) -> Vec<MatchItem> {
        let workspace_root = match &self.workspace_root {
            Some(r) => r.clone(),
            None => return vec![],
        };

        // Require at least one character after '@' to avoid spamming on bare '@'.
        // (The workspace's root listing could be large; let the user type at least
        // one char to narrow it down. This preserves pre-migration behaviour.)
        // Actually per the PRD and current code, bare '@' should still list root.
        // We keep the same behaviour: list when workspace is known.

        let dir_to_list = if ctx.dir_prefix.is_empty() {
            workspace_root.clone()
        } else {
            workspace_root.join(ctx.dir_prefix.trim_end_matches('/'))
        };

        let file_prefix = match ctx.prefix.rfind('/') {
            Some(slash) => &ctx.prefix[slash + 1..],
            None => ctx.prefix,
        };

        let raw_entries = match std::fs::read_dir(&dir_to_list) {
            Ok(rd) => rd.filter_map(|e| e.ok()).collect::<Vec<_>>(),
            Err(_) => return vec![],
        };

        let file_prefix_lower = file_prefix.to_lowercase();
        let mut items: Vec<MatchItem> = raw_entries
            .iter()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                // Skip hidden entries unless the user explicitly starts with '.'.
                if name.starts_with('.') && !file_prefix.starts_with('.') {
                    return None;
                }
                // Prefix match (case-insensitive).
                if !name.to_lowercase().starts_with(&file_prefix_lower) {
                    return None;
                }
                let is_dir = e.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                let (display_name, kind) = if is_dir {
                    (format!("{name}/"), MatchKind::Directory)
                } else {
                    (name.clone(), MatchKind::File)
                };
                let insert = if is_dir {
                    format!("@{}{}/", ctx.dir_prefix, name)
                } else {
                    format!("@{}{} ", ctx.dir_prefix, name)
                };
                Some(MatchItem {
                    display: display_name,
                    insert,
                    hint: None,
                    kind,
                })
            })
            .collect();

        // Sort: directories first, then files, both alphabetically.
        items.sort_by(|a, b| {
            let a_dir = a.kind == MatchKind::Directory;
            let b_dir = b.kind == MatchKind::Directory;
            match (a_dir, b_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.display.cmp(&b.display),
            }
        });
        items.truncate(10);
        items
    }

    fn on_select(&self, item: &MatchItem, _ctx: &TriggerContext<'_>) -> String {
        // `item.insert` already contains the fully-formed `@<path>` or `@<dir>/`
        // string constructed in `matches()`.
        item.insert.clone()
    }

    fn allows_free_text(&self) -> bool {
        false
    }

    fn max_visible(&self) -> usize {
        10
    }
}

// Make the compiler happy — workspace field needs to be used somewhere
impl Drop for MentionTriggerHandler {
    fn drop(&mut self) {
        // workspace field is intentionally held for lifetime
        let _ = &self.workspace;
    }
}
