//! `/skills` handler — list the skills already loaded for this session.
//!
//! Mirrors agenthicc's `/skills` **listing** branch (`_cmd_skills` with no
//! argument): a table of the skills the session can inject, showing the
//! command that inserts each one and where it was loaded from.
//!
//! Two of agenthicc's three branches are deliberately absent, and the parity
//! row this handler closes says so:
//!
//! * the per-agent permission filter (`SkillPermissionSet`,
//!   `SkillDef.is_allowed_for`) — xaft *does* declare
//!   `AgentPreset::allowed_skills` / `denied_skills` and an `allows_skill`
//!   predicate, but **nothing on the injection path consults them**:
//!   `xaft-runtime/src/runtime.rs:337` injects every skill `load_all()`
//!   returns, and `allows_skill` has no production caller. A listing that
//!   filtered here would print a subset the run does not honour, so it lists
//!   what the run will actually inject;
//! * `/skills reload` — xaft loads skills once at startup and
//!   `SkillTriggerHandler`'s working dir is annotated "reserved for future
//!   skill reload", so there is no reload hook to call.
//!
//! The second is refused at the parser rather than accepted and ignored: a
//! `/skills reload` that answered with an unchanged "Available Skills" table
//! would read as a reload that succeeded and found nothing, which is the
//! shape of a placeholder dressed up as a feature.

use std::sync::{Arc, RwLock};

use xaft_skills::Skill;

use crate::slash::registry::SlashHandler;
use crate::slash::table::CliTable;
use crate::slash::{CommandContext, CommandResult};

/// Lists the skills this session loaded.
///
/// The list is *shared* with `AppState::init_skills`, not re-loaded here.
/// `SkillLoader::load_all` is async and this handler is not, and a second
/// loader would be free to disagree with the `$` picker about what exists —
/// listing skills the picker never got, or missing the ones it did.
pub struct SkillsHandler {
    skills: Arc<RwLock<Vec<Skill>>>,
}

impl SkillsHandler {
    /// Build a handler over the session's loaded-skill list.
    pub fn new(skills: Arc<RwLock<Vec<Skill>>>) -> Self {
        Self { skills }
    }
}

impl SlashHandler for SkillsHandler {
    fn description(&self) -> &'static str {
        "List the skills available to this session"
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        let skills = match self.skills.read() {
            Ok(s) => s,
            Err(_) => return CommandResult::Error("Failed to read the loaded skills.".into()),
        };

        let mut table = CliTable::new(["SKILL", "DESCRIPTION", "SOURCE"]);

        // Sorted by name so the output is stable across runs: the loader walks
        // a directory, and directory order is not a contract.
        let mut ordered: Vec<&Skill> = skills.iter().collect();
        ordered.sort_by(|a, b| a.name.cmp(&b.name));

        if ordered.is_empty() {
            // agenthicc's own empty-table row. Visible rather than a blank
            // table, so "no skills" cannot be mistaken for a broken command.
            table.push_row(["—", "(no skills found)", ""]);
        } else {
            for s in ordered {
                table.push_row([
                    format!("${}", s.name),
                    truncate_description(&s.description, 80),
                    s.source_path.clone(),
                ]);
            }
        }

        CommandResult::StyledLines(table.render(ctx.terminal_cols))
    }
}

