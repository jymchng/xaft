//! xaft-skills — loadable agent knowledge files.
//!
//! Skills are Markdown files (`*.md`) in `.xaft/skills/` or
//! `~/.config/xaft/skills/` that extend agent capabilities with domain
//! knowledge, conventions, and tool-usage patterns.
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

pub mod error;
pub mod loader;
pub mod skill;

pub use error::SkillError;
pub use loader::SkillLoader;
pub use skill::{Skill, SkillFrontmatter};
