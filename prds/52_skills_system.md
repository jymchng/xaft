# PRD 52 — xaft Skills System

## Status: Draft
## Depends on: PRD 51 (AGENTS.md auto-loading), PRD 41 (`/init`), PRD 32a–32e (slash commands), PRD 20 (agent handoff)

---

## 1. Problem

xaft agents possess broad general coding knowledge but no project-specific
or domain-specific expertise. Engineers working on Django projects, React
apps, or Rust embedded systems must restate the same conventions,
anti-patterns, and tool-usage rules every time they interact with xaft,
either as part of the task description or through an AGENTS.md file.

AGENTS.md (PRD 51) solves per-project memory. But teams often want reusable
packages of domain knowledge that can be shared across projects — for example,
a "django-rest-framework" skill that teaches xaft the correct serializer
patterns, a "pytest-fixtures" skill that explains fixture scoping rules, or
a "security-review" skill that adds OWASP checking behaviour to the QA agent.

Currently there is no mechanism to define, discover, share, or activate such
reusable agent expertise packages.

---

## 2. Goals

1. **Skill discovery** — at session start, scan `.xaft/skills/` in the
   working directory and `~/.config/xaft/skills/` globally for `*.md` files.
2. **Skill injection** — loaded skill content is appended to the planner's
   system prompt, after the base prompt but before the user task.
3. **Structured format** — each skill file carries YAML frontmatter
   declaring name, version, description, tags, required tools, and an
   `apply_when` hint.
4. **Dynamic slash commands** — any `## /command-name` heading in a skill
   file registers a new slash command. When the user types `/command-name`,
   the section body is sent as a user message to the agent.
5. **`/skills` slash command** — lists all loaded skills with names,
   descriptions, and source paths. `/skills reload` re-scans on demand.
6. **CLI selection** — `xaft run --skills django,pytest "fix the tests"`
   activates only the named skills. `--no-skills` disables loading entirely.
7. **SKILL.md shorthand** — a single `SKILL.md` at the project root is
   treated as a combined skill bundle; each `## Skill: <name>` heading
   starts a new skill entry.
8. **Config** — `[skills]` section in `xaft.toml` controls enabled state,
   search paths, and total byte budget.
9. **`XaftSkillsLoaded` signal** — emitted after discovery, visible in TUI
   as `[skills] Loaded N skills: name1, name2`.

---

## 3. Non-Goals

- LLM-generated skill content (skills are human-authored Markdown only).
- Network fetch of skills from registries or URLs.
- Access-controlled or signed skills (no signature verification in v1).
- Skills modifying agent tools at runtime (read-only system prompt injection;
  the `tools_required` field is advisory only in v1).
- Hot-reloading skills while the orchestrator is running (reload requires
  `/skills reload`).
- Skills scoped to a single agent in the pipeline (all loaded skills are
  visible to all agents via the planner system prompt injection).

---

## 4. Architecture

### 4.1 Overview

```text
XaftRuntime::run_task()
    ├── AgentsMdLoader::load_for_session()    [PRD 51]
    ├── [NEW] SkillLoader::load_from_dirs(search_dirs)
    │         ├── scan .xaft/skills/*.md
    │         ├── scan ~/.config/xaft/skills/*.md
    │         ├── parse frontmatter + body per file
    │         ├── apply --skills filter (from RunRequest::skill_filter)
    │         ├── apply byte budget
    │         └── return Vec<Skill>
    ├── [NEW] SkillInjector::build_extra_system_context(&skills)
    │         └── returns String (concatenated skill bodies with headers)
    ├── run_workflow(task, …)
    │         ├── NamedAgent::new(PLANNER_NAME, combined_prompt)
    │         │     combined_prompt = planner_system_prompt(wd)
    │         │                     + "\n\n" + extra_system_context
    │         └── … normal pipeline …
    └── RunResult
```

Skills are loaded synchronously before `run_workflow()` and injected
directly into the planner's system prompt string. No new `ToolHook` is
needed — the injection is a static string concatenation.

### 4.2 New crate: `crates/xaft-skills/`

```
crates/xaft-skills/
    src/
        lib.rs          — pub use, feature flags
        loader.rs       — SkillLoader, scan logic
        skill.rs        — Skill, SkillFrontmatter types
        injector.rs     — SkillInjector (prompt assembly)
        dynamic_cmd.rs  — DynamicSkillCommand (slash command stubs)
        error.rs        — SkillError (thiserror)
    tests/
        skill_loading.rs
        skill_injection.rs
        dynamic_commands.rs
```

Add to `Cargo.toml` workspace members:

```toml
[workspace]
members = [
    # … existing …
    "crates/xaft-skills",
]
```

Add to `crates/xaft-runtime/Cargo.toml`:

```toml
[dependencies]
xaft-skills = { path = "../xaft-skills" }
```

### 4.3 Type definitions

#### `crates/xaft-skills/src/skill.rs`

```rust
//! Core `Skill` and `SkillFrontmatter` types.

use serde::{Deserialize, Serialize};

/// A parsed skill loaded from a `*.md` file or a `## Skill: <name>`
/// section inside `SKILL.md`.
#[derive(Debug, Clone)]
pub struct Skill {
    /// Parsed YAML frontmatter. May be absent for ad-hoc skill sections.
    pub frontmatter: Option<SkillFrontmatter>,
    /// The Markdown body of the skill (everything after the frontmatter fence).
    pub body: String,
    /// Source path relative to the search root (e.g. `.xaft/skills/django.md`).
    pub source_path: String,
    /// Origin of the skill: `Local` (.xaft/skills), `Global` (~/.config/xaft/skills),
    /// or `Bundle` (from SKILL.md).
    pub origin: SkillOrigin,
}

impl Skill {
    /// Return the display name: frontmatter `name` if present, otherwise the
    /// file stem (e.g. `django` for `django.md`).
    pub fn display_name(&self) -> &str {
        self.frontmatter
            .as_ref()
            .map(|f| f.name.as_str())
            .unwrap_or_else(|| {
                self.source_path
                    .rsplit('/')
                    .next()
                    .and_then(|n| n.strip_suffix(".md"))
                    .unwrap_or(&self.source_path)
            })
    }

    /// Return the description from frontmatter, or an empty string.
    pub fn description(&self) -> &str {
        self.frontmatter
            .as_ref()
            .and_then(|f| f.description.as_deref())
            .unwrap_or("")
    }

    /// Scan `body` for `## /command-name` headings and return all found
    /// dynamic slash command definitions.
    pub fn dynamic_commands(&self) -> Vec<DynamicSkillCommand> {
        extract_dynamic_commands(&self.body, self.display_name())
    }

    /// Byte size of the body.
    pub fn body_bytes(&self) -> usize {
        self.body.len()
    }
}

