//! Skill and SkillFrontmatter types.

// ── Skill ─────────────────────────────────────────────────────────────────────

/// A loaded skill ready for injection into the agent context.
#[derive(Debug, Clone)]
pub struct Skill {
    /// Unique name of this skill (from frontmatter `name:` or file stem).
    pub name: String,
    /// Human-readable description (from frontmatter `description:`).
    pub description: String,
    /// Optional tags for filtering (from frontmatter `tags:`).
    pub tags: Vec<String>,
    /// Markdown body after the frontmatter block.
    pub body: String,
    /// Absolute path this skill was loaded from.
    pub source_path: String,
    /// Injection priority. Higher = earlier in the prompt. Default: 0.
    pub priority: i32,
}

impl Skill {
    /// Format this skill for injection into the planner system prompt.
    pub fn format_for_prompt(&self) -> String {
        format!(
            "## Skill: {} ({})\n\n{}\n",
            self.name, self.description, self.body
        )
    }
}

// ── SkillFrontmatter ──────────────────────────────────────────────────────────

/// YAML frontmatter extracted from the beginning of a skill `.md` file.
///
/// Only a small subset of keys is parsed; unknown keys are silently ignored.
#[derive(Debug, Clone, Default)]
pub struct SkillFrontmatter {
    /// `name:` key.
    pub name: Option<String>,
    /// `description:` key.
    pub description: Option<String>,
    /// `tags:` key (comma-separated on a single line, or multi-value list).
    pub tags: Vec<String>,
    /// `priority:` key (integer).
    pub priority: i32,
}

impl SkillFrontmatter {
    /// Parse YAML frontmatter from the start of a Markdown file.
    ///
    /// Returns `(frontmatter, body_rest_of_file)`.
    ///
    /// If the file does not start with `---`, the frontmatter is empty and
    /// the returned `body` is the full input.
    pub fn parse(content: &str) -> (Self, &str) {
        if !content.starts_with("---") {
            return (Self::default(), content);
        }
        // Content after the opening `---`.
        let after_first_sep = &content[3..];
        // Look for the closing `---`.
        if let Some(end_pos) = after_first_sep.find("\n---") {
            let yaml_str = &after_first_sep[..end_pos];
            let rest = &after_first_sep[end_pos + 4..]; // skip `\n---`
            let rest = rest.trim_start_matches('\n');
            let fm = Self::parse_yaml(yaml_str);
            (fm, rest)
        } else {
            (Self::default(), content)
        }
    }

    /// Minimal line-by-line YAML parser (no external dep).
    fn parse_yaml(yaml: &str) -> Self {
        let mut fm = Self::default();
        let mut in_tags = false;

        for line in yaml.lines() {
            // Handle `tags:` list items (`  - foo`).
            if in_tags {
                if let Some(stripped) = line
                    .strip_prefix("  - ")
                    .or_else(|| line.strip_prefix("- "))
                {
                    fm.tags.push(stripped.trim().to_string());
                    continue;
                } else if !line.trim().is_empty() && !line.starts_with(' ') {
                    // Exited the tags list.
                    in_tags = false;
                } else {
                    continue;
                }
            }

            if let Some((key, val)) = line.split_once(':') {
                let key = key.trim();
                let val = val.trim().trim_matches('"').trim_matches('\'');

                match key {
                    "name" => fm.name = Some(val.to_string()),
                    "description" => fm.description = Some(val.to_string()),
                    "priority" => fm.priority = val.parse().unwrap_or(0),
                    "tags" => {
                        if val.is_empty() {
                            // Multi-line list follows.
                            in_tags = true;
                        } else {
                            // Inline comma-separated: `tags: a, b, c`
                            for tag in val.split(',') {
                                let t = tag.trim();
                                if !t.is_empty() {
                                    fm.tags.push(t.to_string());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        fm
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_no_frontmatter() {
        let content = "# Hello\n\nSome content.\n";
        let (fm, body) = SkillFrontmatter::parse(content);
        assert!(fm.name.is_none());
        assert_eq!(body, content);
    }

    #[test]
    fn parse_basic_frontmatter() {
        let content = "---\nname: my-skill\ndescription: Does things\npriority: 5\n---\n# Body\n";
        let (fm, body) = SkillFrontmatter::parse(content);
        assert_eq!(fm.name.as_deref(), Some("my-skill"));
        assert_eq!(fm.description.as_deref(), Some("Does things"));
        assert_eq!(fm.priority, 5);
        assert!(body.starts_with("# Body"));
    }

    #[test]
    fn parse_inline_tags() {
        let content = "---\nname: foo\ntags: a, b, c\n---\nbody\n";
        let (fm, _) = SkillFrontmatter::parse(content);
        assert_eq!(fm.tags, vec!["a", "b", "c"]);
    }

    #[test]
    fn format_for_prompt_includes_name_and_body() {
        let skill = Skill {
            name: "test".to_string(),
            description: "desc".to_string(),
            tags: vec![],
            body: "Do the thing.".to_string(),
            source_path: "/tmp/test.md".to_string(),
            priority: 0,
        };
        let out = skill.format_for_prompt();
        assert!(out.contains("## Skill: test"));
        assert!(out.contains("Do the thing."));
    }
}
