//! Skill loader — discovers, reads, and assembles skills from the filesystem.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::skill::{Skill, SkillFrontmatter};

/// Maximum bytes for a single skill file.
const MAX_SKILL_BYTES: usize = 32_768; // 32 KiB

/// Maximum total bytes across all loaded skills.
const MAX_TOTAL_BYTES: usize = 131_072; // 128 KiB

// ── SkillLoader ───────────────────────────────────────────────────────────────

/// Discovers and loads skills from one or more search directories.
///
/// # Directory layout
///
/// ```text
/// .xaft/skills/         ← project-local skills (loaded first)
///   my-skill.md
///   another.md
///
/// ~/.config/xaft/skills/  ← global skills (loaded second; deduped by name)
///   shared.md
/// ```
///
/// Each `.md` file may optionally begin with YAML frontmatter:
///
/// ```markdown
/// ---
/// name: my-skill
/// description: Explains something
/// priority: 10
/// tags: rust, async
/// ---
/// # The actual skill content…
/// ```
pub struct SkillLoader {
    search_dirs: Vec<PathBuf>,
    max_skill_bytes: usize,
    max_total_bytes: usize,
}

impl SkillLoader {
    /// Create a loader with explicit search directories.
    pub fn new(search_dirs: Vec<PathBuf>) -> Self {
        Self {
            search_dirs,
            max_skill_bytes: MAX_SKILL_BYTES,
            max_total_bytes: MAX_TOTAL_BYTES,
        }
    }

    /// Create from a working directory.
    ///
    /// Adds `.xaft/skills/` (project-local) and
    /// `~/.config/xaft/skills/` (global) to the search path.
    pub fn for_working_dir(working_dir: &Path) -> Self {
        let mut dirs = vec![working_dir.join(".xaft").join("skills")];
        if let Some(home) = dirs::home_dir() {
            dirs.push(home.join(".config").join("xaft").join("skills"));
        }
        Self::new(dirs)
    }

    /// Load all skills from the configured search directories.
    ///
    /// - Local skills (earlier in `search_dirs`) win over global skills with
    ///   the same name.
    /// - Results are sorted by `priority` descending, then alphabetically.
    /// - Loading stops (with a warning) when `max_total_bytes` is reached.
    pub async fn load_all(&self) -> Vec<Skill> {
        let mut skills = Vec::new();
        let mut total_bytes = 0usize;
        let mut seen_names: HashSet<String> = HashSet::new();

        for dir in &self.search_dirs {
            if !dir.exists() {
                continue;
            }
            let mut entries = match tokio::fs::read_dir(dir).await {
                Ok(rd) => rd,
                Err(_) => continue,
            };

            // Collect all entries first, then sort for deterministic order.
            let mut file_paths: Vec<PathBuf> = Vec::new();
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    file_paths.push(path);
                }
            }
            file_paths.sort();