/// YAML frontmatter block parsed from the `---` fences at the top of a
/// skill file.
///
/// All fields are optional to allow minimal skill files. When `name` is
/// absent, the file stem is used as the skill name.
///
/// Full schema:
/// ```yaml
/// ---
/// name: "django-conventions"
/// version: "1.0"
/// description: "Django project conventions and patterns"
/// tags: ["django", "python", "web"]
/// tools_required: ["read_file", "grep"]
/// apply_when: "*.py files present in workspace"
/// priority: 50
/// ---
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillFrontmatter {
    /// Short machine-readable identifier. Used for `--skills` CLI flag and
    /// `/skills reload`. Must be unique within a session.
    pub name: String,

    /// Semantic version string (e.g. `"1.0"`). Informational only in v1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Human-readable description shown by `/skills list`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Free-form tags for organisation (e.g. `["django", "python"]`).
    #[serde(default)]
    pub tags: Vec<String>,

    /// Tool names this skill's patterns require the agent to have.
    /// Advisory in v1 — no enforcement.
    #[serde(default)]
    pub tools_required: Vec<String>,

    /// Natural-language hint describing when this skill should be active.
    /// Displayed by `/skills list`; not evaluated automatically in v1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_when: Option<String>,

    /// Injection priority — higher values are injected later in the prompt
    /// (i.e., closer to the user task). Default 50. Range 0–100.
    #[serde(default = "default_priority")]
    pub priority: u8,
}

fn default_priority() -> u8 { 50 }

/// Where a skill was loaded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillOrigin {
    /// Loaded from `.xaft/skills/` in the working directory.
    Local,
    /// Loaded from `~/.config/xaft/skills/`.
    Global,
    /// Parsed from a `## Skill: <name>` section inside `SKILL.md`.
    Bundle,
}
```

#### `crates/xaft-skills/src/dynamic_cmd.rs`

```rust
//! Dynamic slash commands derived from `## /command-name` sections in skills.

/// A slash command stub extracted from a skill's `## /command-name` section.
///
/// When the user types `/command-name` in the TUI, `body` is sent as a
/// `Message::user(...)` to the agent pipeline, prefixed with a header that
/// identifies the skill source.
#[derive(Debug, Clone)]
pub struct DynamicSkillCommand {
    /// Command name without the leading slash (e.g. `"django-migrate"`).
    pub name: String,
    /// The Markdown content of the section body.
    pub body: String,
    /// Name of the skill that registered this command.
    pub skill_name: String,
}

impl DynamicSkillCommand {
    /// Build the message text sent to the agent when this command fires.
    pub fn as_agent_message(&self) -> String {
        format!(
            "[Skill: {}] /{}\n\n{}",
            self.skill_name, self.name, self.body
        )
    }
}

/// Parse `## /command-name` headings from a skill body.
///
/// Rules:
/// - A line matching `^## /[a-zA-Z][a-zA-Z0-9_-]*$` starts a command section.
/// - Everything between this heading and the next `##`-level heading is the body.
/// - The body is trimmed of leading/trailing whitespace.
pub fn extract_dynamic_commands(body: &str, skill_name: &str) -> Vec<DynamicSkillCommand> {
    let mut commands = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_body = String::new();

    for line in body.lines() {
        if let Some(cmd_name) = parse_command_heading(line) {
            // Flush previous command if any
            if let Some(name) = current_name.take() {
                commands.push(DynamicSkillCommand {
                    name,
                    body: current_body.trim().to_string(),
                    skill_name: skill_name.to_string(),
                });
            }
            current_body = String::new();
            current_name = Some(cmd_name);
        } else if current_name.is_some() {
            // Check if a new h2 section (non-command) terminates the body
            if line.starts_with("## ") && !line.starts_with("## /") {
                if let Some(name) = current_name.take() {
                    commands.push(DynamicSkillCommand {
                        name,
                        body: current_body.trim().to_string(),
                        skill_name: skill_name.to_string(),
                    });
                }
                current_body = String::new();
            } else {
                current_body.push_str(line);
                current_body.push('\n');
            }
        }
    }

    // Flush final command
    if let Some(name) = current_name.take() {
        commands.push(DynamicSkillCommand {
            name,
            body: current_body.trim().to_string(),
            skill_name: skill_name.to_string(),
        });
    }

    commands
}

/// Parse a line like `## /django-migrate` → `Some("django-migrate")`.
/// Returns `None` for any non-matching line.
fn parse_command_heading(line: &str) -> Option<String> {
    let stripped = line.trim().strip_prefix("## /")?;
    // Validate: only [a-zA-Z][a-zA-Z0-9_-]* allowed
    let re_valid = stripped.chars().enumerate().all(|(i, c)| {
        if i == 0 { c.is_ascii_alphabetic() }
        else { c.is_ascii_alphanumeric() || c == '-' || c == '_' }
    });
    if stripped.is_empty() || !re_valid {
        return None;
    }
    Some(stripped.to_string())
}
```

#### `crates/xaft-skills/src/loader.rs`

```rust
//! `SkillLoader` — discovers and parses skill files from search directories.

use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use crate::error::SkillError;
use crate::skill::{Skill, SkillFrontmatter, SkillOrigin};

/// Maximum byte size of a single skill body (enforced per-file).
pub const DEFAULT_MAX_SKILL_BYTES: usize = 32_768; // 32 KiB per skill

/// Maximum total bytes across all loaded skills.
pub const DEFAULT_MAX_TOTAL_BYTES: usize = 131_072; // 128 KiB total

/// Skill filename sentinel at project root.
pub const SKILL_MD_FILENAME: &str = "SKILL.md";

/// Discovers and loads skill files from one or more directories.
///
/// Loading order:
/// 1. Global skills (`~/.config/xaft/skills/*.md`) sorted alphabetically.
/// 2. Local skills (`.xaft/skills/*.md`) sorted alphabetically.
/// 3. Local overrides global when names collide (`frontmatter.name` match).
///
/// After deduplication and byte-budget enforcement, skills are sorted by
/// `frontmatter.priority` (ascending — lower priority appears first, giving
/// higher-priority skills the last-written-wins position in the prompt).
pub struct SkillLoader {
    /// Maximum bytes to read from a single skill file body.
    pub max_skill_bytes: usize,
    /// Maximum total bytes across all skills combined.
    pub max_total_bytes: usize,
    /// If set, only load skills whose `name` matches an entry in this list.
    pub name_filter: Option<Vec<String>>,
}

impl Default for SkillLoader {
    fn default() -> Self {
        Self {
            max_skill_bytes: DEFAULT_MAX_SKILL_BYTES,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
            name_filter: None,
        }
    }
}

impl SkillLoader {
    /// Create a loader with custom byte budgets.
    pub fn new(max_skill_bytes: usize, max_total_bytes: usize) -> Self {
        Self {
            max_skill_bytes,
            max_total_bytes,
            name_filter: None,
        }
    }

    /// Restrict loading to skills named in `names` (case-sensitive match
    /// against `SkillFrontmatter::name` or file stem).
    pub fn with_name_filter(mut self, names: Vec<String>) -> Self {
        self.name_filter = Some(names);
        self
    }

