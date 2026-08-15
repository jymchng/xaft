//! `$`-skill trigger handler — opens a skill-only dropdown.
//!
//! agenthicc parity (src/agenthicc/tui/triggers/slash_command.py,
//! `class SkillTrigger(SlashCommandTrigger): char = "$"; skill_only = True`):
//! the `$` trigger opens a picker listing only skills (never slash commands).
//! Skills are loaded via `xaft_skills::SkillLoader`; the handler caches the
//! last-loaded list so `matches()` stays cheap (no async I/O on each keystroke).

use std::path::PathBuf;

use xaft_skills::{Skill, SkillLoader};

use crate::trigger::{MatchItem, MatchKind, TriggerContext, TriggerHandler};

/// Trigger handler for `$` — skill-only dropdown.
///
/// Constructed with a working directory so it can resolve `.xaft/skills/`
/// and `~/.config/xaft/skills/`. `matches()` filters loaded skills by the
/// typed prefix (case-insensitive) and returns them as `MatchKind::Custom`
/// rows whose `insert` is `$<name> `.
pub struct SkillTriggerHandler {
    /// Reserved for future skill reload; kept to carry the working dir.
    #[allow(dead_code)]
    working_dir: PathBuf,
    /// Cached skills (refreshed on construction; `matches()` is synchronous).
    skills: Vec<Skill>,
}

impl SkillTriggerHandler {
    /// Create a handler for `working_dir`. Skills are loaded eagerly (the
    /// loader is async, so this runs once at construction time).
    pub async fn new(working_dir: PathBuf) -> Self {
        let loader = SkillLoader::for_working_dir(&working_dir);
        let skills = loader.load_all().await;
        Self {
            working_dir,
            skills,
        }
    }

    /// Create a handler with an explicit pre-loaded skill list (for tests and
    /// registries that already have skills).
    pub fn with_skills(working_dir: PathBuf, skills: Vec<Skill>) -> Self {
        Self {
            working_dir,
            skills,
        }
    }

    /// Create a handler from a borrowed skill slice (used by
    /// `AppState::init_skills` which receives `Vec<xaft_skills::Skill>`).
    pub fn from_skills(skills: &[Skill]) -> Self {
        Self {
            working_dir: PathBuf::new(),
            skills: skills.to_vec(),
        }
    }

    /// The loaded skills (for tests / tooling).
    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }
}

impl TriggerHandler for SkillTriggerHandler {
    fn trigger_char(&self) -> char {
        '$'
    }

    fn matches(&self, ctx: &TriggerContext<'_>) -> Vec<MatchItem> {
        let prefix_lower = ctx.prefix.to_lowercase();
        self.skills
            .iter()
            .filter(|s| {
                s.name.to_lowercase().starts_with(&prefix_lower)
                    || s.description.to_lowercase().contains(&prefix_lower)
            })
            .map(|s| MatchItem {
                display: format!("${}", s.name),
                insert: format!("${} ", s.name),
                hint: if s.description.is_empty() {
                    None
                } else {
                    Some(s.description.clone())
                },
                kind: MatchKind::Custom("skill".into()),
            })
            .collect()
    }

    fn on_select(&self, item: &MatchItem, _ctx: &TriggerContext<'_>) -> String {
        item.insert.clone()
    }

    fn allows_free_text(&self) -> bool {
        true
    }

    fn max_visible(&self) -> usize {
        10
    }
}

// Keep the field used (future: reload on refresh).
#[allow(dead_code)]
fn _touch(_p: &PathBuf) {}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trigger::TriggerContext;
    use std::path::Path;

    fn sample_skills() -> Vec<Skill> {
        vec![
            Skill {
                name: "rust-review".into(),
                description: "Rust code review checklist".into(),
                tags: vec![],
                body: "review".into(),
                source_path: "/tmp/rust-review.md".into(),
                priority: 0,
            },
            Skill {
                name: "commit-msg".into(),
                description: "Conventional commit message format".into(),
                tags: vec![],
                body: "commit".into(),
                source_path: "/tmp/commit-msg.md".into(),
                priority: 0,
            },
        ]
    }

    fn ctx(prefix: &str) -> TriggerContext<'_> {
        TriggerContext {
            prefix,
            dir_prefix: "",
            working_dir: Path::new("/tmp"),
            terminal_cols: 80,
        }
    }

    #[test]
    fn trigger_char_is_dollar() {
        let h = SkillTriggerHandler::with_skills(PathBuf::from("/tmp"), sample_skills());
        assert_eq!(h.trigger_char(), '$');
    }

    #[test]
    fn matches_filters_by_name_prefix() {
        let h = SkillTriggerHandler::with_skills(PathBuf::from("/tmp"), sample_skills());
        let items = h.matches(&ctx("rust"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].display, "$rust-review");
        assert_eq!(items[0].insert, "$rust-review ");
        assert_eq!(items[0].kind, MatchKind::Custom("skill".into()));
    }

    #[test]
    fn matches_case_insensitive() {
        let h = SkillTriggerHandler::with_skills(PathBuf::from("/tmp"), sample_skills());
        let items = h.matches(&ctx("COMMIT"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].display, "$commit-msg");
    }

    #[test]
    fn matches_empty_prefix_returns_all() {
        let h = SkillTriggerHandler::with_skills(PathBuf::from("/tmp"), sample_skills());
        let items = h.matches(&ctx(""));
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn matches_no_skills_empty() {
        let h = SkillTriggerHandler::with_skills(PathBuf::from("/tmp"), vec![]);
        assert!(h.matches(&ctx("")).is_empty());
    }

    #[test]
    fn on_select_returns_insert() {
        let h = SkillTriggerHandler::with_skills(PathBuf::from("/tmp"), sample_skills());
        let items = h.matches(&ctx("rust"));
        let sel = h.on_select(&items[0], &ctx("rust"));
        assert_eq!(sel, "$rust-review ");
    }

    #[test]
    fn allows_free_text_true() {
        let h = SkillTriggerHandler::with_skills(PathBuf::from("/tmp"), sample_skills());
        assert!(h.allows_free_text());
    }
}