            for path in file_paths {
                if total_bytes >= self.max_total_bytes {
                    tracing::warn!(
                        "xaft-skills: total byte limit ({}) reached, skipping remaining skills",
                        self.max_total_bytes
                    );
                    break;
                }
                if let Some(skill) = self.load_skill_file(&path).await {
                    if seen_names.insert(skill.name.clone()) {
                        total_bytes += skill.body.len();
                        skills.push(skill);
                    }
                }
            }
        }

        // Also look for a SKILL.md bundle in the working directory root.
        if let Some(first_dir) = self.search_dirs.first() {
            if let Some(xaft_dir) = first_dir.parent() {
                if let Some(working_dir) = xaft_dir.parent() {
                    let skill_md = working_dir.join("SKILL.md");
                    if skill_md.exists() {
                        let bundle_skills = self.load_skill_bundle(&skill_md).await;
                        for skill in bundle_skills {
                            if seen_names.insert(skill.name.clone()) {
                                skills.push(skill);
                            }
                        }
                    }
                }
            }
        }

        // Sort: higher priority first, then alphabetically by name.
        skills.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.name.cmp(&b.name))
        });

        skills
    }

    /// Assemble all loaded skills into a single system prompt section.
    ///
    /// Returns an empty string when `skills` is empty.
    pub fn build_prompt_section(skills: &[Skill]) -> String {
        if skills.is_empty() {
            return String::new();
        }
        let mut out = String::from("\n\n# Loaded Skills\n\n");
        for skill in skills {
            out.push_str(&skill.format_for_prompt());
            out.push('\n');
        }
        out
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    async fn load_skill_file(&self, path: &Path) -> Option<Skill> {
        let content = tokio::fs::read_to_string(path).await.ok()?;
        if content.trim().is_empty() {
            return None;
        }
        // Cap at per-file limit (soft — we keep whatever fits).
        let capped: &str = if content.len() > self.max_skill_bytes {
            // Walk back to a UTF-8 boundary.
            let mut boundary = self.max_skill_bytes;
            while boundary > 0 && !content.is_char_boundary(boundary) {
                boundary -= 1;
            }
            &content[..boundary]
        } else {
            &content
        };

        let (fm, body) = SkillFrontmatter::parse(capped);
        if body.trim().is_empty() {
            return None;
        }

        let name = fm.name.clone().unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string()
        });

        Some(Skill {
            name,
            description: fm.description.unwrap_or_default(),
            tags: fm.tags,
            body: body.to_string(),
            source_path: path.display().to_string(),
            priority: fm.priority,
        })
    }

    /// Parse a `SKILL.md` bundle: each `## Skill: <name>` heading starts a
    /// new skill.  Content before the first heading is ignored.
    async fn load_skill_bundle(&self, path: &Path) -> Vec<Skill> {
        let content = match tokio::fs::read_to_string(path).await {
            Ok(c) => c,
            Err(_) => return vec![],
        };

        let mut skills = Vec::new();
        let mut current_name: Option<String> = None;
        let mut current_body = String::new();

        for line in content.lines() {
            if let Some(name) = line.strip_prefix("## Skill: ") {
                // Flush previous skill.
                if let Some(prev_name) = current_name.take() {
                    if !current_body.trim().is_empty() {
                        skills.push(Skill {
                            name: prev_name,
                            description: String::new(),
                            tags: vec![],
                            body: current_body.trim().to_string(),
                            source_path: path.display().to_string(),
                            priority: 0,
                        });
                    }
                }
                current_name = Some(name.trim().to_string());
                current_body = String::new();
            } else if current_name.is_some() {
                current_body.push_str(line);
                current_body.push('\n');
            }
        }

        // Flush last skill.
        if let Some(name) = current_name {
            if !current_body.trim().is_empty() {
                skills.push(Skill {
                    name,
                    description: String::new(),
                    tags: vec![],
                    body: current_body.trim().to_string(),
                    source_path: path.display().to_string(),
                    priority: 0,
                });
            }
        }

        skills
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn write_file(dir: &Path, name: &str, content: &str) {
        tokio::fs::write(dir.join(name), content).await.unwrap();
    }

    #[tokio::test]
    async fn build_prompt_section_empty_for_no_skills() {
        assert_eq!(SkillLoader::build_prompt_section(&[]), "");
    }

    #[tokio::test]
    async fn build_prompt_section_includes_all_skills() {
        let skills = vec![
            Skill {
                name: "a".into(),
                description: "desc-a".into(),
                tags: vec![],
                body: "body-a".into(),
                source_path: "/tmp/a.md".into(),
                priority: 0,
            },
            Skill {
                name: "b".into(),
                description: "desc-b".into(),
                tags: vec![],
                body: "body-b".into(),
                source_path: "/tmp/b.md".into(),
                priority: 0,
            },
        ];
        let section = SkillLoader::build_prompt_section(&skills);
        assert!(section.contains("Skill: a"));
        assert!(section.contains("Skill: b"));
        assert!(section.contains("body-a"));
        assert!(section.contains("body-b"));
    }

    #[tokio::test]
    async fn loader_loads_skill_from_dir() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join(".xaft").join("skills");
        tokio::fs::create_dir_all(&skills_dir).await.unwrap();
        write_file(
            &skills_dir,
            "test.md",
            "---\nname: test\ndescription: A test skill\n---\nDo the thing.\n",
        )
        .await;

        let loader = SkillLoader::new(vec![skills_dir]);
        let skills = loader.load_all().await;
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "test");
        assert!(skills[0].body.contains("Do the thing."));
    }

    #[tokio::test]
    async fn loader_parses_frontmatter_name_and_description() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        tokio::fs::create_dir_all(&skills_dir).await.unwrap();
        write_file(
            &skills_dir,
            "skill.md",
            "---\nname: my-skill\ndescription: Does X\n---\nContent here.\n",
        )
        .await;

        let loader = SkillLoader::new(vec![skills_dir]);
        let skills = loader.load_all().await;
        assert_eq!(skills[0].name, "my-skill");
        assert_eq!(skills[0].description, "Does X");
    }

    #[tokio::test]
    async fn loader_loads_skill_bundle_from_skill_md() {
        let tmp = TempDir::new().unwrap();
        // Put SKILL.md in working dir root.
        let working_dir = tmp.path();
        let skills_dir = working_dir.join(".xaft").join("skills");
        tokio::fs::create_dir_all(&skills_dir).await.unwrap();

        let bundle = "## Skill: alpha\nDo alpha things.\n## Skill: beta\nDo beta things.\n";
        write_file(working_dir, "SKILL.md", bundle).await;

        let loader = SkillLoader::new(vec![skills_dir]);
        let skills = loader.load_all().await;
        assert!(skills.iter().any(|s| s.name == "alpha"));
        assert!(skills.iter().any(|s| s.name == "beta"));
    }

    #[tokio::test]
    async fn loader_respects_size_limit() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        tokio::fs::create_dir_all(&skills_dir).await.unwrap();

        // Write a skill file larger than MAX_SKILL_BYTES.
        let big = "X".repeat(MAX_SKILL_BYTES + 100);
        write_file(&skills_dir, "big.md", &big).await;

        let loader = SkillLoader::new(vec![skills_dir]);
        let skills = loader.load_all().await;
        // The skill is loaded but capped.
        assert_eq!(skills.len(), 1);
        assert!(skills[0].body.len() <= MAX_SKILL_BYTES);
    }

    #[tokio::test]
    async fn loader_deduplicates_by_name_local_wins() {
        let tmp = TempDir::new().unwrap();
        let local_dir = tmp.path().join("local").join("skills");
        let global_dir = tmp.path().join("global").join("skills");
        tokio::fs::create_dir_all(&local_dir).await.unwrap();
        tokio::fs::create_dir_all(&global_dir).await.unwrap();

        write_file(
            &local_dir,
            "shared.md",
            "---\nname: shared\n---\nlocal body",
        )
        .await;
        write_file(
            &global_dir,
            "shared.md",
            "---\nname: shared\n---\nglobal body",
        )
        .await;

        // local_dir first → local wins.
        let loader = SkillLoader::new(vec![local_dir, global_dir]);
        let skills = loader.load_all().await;
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].body.trim(), "local body");
    }

    #[tokio::test]
    async fn loader_sorts_by_priority() {
        let tmp = TempDir::new().unwrap();
        let skills_dir = tmp.path().join("skills");
        tokio::fs::create_dir_all(&skills_dir).await.unwrap();

        write_file(
            &skills_dir,
            "low.md",
            "---\nname: low\npriority: 1\n---\nlow",
        )
        .await;
        write_file(
            &skills_dir,
            "high.md",
            "---\nname: high\npriority: 10\n---\nhigh",
        )
        .await;
        write_file(
            &skills_dir,
            "mid.md",
            "---\nname: mid\npriority: 5\n---\nmid",
        )
        .await;

        let loader = SkillLoader::new(vec![skills_dir]);
        let skills = loader.load_all().await;
        assert_eq!(skills[0].name, "high");
        assert_eq!(skills[1].name, "mid");
        assert_eq!(skills[2].name, "low");
    }
}