    /// Load all skills from `dirs` in order.
    ///
    /// Returns `Vec<Skill>` sorted by priority, deduplicated by name (local
    /// wins over global on name collision).
    pub async fn load_from_dirs(&self, dirs: &[SkillSearchDir]) -> Vec<Skill> {
        let mut all: Vec<Skill> = Vec::new();

        for dir_spec in dirs {
            let skills = self.load_dir(&dir_spec.path, dir_spec.origin.clone()).await;
            all.extend(skills);
        }

        // Deduplicate: later entries (local) override earlier (global) on name collision
        let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut deduped: Vec<Skill> = Vec::new();
        for skill in all.into_iter().rev() {
            let name = skill.display_name().to_string();
            if seen_names.insert(name) {
                deduped.push(skill);
            }
        }
        deduped.reverse();

        // Apply name filter
        let filtered: Vec<Skill> = if let Some(ref filter) = self.name_filter {
            deduped
                .into_iter()
                .filter(|s| filter.iter().any(|f| f == s.display_name()))
                .collect()
        } else {
            deduped
        };

        // Sort by priority (ascending: lower priority first in prompt)
        let mut sorted = filtered;
        sorted.sort_by_key(|s| {
            s.frontmatter
                .as_ref()
                .map(|f| f.priority)
                .unwrap_or(50)
        });

        // Enforce total byte budget (drop trailing skills if over budget)
        let mut total = 0usize;
        sorted
            .into_iter()
            .take_while(|s| {
                total += s.body_bytes();
                total <= self.max_total_bytes
            })
            .collect()
    }

    /// Load all `*.md` files from a single directory.
    async fn load_dir(&self, dir: &Path, origin: SkillOrigin) -> Vec<Skill> {
        let mut skills = Vec::new();

        // Check for SKILL.md bundle at the working directory root
        // (only for Local origin — bundle lives at project root, not in skills/)
        if origin == SkillOrigin::Local {
            let bundle_path = dir
                .parent()
                .unwrap_or(dir)
                .parent()
                .unwrap_or(dir)
                .join(SKILL_MD_FILENAME);
            if bundle_path.exists() {
                let bundle_skills = self
                    .load_bundle(&bundle_path, SkillOrigin::Bundle)
                    .await;
                skills.extend(bundle_skills);
            }
        }

        if !dir.exists() {
            debug!(path = %dir.display(), "skills: directory does not exist, skipping");
            return skills;
        }

        let mut rd = match tokio::fs::read_dir(dir).await {
            Ok(r) => r,
            Err(e) => {
                warn!(path = %dir.display(), error = %e, "skills: failed to read directory");
                return skills;
            }
        };

        let mut paths: Vec<PathBuf> = Vec::new();
        while let Ok(Some(entry)) = rd.next_entry().await {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("md") {
                // Skip SKILL.md itself if it ends up in the skills dir
                if p.file_name().and_then(|n| n.to_str()) != Some(SKILL_MD_FILENAME) {
                    paths.push(p);
                }
            }
        }
        paths.sort();

        for path in &paths {
            match self.load_file(path, origin.clone()).await {
                Ok(skill) => {
                    debug!(
                        name = skill.display_name(),
                        path = %path.display(),
                        bytes = skill.body_bytes(),
                        "skills: loaded skill"
                    );
                    skills.push(skill);
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "skills: failed to load skill");
                }
            }
        }

        info!(
            dir = %dir.display(),
            count = skills.len(),
            "skills: scanned directory"
        );
        skills
    }

    /// Parse a single `*.md` skill file.
    async fn load_file(&self, path: &Path, origin: SkillOrigin) -> Result<Skill, SkillError> {
        let raw = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| SkillError::Io(e.to_string()))?;

        let (frontmatter, body) = parse_frontmatter(&raw);

        // Enforce per-skill byte budget
        let body = if body.len() > self.max_skill_bytes {
            let truncated = &body.as_bytes()[..self.max_skill_bytes];
            let safe_end = find_utf8_boundary(truncated);
            let mut b = String::from_utf8_lossy(&body.as_bytes()[..safe_end]).into_owned();
            b.push_str("\n\n[skill truncated at max_skill_bytes limit]");
            warn!(
                path = %path.display(),
                original_bytes = body.len(),
                limit = self.max_skill_bytes,
                "skills: skill body truncated"
            );
            b
        } else {
            body
        };

        let source_path = path.display().to_string();

        Ok(Skill { frontmatter, body, source_path, origin })
    }

    /// Parse `SKILL.md` bundle: split on `## Skill: <name>` headings.
    async fn load_bundle(&self, path: &Path, origin: SkillOrigin) -> Vec<Skill> {
        let raw = match tokio::fs::read_to_string(path).await {
            Ok(r) => r,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "skills: failed to read SKILL.md");
                return Vec::new();
            }
        };

        let mut skills = Vec::new();
        let mut current_name: Option<String> = None;
        let mut current_body = String::new();
        let source = path.display().to_string();

        for line in raw.lines() {
            if let Some(name) = line
                .trim()
                .strip_prefix("## Skill: ")
                .map(str::trim)
                .filter(|n| !n.is_empty())
            {
                if let Some(prev_name) = current_name.take() {
                    skills.push(Skill {
                        frontmatter: Some(SkillFrontmatter {
                            name: prev_name,
                            ..Default::default()
                        }),
                        body: current_body.trim().to_string(),
                        source_path: source.clone(),
                        origin: origin.clone(),
                    });
                }
                current_name = Some(name.to_string());
                current_body = String::new();
            } else if current_name.is_some() {
                current_body.push_str(line);
                current_body.push('\n');
            }
        }

        if let Some(name) = current_name {
            skills.push(Skill {
                frontmatter: Some(SkillFrontmatter {
                    name,
                    ..Default::default()
                }),
                body: current_body.trim().to_string(),
                source_path: source,
                origin,
            });
        }

        skills
    }
}

/// Split raw file content into `(Option<SkillFrontmatter>, body)`.
///
/// YAML frontmatter is delimited by `---` fences at the start of the file.
/// If no valid frontmatter is found, returns `(None, full_content)`.
pub fn parse_frontmatter(raw: &str) -> (Option<SkillFrontmatter>, String) {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---") {
        return (None, raw.to_string());
    }

    // Find closing ---
    let rest = &trimmed[3..];
    let close = rest.find("\n---").or_else(|| rest.find("\r\n---"));
    let Some(close_idx) = close else {
        return (None, raw.to_string());
    };

    let yaml_content = &rest[..close_idx];
    let body_start = close_idx + 4; // skip past "\n---"
    let body = rest.get(body_start..).unwrap_or("").trim_start().to_string();

    match serde_yaml::from_str::<SkillFrontmatter>(yaml_content) {
        Ok(fm) => (Some(fm), body),
        Err(e) => {
            warn!("skills: invalid frontmatter YAML: {e}");
            (None, raw.to_string())
        }
    }
}

/// A directory to scan for skill files.
#[derive(Debug, Clone)]
pub struct SkillSearchDir {
    /// Absolute path to the directory.
    pub path: PathBuf,
    /// Where this directory lives in the precedence hierarchy.
    pub origin: SkillOrigin,
}

impl SkillSearchDir {
    pub fn local(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), origin: SkillOrigin::Local }
    }

    pub fn global(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), origin: SkillOrigin::Global }
    }
}

