//! xaft-skills — loadable agent knowledge files.
//!
//! Skills are Markdown files (`*.md`), or directories holding a `SKILL.md`, in
//! `.xaft/skills/` or `~/.config/xaft/skills/` that extend agent capabilities
//! with domain knowledge, conventions, and tool-usage patterns.
//! [`installer`] is what `xaft skills add` uses to put them there.
//!
//! # Quick start
//!
//! ```rust,no_run
//! # async fn example() {
//! use std::path::Path;
//! use xaft_skills::{SkillLoader};
//!
//! let loader = SkillLoader::for_working_dir(Path::new("."));
//! let skills = loader.load_all().await;
//! let section = SkillLoader::build_prompt_section(&skills);
//! println!("{section}");
//! # }
//! ```

pub mod bootstrap;
pub mod error;
pub mod installer;
pub mod loader;
pub mod skill;

pub use bootstrap::{DEFAULT_SKILLS, bootstrap_default_skills, default_skill_root};
pub use error::SkillError;
pub use installer::{
    InstallRequest, SkillInstallError, SkillInstallResult, canonical_skill_name, install_skills,
};
pub use loader::{SkillLoader, parse_skill_text};
pub use skill::{Skill, SkillFrontmatter};
