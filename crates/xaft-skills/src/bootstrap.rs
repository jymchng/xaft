//! Default skills installed on first launch — agenthicc's
//! `skills/bootstrap.py` (PRD-104).
//!
//! Nine curated skills (review, refactor, architect, docs, debug, commit,
//! create-tools, create-commands, create-workflow) are written as directory
//! skills (`<slug>/SKILL.md`) into the global skills directory the first time
//! xaft starts. Existing directories are never overwritten, and a skill the
//! user deleted after it was installed is remembered in
//! `default_skills.json` and never recreated.
//!
//! The three `create-*` skills describe xaft's own extension points (MCP
//! servers, `.xaft/cli.toml` commands, phased workflows) rather than
//! agenthicc's Python plugin APIs; the other six are agenthicc's text.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The marker file recording each default skill's lifecycle.
pub const MARKER_FILE: &str = "default_skills.json";

/// `(slug, SKILL.md)` for every default skill.
pub const DEFAULT_SKILLS: &[(&str, &str)] = &[
    ("review", include_str!("defaults/review.md")),
    ("refactor", include_str!("defaults/refactor.md")),
    ("architect", include_str!("defaults/architect.md")),
    ("docs", include_str!("defaults/docs.md")),
    ("debug", include_str!("defaults/debug.md")),
    ("commit", include_str!("defaults/commit.md")),
    ("create-tools", include_str!("defaults/create-tools.md")),
    (
        "create-commands",
        include_str!("defaults/create-commands.md"),
    ),
    (
        "create-workflow",
        include_str!("defaults/create-workflow.md"),
    ),
];

/// The global root default skills belong to: `[skills]
/// default_skill_directory` when set, else `~/.config/xaft`. Skills live in
/// `<root>/skills`, the marker file in `<root>`.
pub fn default_skill_root(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if configured.is_empty() {
        return dirs::home_dir().map(|h| h.join(".config").join("xaft"));
    }
    Some(match configured.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()?.join(rest),
        None => PathBuf::from(configured),
    })
}

fn load_markers(root: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(root.join(MARKER_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_markers(root: &Path, markers: &BTreeMap<String, String>) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(markers).map_err(std::io::Error::other)?;
    std::fs::write(root.join(MARKER_FILE), text)
}

/// Install every missing default skill under `<root>/skills`.
///
/// Returns how many were newly installed. A skill marked `deleted` is
/// skipped; one marked `installed` whose directory is gone is re-marked
/// `deleted` (the user removed it) and skipped; an existing directory is never
/// touched. Does nothing when `enabled` is false.
pub fn bootstrap_default_skills(root: &Path, enabled: bool) -> std::io::Result<usize> {
    if !enabled {
        return Ok(0);
    }
    let skills_dir = root.join("skills");
    std::fs::create_dir_all(&skills_dir)?;
    let mut markers = load_markers(root);
    let mut dirty = false;
    let mut installed = 0;

    for (slug, content) in DEFAULT_SKILLS {
        let slug = (*slug).to_string();
        if markers.get(&slug).map(String::as_str) == Some("deleted") {
            continue;
        }
        let dir = skills_dir.join(&slug);
        if markers.get(&slug).map(String::as_str) == Some("installed") && !dir.exists() {
            markers.insert(slug, "deleted".into());
            dirty = true;
            continue;
        }
        if dir.exists() {
            if markers.get(&slug).map(String::as_str) != Some("installed") {
                markers.insert(slug, "installed".into());
                dirty = true;
            }
            continue;
        }
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("SKILL.md"), content)?;
        markers.insert(slug, "installed".into());
        dirty = true;
        installed += 1;
    }
    if dirty {
        save_markers(root, &markers)?;
    }
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::SkillLoader;

    #[test]
    fn first_launch_installs_all_nine_and_a_second_is_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(bootstrap_default_skills(tmp.path(), true).unwrap(), 9);
        assert!(tmp.path().join("skills/create-workflow/SKILL.md").is_file());
        assert_eq!(bootstrap_default_skills(tmp.path(), true).unwrap(), 0);
    }

    #[test]
    fn a_deleted_default_is_remembered_and_never_recreated() {
        let tmp = tempfile::tempdir().unwrap();
        bootstrap_default_skills(tmp.path(), true).unwrap();
        std::fs::remove_dir_all(tmp.path().join("skills/commit")).unwrap();
        assert_eq!(bootstrap_default_skills(tmp.path(), true).unwrap(), 0);
        assert!(!tmp.path().join("skills/commit").exists());
        let markers = load_markers(tmp.path());
        assert_eq!(markers["commit"], "deleted");
        assert_eq!(markers["review"], "installed");
    }

    #[test]
    fn an_existing_user_skill_is_never_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let mine = tmp.path().join("skills/review");
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::write(mine.join("SKILL.md"), "my own review skill").unwrap();
        assert_eq!(bootstrap_default_skills(tmp.path(), true).unwrap(), 8);
        assert_eq!(
            std::fs::read_to_string(mine.join("SKILL.md")).unwrap(),
            "my own review skill"
        );
    }

    #[test]
    fn disabled_bootstrap_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(bootstrap_default_skills(tmp.path(), false).unwrap(), 0);
        assert!(!tmp.path().join("skills").exists());
    }

    #[test]
    fn a_configured_root_is_used_verbatim_and_blank_means_home() {
        assert_eq!(
            default_skill_root("/opt/xaft-skills"),
            Some(PathBuf::from("/opt/xaft-skills"))
        );
        if let Some(home) = dirs::home_dir() {
            assert_eq!(default_skill_root(""), Some(home.join(".config/xaft")));
            assert_eq!(default_skill_root("~/team"), Some(home.join("team")));
        }
    }

    #[tokio::test]
    async fn every_default_skill_loads_with_a_name_and_description() {
        let tmp = tempfile::tempdir().unwrap();
        bootstrap_default_skills(tmp.path(), true).unwrap();
        let skills = SkillLoader::new(vec![tmp.path().join("skills")])
            .load_all()
            .await;
        assert_eq!(skills.len(), 9);
        for skill in &skills {
            assert!(!skill.name.trim().is_empty());
            assert!(
                !skill.description.trim().is_empty(),
                "{} has no description",
                skill.name
            );
        }
    }
}