/// Find the largest byte index ≤ `bytes.len()` that is a valid UTF-8 boundary.
fn find_utf8_boundary(bytes: &[u8]) -> usize {
    let mut end = bytes.len();
    while end > 0 && (bytes[end - 1] & 0xC0) == 0x80 {
        end -= 1;
    }
    end
}
```

#### `crates/xaft-skills/src/injector.rs`

```rust
//! Assemble skill content into the planner system prompt.

use crate::skill::Skill;

/// Builds the extra system context string appended to the planner's prompt.
///
/// Format:
/// ```text
/// ---
/// ## Loaded Skills
///
/// ### django-conventions
/// > Django project conventions and patterns
///
/// <skill body>
///
/// ### pytest-fixtures
/// > Pytest fixture scoping rules
///
/// <skill body>
/// ---
/// ```
///
/// The separator lines (`---`) signal the boundary between the base planner
/// prompt and injected domain knowledge, making it easier to debug prompt
/// content with `RUST_LOG=xaft=debug`.
pub fn build_extra_system_context(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut out = String::from("\n\n---\n## Loaded Skills\n\n");

    for skill in skills {
        let name = skill.display_name();
        out.push_str(&format!("### {name}\n"));
        if !skill.description().is_empty() {
            out.push_str(&format!("> {}\n\n", skill.description()));
        }
        if let Some(ref fm) = skill.frontmatter {
            if let Some(ref aw) = fm.apply_when {
                out.push_str(&format!("*Applies when:* {aw}\n\n"));
            }
        }
        out.push_str(&skill.body);
        out.push_str("\n\n");
    }

    out.push_str("---\n");
    out
}

/// Build a summary line for the TUI signal: `"django-conventions, pytest-fixtures"`.
pub fn skill_names_summary(skills: &[Skill]) -> String {
    skills
        .iter()
        .map(|s| s.display_name().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
```

#### `crates/xaft-skills/src/error.rs`

```rust
use thiserror::Error;

/// Errors from the xaft-skills crate.
///
/// Follows the xaft convention: typed error enum, `thiserror`, no `anyhow`.
#[derive(Debug, Error)]
pub enum SkillError {
    /// I/O failure reading a skill file or directory.
    #[error("I/O error: {0}")]
    Io(String),

    /// YAML frontmatter parse error.
    #[error("frontmatter parse error in {path}: {reason}")]
    FrontmatterParse { path: String, reason: String },

    /// Skill name is empty or invalid.
    #[error("invalid skill name: {0}")]
    InvalidName(String),

    /// Byte budget exceeded — should not propagate as error (handled by
    /// truncation), kept here for internal logging.
    #[error("skill body exceeds max_skill_bytes ({max}): {path}")]
    SizeLimitExceeded { path: String, max: usize },
}
```

### 4.4 Signal: `XaftSkillsLoaded`

Add to `crates/xaft-agent/src/signals.rs`:

```rust
/// Emitted after skill discovery completes at session start.
///
/// Zero skills loaded (all directories empty or feature disabled) is NOT
/// emitted — check `count > 0` on the receiver side.
///
/// Subscribe via the shared `SignalBus` to display a TUI status line:
///
/// ```rust,ignore
/// bus.on::<XaftSkillsLoaded>(|e| {
///     println!("[skills] Loaded {} skills: {}", e.count, e.names.join(", "));
/// }).await;
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XaftSkillsLoaded {
    /// Number of skills loaded.
    pub count: usize,
    /// Display names of all loaded skills, in injection order.
    pub names: Vec<String>,
    /// Total bytes of skill content injected into the planner prompt.
    pub total_bytes: usize,
}
```

### 4.5 Integration in `XaftRuntime::run_task()`

After AGENTS.md loading and before `run_workflow()`:

```rust
// ── Skill loading (PRD 52) ────────────────────────────────────────────────
let skill_extra_context = if self.config.skills.enabled {
    // Build search dirs: global first, then local (local wins on name collision)
    let global_skills_dir = dirs::config_dir()
        .unwrap_or_default()
        .join("xaft")
        .join("skills");
    let local_skills_dir = working_dir.join(".xaft").join("skills");

    let search_dirs = vec![
        SkillSearchDir::global(global_skills_dir),
        SkillSearchDir::local(local_skills_dir),
    ];

    let mut loader = SkillLoader::new(
        DEFAULT_MAX_SKILL_BYTES,
        self.config.skills.max_total_bytes,
    );

    // Apply CLI --skills filter if provided
    if let Some(ref filter) = request.skill_filter {
        loader = loader.with_name_filter(filter.clone());
    }

    let skills = loader.load_from_dirs(&search_dirs).await;

    if !skills.is_empty() {
        let count = skills.len();
        let names: Vec<String> = skills.iter().map(|s| s.display_name().to_string()).collect();
        let total_bytes: usize = skills.iter().map(|s| s.body_bytes()).sum();

        // Emit signal
        let sig = Arc::clone(&self.signals);
        tokio::spawn(async move {
            sig.emit(XaftSkillsLoaded { count, names, total_bytes }).await;
        });

        build_extra_system_context(&skills)
    } else {
        String::new()
    }
} else {
    String::new()
};
```

### 4.6 Injecting into the planner system prompt

The `run_workflow()` signature gains a new `extra_system_context: &str`
parameter. In `orchestrator.rs`:

```rust
pub async fn run_workflow(
    task: &str,
    // … existing parameters …
    extra_hooks: Vec<Arc<dyn ToolHook>>,
    extra_system_context: &str,   // NEW
) -> Result<(String, ExitCode), RuntimeError> {
    // …
    let combined_planner_prompt = if extra_system_context.is_empty() {
        planner_system_prompt(&wd)
    } else {
        format!("{}{}", planner_system_prompt(&wd), extra_system_context)
    };

    let planner_agent = Arc::new(
        NamedAgent::new(PLANNER_NAME, &combined_planner_prompt, 100)
            .with_tools(planner_tools)
            .with_signals(Arc::clone(&signals))
            .with_handoff_flag(Arc::clone(&planner_stop)),
    );
    // …
}
```

### 4.7 `/skills` slash command

New file: `crates/xaft-tui/src/slash/commands/skills.rs`

```rust
//! /skills command handler — lists loaded skills and supports reload.

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

pub struct SkillsHandler;

impl SlashHandler for SkillsHandler {
    fn description(&self) -> &'static str {
        "List loaded skills or reload from disk"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[reload]")
    }

    fn is_async(&self) -> bool {
        true
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Error("use async execute".into())
    }

    fn execute_boxed_async(
        &self,
        ctx: CommandContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = CommandResult> + Send + 'static>> {
        Box::pin(async move {
            let args = ctx.args.as_deref().unwrap_or("").trim();

            if args == "reload" {
                // Emit XaftSkillsReloadRequested signal for the runtime to pick up
                ctx.signals
                    .emit(xaft_agent::signals::XaftSkillsReloadRequested {})
                    .await;
                return CommandResult::Lines(vec![
                    "  Skill reload requested. Changes take effect on next task run.".into(),
                ]);
            }

            // List mode: pull from the cached signal state stored in app
            let skills = ctx.loaded_skills.unwrap_or_default();

            if skills.is_empty() {
                return CommandResult::Lines(vec![
                    "  No skills loaded. Add *.md files to .xaft/skills/ or ~/.config/xaft/skills/".into(),
                    "  Run /skills reload after adding files.".into(),
                ]);
            }

            let mut lines = vec![format!("  {} skill(s) loaded:", skills.len())];
            for skill in &skills {
                let desc = if skill.description.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", skill.description)
                };
                lines.push(format!(
                    "    • {} [{}]{desc}",
                    skill.name, skill.source_path
                ));
            }
            CommandResult::Lines(lines)
        })
    }
}