/// agenthicc prints `(getattr(skill, "description", "") or "")[:80] or "—"`.
/// Same 80-column budget and same empty fallback, so the two listings fit the
/// same width.
fn truncate_description(description: &str, max: usize) -> String {
    if description.is_empty() {
        return "—".to_string();
    }
    if description.chars().count() <= max {
        return description.to_string();
    }
    description.chars().take(max).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::{AgentStatsMap, SlashCommand};
    use agtrs_runtime::signals::SignalBus;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use xaft_config::XaftConfig;

    fn skill(name: &str, description: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            tags: vec![],
            body: String::new(),
            source_path: format!("/proj/.xaft/skills/{name}.md"),
            priority: 0,
        }
    }

    fn context() -> CommandContext {
        CommandContext {
            args: String::new(),
            command: SlashCommand::Skills,
            signals: Arc::new(SignalBus::new()),
            config: Arc::new(XaftConfig::default()),
            session_id: None,
            working_dir: PathBuf::from("/proj"),
            terminal_cols: 200,
            llm_stats: Arc::new(RwLock::new(AgentStatsMap::new())),
            conversation_store: None,
            session_store: None,
            intent_active: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            input_history: None,
            services: Default::default(),
        }
    }

    /// The handler reads a shared list, so every test hands it the same `Arc`
    /// `AppState` would.
    fn handler_and_ctx(skills: Vec<Skill>) -> (SkillsHandler, CommandContext) {
        let shared = Arc::new(RwLock::new(skills));
        (SkillsHandler::new(shared), context())
    }

    fn rendered(result: CommandResult) -> String {
        match result {
            CommandResult::StyledLines(lines) => lines
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            other => panic!("expected StyledLines, got {other:?}"),
        }
    }

    #[test]
    fn lists_every_loaded_skill_with_its_source() {
        let (handler, ctx) = handler_and_ctx(vec![
            skill("rust-review", "Rust code review checklist"),
            skill("commit-msg", "Conventional commit message format"),
        ]);
        let text = rendered(handler.execute(ctx));
        assert!(text.contains("$rust-review"), "{text}");
        assert!(text.contains("$commit-msg"), "{text}");
        assert!(text.contains("Rust code review checklist"), "{text}");
        assert!(
            text.contains("/proj/.xaft/skills/rust-review.md"),
            "the source path is what makes \"which file did this come from\" \
             answerable: {text}"
        );
    }

    #[test]
    fn sorts_by_name_so_the_listing_is_stable() {
        let (handler, ctx) = handler_and_ctx(vec![
            skill("zebra", "z"),
            skill("alpha", "a"),
            skill("middle", "m"),
        ]);
        let text = rendered(handler.execute(ctx));
        let alpha = text.find("$alpha").expect("alpha row");
        let middle = text.find("$middle").expect("middle row");
        let zebra = text.find("$zebra").expect("zebra row");
        assert!(alpha < middle && middle < zebra, "{text}");
    }

    #[test]
    fn an_empty_skill_list_says_so_instead_of_rendering_a_blank_table() {
        let (handler, ctx) = handler_and_ctx(Vec::new());
        let text = rendered(handler.execute(ctx));
        assert!(
            text.contains("(no skills found)"),
            "an empty listing must name its own emptiness: {text}"
        );
    }

    #[test]
    fn a_long_description_is_cut_to_the_same_budget_as_agenthicc() {
        let long = "x".repeat(200);
        let (handler, ctx) = handler_and_ctx(vec![skill("wordy", &long)]);
        let text = rendered(handler.execute(ctx));
        assert!(
            !text.contains(&long),
            "the full 200-char description leaked"
        );
        assert!(text.contains(&"x".repeat(80)), "{text}");
        assert!(!text.contains(&"x".repeat(81)), "{text}");
    }

    #[test]
    fn an_empty_description_renders_the_same_placeholder_agenthicc_uses() {
        let (handler, ctx) = handler_and_ctx(vec![skill("bare", "")]);
        let text = rendered(handler.execute(ctx));
        assert!(text.contains("$bare"), "{text}");
        assert!(text.contains('—'), "{text}");
    }

    #[test]
    fn reads_the_list_after_skills_are_loaded() {
        // `AppState` builds the registry (and so this handler) before skills
        // are loaded, then writes them in. A handler that snapshotted at
        // construction time would still list nothing here.
        let shared = Arc::new(RwLock::new(Vec::new()));
        let handler = SkillsHandler::new(Arc::clone(&shared));

        assert!(rendered(handler.execute(context())).contains("(no skills found)"));

        *shared.write().expect("uncontended lock") = vec![skill("late", "loaded after wiring")];
        let text = rendered(handler.execute(context()));
        assert!(text.contains("$late"), "{text}");
        assert!(text.contains("loaded after wiring"), "{text}");
    }
}