/// Serialisable summary sent from the runtime to the TUI via `CommandContext`.
#[derive(Debug, Clone)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub source_path: String,
}
```

Register in `crates/xaft-tui/src/slash/commands/mod.rs`:

```rust
pub mod skills;
pub use skills::SkillsHandler;
```

Add to `build_registry_with_config()`:

```rust
.register(SlashCommand::Skills { args: None }, SkillsHandler)
```

Add `Skills { args: Option<String> }` variant to the `SlashCommand` enum in
`crates/xaft-tui/src/slash/parser.rs` / wherever the enum lives.

### 4.8 Dynamic slash commands registration

Dynamic commands (from `## /cmd-name` sections) are discovered after skill
loading and registered into the `SlashCommandRegistry` via a dedicated
`DynamicSkillHandler`:

```rust
/// Handles a dynamic slash command by sending its section body as a user message.
pub struct DynamicSkillHandler {
    pub command: DynamicSkillCommand,
}

impl SlashHandler for DynamicSkillHandler {
    fn description(&self) -> &'static str {
        // Dynamic — fallback to skill name
        "Skill-defined command"
    }

    fn execute(&self, ctx: CommandContext) -> CommandResult {
        // Send the command body as a user message to the agent pipeline
        CommandResult::SubmitToAgent(self.command.as_agent_message())
    }
}
```

`CommandResult::SubmitToAgent(String)` is a new variant that instructs
the TUI to submit the string as if the user typed and submitted it.

Registration loop in the TUI startup path (after skills are loaded):

```rust
for skill in &skills {
    for cmd in skill.dynamic_commands() {
        let slash_cmd = SlashCommand::Dynamic { name: cmd.name.clone() };
        registry.register(slash_cmd, DynamicSkillHandler { command: cmd });
    }
}
```

### 4.9 CLI additions

Add to `RunRequest` in `crates/xaft-runtime/src/dispatch.rs`:

```rust
/// Names of skills to load exclusively. `None` = load all discovered skills.
/// `Some([])` = same as --no-skills (empty filter loads nothing).
pub skill_filter: Option<Vec<String>>,

/// When `true`, disable all skill loading regardless of config.
pub no_skills: bool,
```

Add to `xaft run` CLI flags in `crates/xaft-cli/`:

```rust
/// Load only the named skills (comma-separated). Example: --skills django,pytest
#[arg(long, value_delimiter = ',')]
pub skills: Option<Vec<String>>,

/// Disable skill loading entirely for this run.
#[arg(long, default_value_t = false)]
pub no_skills: bool,
```

---

## 5. Skill File Format Specification

### 5.1 Complete YAML frontmatter schema

```yaml
---
# REQUIRED: machine-readable identifier used for --skills filter.
# Must match /^[a-zA-Z][a-zA-Z0-9_-]*$/.
name: "django-conventions"

# OPTIONAL: semantic version (informational only).
version: "1.2"

# OPTIONAL: human-readable description shown by /skills list.
description: "Django REST framework conventions and anti-patterns"

# OPTIONAL: free-form tags for organisation.
tags:
  - django
  - python
  - web
  - rest

# OPTIONAL: tool names this skill needs the agent to have.
# Advisory only in v1 — no enforcement; emits a WARN if the tool is absent.
tools_required:
  - read_file
  - grep

# OPTIONAL: when this skill should be active (shown in /skills list).
# Not evaluated automatically; purely informational in v1.
apply_when: "*.py files present in workspace"

# OPTIONAL: injection priority 0–100. Higher values inject later (closer to task).
# Default: 50. Use low values for foundation skills, high for task-specific ones.
priority: 60
---
```

### 5.2 Skill body conventions

The body follows the frontmatter fence and is free-form Markdown. Conventions:

- Use `##` headings to organise sub-sections within a skill.
- Use `## /command-name` to register a dynamic slash command (see §4.8).
- Code blocks with language tags (`\`\`\`python`) are shown as-is to the agent.
- Keep each skill focused on a single domain (one concern per file).
- Prefer actionable instructions over explanations: "always use `@transaction.atomic`"
  rather than "transactions ensure consistency".

### 5.3 Full example skill file

```markdown
---
name: "django-rest-framework"
version: "1.0"
description: "DRF serializer and view patterns for this project"
tags: ["django", "python", "api"]
tools_required: ["read_file", "grep"]
apply_when: "rest_framework in requirements.txt"
priority: 65
---

## Serializers

Always inherit from `serializers.ModelSerializer`. Never use `Serializer`
directly unless the data has no model backing.

Field validation goes in `validate_<field>()` methods, not in `save()`.

```python
class UserSerializer(serializers.ModelSerializer):
    class Meta:
        model = User
        fields = ["id", "email", "created_at"]
        read_only_fields = ["id", "created_at"]
```

## Views

Use `generics.RetrieveUpdateDestroyAPIView` for standard CRUD. Never use
`APIView` directly unless the endpoint has unusual logic that does not map
to a single model operation.

## Pagination

All list endpoints MUST use `PageNumberPagination` with `page_size = 20`.
Never return unbounded querysets.

## /django-new-endpoint

When asked to add a new API endpoint, follow this checklist:

1. Add the serializer in `api/serializers/<resource>.py`.
2. Add the view in `api/views/<resource>.py` using the correct generic.
3. Register the URL in `api/urls.py` with a versioned prefix (`/v1/`).
4. Write a test in `tests/api/test_<resource>.py` covering GET, POST, and 401.
5. Update `docs/api.md` with the new endpoint.
```

### 5.4 SKILL.md bundle format

```markdown
# Project Skills Bundle

## Skill: django-rest-framework

Always use `serializers.ModelSerializer`...

## Skill: pytest-conventions

Use `@pytest.fixture(scope="module")` for expensive setup...
```

Each `## Skill: <name>` heading starts a new skill entry. The body continues
until the next `## Skill:` heading or end of file. Frontmatter is not
supported inside bundles — the heading name becomes the skill `name`.

---

## 6. Config Section

Add `SkillsConfig` to `crates/xaft-config/src/types.rs`:

```rust
/// Configuration for the xaft Skills System (PRD 52).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    /// Enable skill discovery and injection.
    /// Default: true
    pub enabled: bool,

    /// Directories to scan for `*.md` skill files, in addition to the
    /// built-in `~/.config/xaft/skills` (global) and `.xaft/skills`
    /// (local) paths. Paths may use `~` for home directory expansion.
    /// Directories are scanned in order; last-found wins on name collision.
    #[serde(default)]
    pub search_paths: Vec<String>,

    /// Maximum bytes per individual skill body. Skills exceeding this limit
    /// are truncated with a notice. Default: 32768 (32 KiB).
    pub max_skill_bytes: usize,

    /// Maximum total bytes across all loaded skills combined.
    /// Skills that would push the total over this limit are silently dropped
    /// (lowest-priority first). Default: 131072 (128 KiB).
    pub max_total_bytes: usize,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            search_paths: vec![".xaft/skills".to_string()],
            max_skill_bytes: 32_768,
            max_total_bytes: 131_072,
        }
    }
}
```

Add to `XaftConfig`:

```rust
/// Skills system configuration.
pub skills: SkillsConfig,
```

Full TOML reference:

```toml
[skills]
# Enable skill loading at session start.
enabled = true

# Additional directories to scan (appended to built-in paths).
search_paths = [".xaft/skills"]

# Maximum bytes for a single skill body (default: 32 KiB).
max_skill_bytes = 32768

# Maximum total bytes across all loaded skills (default: 128 KiB).
max_total_bytes = 131072
```

Environment variable overrides:

| Variable | Config key |
|----------|------------|
| `XAFT_SKILLS_ENABLED` | `skills.enabled` |
| `XAFT_SKILLS_MAX_TOTAL_BYTES` | `skills.max_total_bytes` |

---

## 7. Files Table

| File | Action | Description |
|------|--------|-------------|
| `crates/xaft-skills/` | **Create crate** | New crate: `SkillLoader`, `Skill`, `SkillFrontmatter`, `SkillInjector`, `DynamicSkillCommand`, `SkillError` |
| `crates/xaft-skills/src/lib.rs` | **Create** | `pub mod loader; pub mod skill; pub mod injector; pub mod dynamic_cmd; pub mod error;` |
| `crates/xaft-skills/src/loader.rs` | **Create** | `SkillLoader`, `SkillSearchDir`, `parse_frontmatter`, `find_utf8_boundary` |
| `crates/xaft-skills/src/skill.rs` | **Create** | `Skill`, `SkillFrontmatter`, `SkillOrigin` types |
| `crates/xaft-skills/src/injector.rs` | **Create** | `build_extra_system_context()`, `skill_names_summary()` |
| `crates/xaft-skills/src/dynamic_cmd.rs` | **Create** | `DynamicSkillCommand`, `extract_dynamic_commands()`, `parse_command_heading()` |
| `crates/xaft-skills/src/error.rs` | **Create** | `SkillError` enum |
| `crates/xaft-skills/Cargo.toml` | **Create** | Crate manifest (tokio, serde, serde_yaml, thiserror, tracing) |
| `crates/xaft-skills/tests/skill_loading.rs` | **Create** | Integration tests (see §10) |
| `crates/xaft-skills/tests/skill_injection.rs` | **Create** | Injection + prompt assembly tests |
| `crates/xaft-skills/tests/dynamic_commands.rs` | **Create** | Dynamic command parsing tests |
| `Cargo.toml` | Edit | Add `"crates/xaft-skills"` to `[workspace] members` |
| `crates/xaft-runtime/Cargo.toml` | Edit | Add `xaft-skills` dependency |
| `crates/xaft-runtime/src/runtime.rs` | Edit | Skill loading block after AGENTS.md, pass `extra_system_context` to `run_workflow()` |
| `crates/xaft-runtime/src/orchestrator.rs` | Edit | Accept `extra_system_context: &str` in `run_workflow()`, prefix planner prompt |
| `crates/xaft-runtime/src/dispatch.rs` | Edit | Add `skill_filter: Option<Vec<String>>` and `no_skills: bool` to `RunRequest` |
| `crates/xaft-agent/src/signals.rs` | Edit | Add `XaftSkillsLoaded`, `XaftSkillsReloadRequested` |
| `crates/xaft-config/src/types.rs` | Edit | Add `SkillsConfig` struct and `pub skills: SkillsConfig` to `XaftConfig` |
| `crates/xaft-config/src/defaults.rs` | Edit | `impl Default for SkillsConfig` |
| `crates/xaft-tui/src/slash/commands/skills.rs` | **Create** | `SkillsHandler`, `DynamicSkillHandler`, `SkillSummary` |
| `crates/xaft-tui/src/slash/commands/mod.rs` | Edit | `pub mod skills; pub use skills::SkillsHandler;` |
| `crates/xaft-tui/src/slash/commands/mod.rs` | Edit | Register `SkillsHandler` in `build_registry_with_config()` |
| `crates/xaft-tui/src/slash/parser.rs` (or equivalent) | Edit | Add `Skills` and `Dynamic { name }` variants to `SlashCommand` enum |
| `crates/xaft-cli/src/commands/run.rs` (or args) | Edit | Add `--skills` and `--no-skills` CLI flags |
| `crates/xaft-tui/src/app.rs` (event bridge) | Edit | Subscribe to `XaftSkillsLoaded`, show `[skills]` status line |

---

## 8. Acceptance Criteria

1. **AC-1: Local discovery** — Placing `*.md` files in `.xaft/skills/` at the
   working directory causes them to be loaded and their content to appear in
   the planner's system prompt.

2. **AC-2: Global discovery** — Placing `*.md` files in
   `~/.config/xaft/skills/` causes them to be loaded when no local override
   exists for the same skill name.

3. **AC-3: Local overrides global** — When both local `.xaft/skills/foo.md`
   and global `~/.config/xaft/skills/foo.md` exist with the same `name:` in
   their frontmatter, only the local version is loaded.

4. **AC-4: Per-skill size cap** — A skill body larger than `max_skill_bytes`
   is truncated with `[skill truncated at max_skill_bytes limit]` appended.
   The planner prompt does not contain the truncated portion.

5. **AC-5: Total byte cap** — When total skill content exceeds
   `max_total_bytes`, low-priority skills are silently dropped until the
   total is within budget. No error is raised.

6. **AC-6: Frontmatter parse** — A skill with valid YAML frontmatter has its
   `name`, `description`, `tags`, and `priority` fields correctly parsed.

7. **AC-7: Frontmatter absent** — A skill file with no `---` fences is loaded
   successfully; the file stem is used as the skill name.

8. **AC-8: Dynamic slash commands** — A skill containing `## /django-migrate`
   registers a `/django-migrate` slash command in the TUI registry. Typing
   `/django-migrate` submits the section body as a user message to the agent.

9. **AC-9: `--skills` filter** — Running `xaft run --skills django,pytest "fix"`
   loads only the `django` and `pytest` skills; all other discovered skills
   are ignored.

10. **AC-10: `--no-skills` flag** — Running `xaft run --no-skills "fix"` loads
    no skills even when skill files exist and `skills.enabled = true` in config.

11. **AC-11: `/skills` command** — Typing `/skills` in the TUI lists all
    loaded skills with names, descriptions, and source paths.

12. **AC-12: `/skills reload` command** — Typing `/skills reload` emits
    `XaftSkillsReloadRequested`; the TUI confirms the reload request.

13. **AC-13: `XaftSkillsLoaded` signal** — The signal is emitted with correct
    `count`, `names`, and `total_bytes` when skills are loaded. Not emitted
    when no skills are found.

14. **AC-14: TUI status line** — After loading, the TUI transcript displays
    `[skills] Loaded N skills: name1, name2`.

15. **AC-15: SKILL.md bundle** — A `SKILL.md` at the project root with two
    `## Skill: <name>` sections loads as two separate `Skill` instances.

16. **AC-16: Priority ordering** — Skills with lower `priority` values appear
    earlier in the injected prompt section. A skill with `priority: 10` is
    injected before a skill with `priority: 90`.

---

## 9. Dynamic Slash Commands Design

### 9.1 Command naming rules

- The heading `## /command-name` registers command `command-name`.
- Names must match `/^[a-zA-Z][a-zA-Z0-9_-]*$/`.
- Names must not conflict with built-in commands. On collision, the built-in
  takes precedence and a `WARN` log is emitted.
- Multiple skills may not register the same dynamic command name. The
  last-loaded skill's definition wins (local > global; higher file name
  alphabetically within same origin).

### 9.2 Command invocation flow

```text
User types: /django-migrate
    ↓
SlashCommandRegistry::lookup("django-migrate")
    ↓
DynamicSkillHandler::execute(ctx)
    ↓
CommandResult::SubmitToAgent(body_text)
    ↓
TUI AppState: submit body_text as if user typed it
    ↓
XaftRuntime::run(RunRequest { task: body_text, … })
```

The command body is sent to the full agent pipeline (Planner → Coder → QA
↔ Fixer). The planner sees both the command body and the skill context in its
system prompt, so it can act with full domain knowledge.

### 9.3 Command body templating (v2, not in scope)

In a future version, command bodies may contain `{{variable}}` placeholders
that the TUI replaces with user-supplied values before submitting to the agent.
This is explicitly out of scope for v1 — all command bodies are injected
verbatim.

---

## 10. Tests

All tests live in `crates/xaft-skills/tests/` and in `#[cfg(test)] mod tests`
blocks inside the source files.

### 10.1 `test_load_from_local_skills_dir`

**What it tests:** Skills in `.xaft/skills/` are discovered and loaded.

```rust
#[tokio::test]
async fn test_load_from_local_skills_dir() {
    let tmp = TempDir::new().unwrap();
    let skills_dir = tmp.path().join(".xaft").join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(skills_dir.join("django.md"), "---\nname: django\n---\nUse ModelSerializer.")
        .unwrap();

    let loader = SkillLoader::default();
    let dirs = vec![SkillSearchDir::local(&skills_dir)];
    let skills = loader.load_from_dirs(&dirs).await;

    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].display_name(), "django");
    assert!(skills[0].body.contains("ModelSerializer"));
}
```

### 10.2 `test_local_overrides_global`

**What it tests:** Local skill wins over global on name collision.

```rust
#[tokio::test]
async fn test_local_overrides_global() {
    let global_dir = TempDir::new().unwrap();
    let local_dir = TempDir::new().unwrap();

    std::fs::write(
        global_dir.path().join("foo.md"),
        "---\nname: foo\n---\nGlobal foo body.",
    ).unwrap();
    std::fs::write(
        local_dir.path().join("foo.md"),
        "---\nname: foo\n---\nLocal foo body.",
    ).unwrap();

    let loader = SkillLoader::default();
    let dirs = vec![
        SkillSearchDir::global(global_dir.path()),
        SkillSearchDir::local(local_dir.path()),
    ];
    let skills = loader.load_from_dirs(&dirs).await;

    // Only one skill (deduplicated), and it should be the local one
    assert_eq!(skills.len(), 1);
    assert!(skills[0].body.contains("Local foo body"));
    assert_eq!(skills[0].origin, SkillOrigin::Local);
}
```

### 10.3 `test_frontmatter_parse_full`

**What it tests:** All frontmatter fields are correctly parsed.

```rust
#[test]
fn test_frontmatter_parse_full() {
    let raw = "---\nname: django\nversion: \"2.0\"\ndescription: DRF patterns\ntags:\n  - python\ntools_required:\n  - grep\napply_when: '*.py present'\npriority: 75\n---\nBody content.";
    let (fm, body) = parse_frontmatter(raw);
    let fm = fm.unwrap();
    assert_eq!(fm.name, "django");
    assert_eq!(fm.version.as_deref(), Some("2.0"));
    assert_eq!(fm.description.as_deref(), Some("DRF patterns"));
    assert_eq!(fm.tags, vec!["python"]);
    assert_eq!(fm.tools_required, vec!["grep"]);
    assert_eq!(fm.apply_when.as_deref(), Some("*.py present"));
    assert_eq!(fm.priority, 75);
    assert_eq!(body.trim(), "Body content.");
}
```

### 10.4 `test_frontmatter_absent_fallback`

**What it tests:** A file with no `---` fences loads with `frontmatter: None`
and the full content as body.

```rust
#[test]
fn test_frontmatter_absent_fallback() {
    let raw = "Use ModelSerializer always.";
    let (fm, body) = parse_frontmatter(raw);
    assert!(fm.is_none());
    assert_eq!(body, raw);
}
```

### 10.5 `test_skill_truncation`

**What it tests:** A body larger than `max_skill_bytes` is truncated with
the notice appended.

```rust
#[tokio::test]
async fn test_skill_truncation() {
    let tmp = TempDir::new().unwrap();
    let skills_dir = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    let big_body = format!("---\nname: big\n---\n{}", "x".repeat(100_000));
    std::fs::write(skills_dir.join("big.md"), &big_body).unwrap();

    let loader = SkillLoader::new(512, DEFAULT_MAX_TOTAL_BYTES);
    let dirs = vec![SkillSearchDir::local(&skills_dir)];
    let skills = loader.load_from_dirs(&dirs).await;

    assert_eq!(skills.len(), 1);
    assert!(skills[0].body.contains("[skill truncated at max_skill_bytes limit]"));
    assert!(skills[0].body_bytes() <= 512 + 50); // 50 chars for the notice
}
```

### 10.6 `test_total_byte_budget`

**What it tests:** Skills that would exceed `max_total_bytes` are dropped.

```rust
#[tokio::test]
async fn test_total_byte_budget() {
    let tmp = TempDir::new().unwrap();
    let skills_dir = tmp.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();

    // Three 400-byte skills; budget = 700 → only first two fit
    for i in 0..3u8 {
        std::fs::write(
            skills_dir.join(format!("skill{i}.md")),
            format!("---\nname: skill{i}\n---\n{}", "y".repeat(400)),
        ).unwrap();
    }

    let loader = SkillLoader::new(DEFAULT_MAX_SKILL_BYTES, 700);
    let dirs = vec![SkillSearchDir::local(&skills_dir)];
    let skills = loader.load_from_dirs(&dirs).await;

    // Only two skills should fit in 700 bytes
    assert!(skills.len() <= 2);
    let total: usize = skills.iter().map(|s| s.body_bytes()).sum();
    assert!(total <= 700);
}
```

### 10.7 `test_name_filter`

**What it tests:** `with_name_filter()` restricts loading to named skills.

```rust
#[tokio::test]
async fn test_name_filter() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    for name in &["alpha", "beta", "gamma"] {
        std::fs::write(dir.join(format!("{name}.md")),
            format!("---\nname: {name}\n---\nBody of {name}."))
        .unwrap();
    }

    let loader = SkillLoader::default()
        .with_name_filter(vec!["alpha".to_string(), "gamma".to_string()]);
    let dirs = vec![SkillSearchDir::local(dir)];
    let skills = loader.load_from_dirs(&dirs).await;

    assert_eq!(skills.len(), 2);
    let names: Vec<_> = skills.iter().map(|s| s.display_name()).collect();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"gamma"));
    assert!(!names.contains(&"beta"));
}
```

### 10.8 `test_dynamic_command_extraction`

**What it tests:** `extract_dynamic_commands()` correctly parses `## /cmd`
headings from a skill body.

```rust
#[test]
fn test_dynamic_command_extraction() {
    let body = "## Overview\nSome knowledge.\n\n## /django-migrate\nRun migration steps.\n1. Step one\n2. Step two\n\n## /pytest-run\nRun pytest with coverage.";
    let cmds = extract_dynamic_commands(body, "my-skill");
    assert_eq!(cmds.len(), 2);
    assert_eq!(cmds[0].name, "django-migrate");
    assert!(cmds[0].body.contains("Run migration steps"));
    assert_eq!(cmds[1].name, "pytest-run");
    assert!(cmds[1].body.contains("Run pytest"));
    assert_eq!(cmds[0].skill_name, "my-skill");
}
```

### 10.9 `test_dynamic_command_no_false_positives`

**What it tests:** Regular `## Heading` sections that do not start with `/`
are NOT parsed as dynamic commands.

```rust
#[test]
fn test_dynamic_command_no_false_positives() {
    let body = "## Serializers\nUse ModelSerializer.\n\n## Views\nUse generics.";
    let cmds = extract_dynamic_commands(body, "django");
    assert!(cmds.is_empty());
}
```

### 10.10 `test_prompt_injection_format`

**What it tests:** `build_extra_system_context()` produces a string that
contains skill names, descriptions, and bodies with `---` delimiters.

```rust
#[test]
fn test_prompt_injection_format() {
    let skills = vec![
        Skill {
            frontmatter: Some(SkillFrontmatter {
                name: "alpha".to_string(),
                description: Some("Alpha desc".to_string()),
                ..Default::default()
            }),
            body: "Alpha knowledge.".to_string(),
            source_path: ".xaft/skills/alpha.md".to_string(),
            origin: SkillOrigin::Local,
        },
    ];
    let ctx = build_extra_system_context(&skills);
    assert!(ctx.contains("## Loaded Skills"));
    assert!(ctx.contains("### alpha"));
    assert!(ctx.contains("Alpha desc"));
    assert!(ctx.contains("Alpha knowledge."));
    assert!(ctx.contains("---"));
}
```

### 10.11 `test_skill_md_bundle_parsing`

**What it tests:** `SKILL.md` with two `## Skill:` sections produces two
`Skill` instances with correct names and bodies.

```rust
#[tokio::test]
async fn test_skill_md_bundle_parsing() {
    let tmp = TempDir::new().unwrap();
    let bundle = "## Skill: alpha\nAlpha body.\n\n## Skill: beta\nBeta body.";
    std::fs::write(tmp.path().join("SKILL.md"), bundle).unwrap();

    let loader = SkillLoader::default();
    let local_skills_dir = tmp.path().join(".xaft").join("skills");
    std::fs::create_dir_all(&local_skills_dir).unwrap();

    // SkillLoader checks for SKILL.md at project root when loading local dir
    let dirs = vec![SkillSearchDir::local(&local_skills_dir)];
    let skills = loader.load_from_dirs(&dirs).await;

    assert_eq!(skills.len(), 2);
    let names: Vec<_> = skills.iter().map(|s| s.display_name().to_string()).collect();
    assert!(names.contains(&"alpha".to_string()));
    assert!(names.contains(&"beta".to_string()));
}
```

### 10.12 `test_priority_ordering`

**What it tests:** Skills are sorted by `priority` (ascending), so lower
numbers appear first in the injected prompt.

```rust
#[tokio::test]
async fn test_priority_ordering() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path();
    std::fs::write(dir.join("high.md"),
        "---\nname: high-pri\npriority: 80\n---\nHigh priority body.")
    .unwrap();
    std::fs::write(dir.join("low.md"),
        "---\nname: low-pri\npriority: 20\n---\nLow priority body.")
    .unwrap();

    let loader = SkillLoader::default();
    let skills = loader.load_from_dirs(&[SkillSearchDir::local(dir)]).await;
    assert_eq!(skills.len(), 2);
    assert_eq!(skills[0].display_name(), "low-pri");   // priority 20 first
    assert_eq!(skills[1].display_name(), "high-pri");  // priority 80 last
}
```

---

## 11. Migration & Rollout

1. The feature ships with `skills.enabled = true` by default. Teams that
   do not want skills loaded can set `XAFT_SKILLS_ENABLED=false` or add
   `[skills] enabled = false` to their `xaft.toml`.
2. No existing sessions are affected: skill content is injected fresh at
   each session start.
3. The new `xaft-skills` crate is a library crate with no binary entry
   point. It has no transitive dependencies beyond what the workspace
   already has (tokio, serde, serde_yaml, thiserror, tracing).
4. The `serde_yaml` dependency is new for the workspace. Add it to
   `[workspace.dependencies]` in the root `Cargo.toml`:
   ```toml
   serde_yaml = "0.9"
   ```
5. `RunRequest::skill_filter` and `RunRequest::no_skills` default to
   `None` / `false` respectively — no behavioural change for callers that
   do not set them.
6. The `run_workflow()` signature change (`extra_system_context: &str`) is
   a backward-compatible addition for internal callers within `xaft-runtime`.

---

## 12. Open Questions

1. **Conflict resolution for dynamic commands**: Two skills registering the
   same `## /cmd-name` heading produce a name collision. Current proposal:
   last-loaded wins with a `WARN` log. An alternative is to prefix with the
   skill name (e.g., `/django:migrate`) but this makes the feature less
   ergonomic. Decision deferred to implementation.

2. **`apply_when` auto-evaluation**: The `apply_when` field is currently
   advisory (shown in `/skills list` but not evaluated). A future version
   could evaluate simple glob patterns (`*.py files present`) against the
   workspace listing and auto-skip irrelevant skills. This would reduce
   prompt bloat for polyglot workspaces but requires a glob evaluator.

3. **Skill version pinning**: A project might want to pin `django-conventions`
   to version `1.0` rather than always using the latest from the global
   skills directory. A `[[skills.pin]]` config section could support this:
   ```toml
   [[skills.pin]]
   name = "django-conventions"
   version = "1.0"
   path = ".xaft/skills/pinned/django-1.0.md"
   ```
   Out of scope for v1.

4. **Skill output caching**: Building the planner system prompt is cheap
   (string concatenation), but if skills are numerous or large, the full
   string is rebuilt on every session start. A session-level cache keyed by
   the hash of all skill file mtimes would avoid redundant work. Deferred
   to a follow-up PRD.
