//! Workflow files on disk — the xaft analogue of agenthicc's
//! `_scan_workflow_dir` / `load_python_workflows`.
//!
//! A workflow directory (`.xaft/workflows/` in a project,
//! `~/.config/xaft/workflows/` for the user) may hold two shapes:
//!
//! * `<name>.toml` — one self-contained definition;
//! * `<name>/workflow.toml` — a package whose phases may keep long prompts in
//!   sibling files (`system_prompt_file = "design.md"`).
//!
//! Entries whose name starts with `_` or `.` are drafts, backups or temporary
//! publication directories and are never loaded — the same rule agenthicc
//! applies so a half-written workflow can never be selected.
//!
//! Prompt files must stay inside the workflow's own directory: an absolute
//! path or a `..` component is a load error, so a project workflow cannot read
//! arbitrary files into a prompt.

use std::path::{Component, Path, PathBuf};

use crate::types::WorkflowDefinition;

/// The file name a package-style workflow must contain.
pub const PACKAGE_MANIFEST: &str = "workflow.toml";

/// The project-local workflow directory under a working directory.
pub fn project_workflow_dir(working_dir: &Path) -> PathBuf {
    working_dir.join(".xaft").join("workflows")
}

/// The user-global workflow directory, if a config directory exists.
pub fn user_workflow_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("xaft").join("workflows"))
}

/// A workflow file that could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowLoadError {
    /// The file or package directory.
    pub path: PathBuf,
    /// Why it was skipped.
    pub message: String,
}

impl std::fmt::Display for WorkflowLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}

/// A successfully parsed workflow file.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedWorkflow {
    /// The workflow name — the `name` key, or the file stem / directory name.
    pub name: String,
    /// The definition, with every `*_file` prompt inlined.
    pub definition: WorkflowDefinition,
    /// The file or package directory it came from.
    pub path: PathBuf,
}

/// Whether a directory entry name marks a draft that must not be loaded.
pub fn is_hidden_entry(name: &str) -> bool {
    name.starts_with('_') || name.starts_with('.')
}

/// Load every workflow in `dir`, sorted by entry name.
///
/// A missing directory is not an error. Single files are loaded before
/// packages so that, when both `foo.toml` and `foo/` exist, the package — the
/// shape `create_workflow` publishes — is the one that wins.
pub fn load_workflow_dir(dir: &Path) -> (Vec<LoadedWorkflow>, Vec<WorkflowLoadError>) {
    let mut loaded = Vec::new();
    let mut errors = Vec::new();
    let Ok(read) = std::fs::read_dir(dir) else {
        return (loaded, errors);
    };
    let mut entries: Vec<PathBuf> = read.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    entries.sort_by_key(|p| p.is_dir());

    for path in entries {
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if is_hidden_entry(file_name) {
            continue;
        }
        let candidate = if path.is_dir() {
            if !path.join(PACKAGE_MANIFEST).is_file() {
                continue;
            }
            path.clone()
        } else if path.extension().and_then(|e| e.to_str()) == Some("toml") {
            path.clone()
        } else {
            continue;
        };
        match load_workflow_path(&candidate) {
            Ok(wf) => loaded.push(wf),
            Err(err) => errors.push(err),
        }
    }
    (loaded, errors)
}

/// Load one workflow from a `.toml` file or a package directory.
pub fn load_workflow_path(path: &Path) -> Result<LoadedWorkflow, WorkflowLoadError> {
    let fail = |message: String| WorkflowLoadError {
        path: path.to_path_buf(),
        message,
    };
    let (manifest, base_dir, default_name) = if path.is_dir() {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        (path.join(PACKAGE_MANIFEST), path.to_path_buf(), name)
    } else {
        let name = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
        (path.to_path_buf(), base, name)
    };

    let text = std::fs::read_to_string(&manifest)
        .map_err(|e| fail(format!("cannot read {}: {e}", manifest.display())))?;
    let mut definition = parse_workflow_toml(&text).map_err(fail)?;
    let name = definition
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or(default_name);
    inline_prompt_files(&mut definition, &base_dir).map_err(fail)?;
    Ok(LoadedWorkflow {
        name,
        definition,
        path: path.to_path_buf(),
    })
}

/// Parse a standalone workflow document.
///
/// Two spellings are accepted: a bare definition at the top level, or the
/// `xaft.toml` spelling with exactly one `[workflows.<name>]` table — so a
/// table can be copied out of a config file into its own file unchanged.
pub fn parse_workflow_toml(text: &str) -> Result<WorkflowDefinition, String> {
    let value: toml::Value = toml::from_str(text).map_err(|e| format!("invalid TOML: {e}"))?;
    if let Some(table) = value.as_table() {
        if table.len() == 1 {
            if let Some(toml::Value::Table(workflows)) = table.get("workflows") {
                if workflows.len() == 1 {
                    let (name, body) = workflows.iter().next().expect("one entry");
                    let mut def: WorkflowDefinition = body
                        .clone()
                        .try_into()
                        .map_err(|e: toml::de::Error| format!("invalid workflow: {e}"))?;
                    if def.name.is_none() {
                        def.name = Some(name.clone());
                    }
                    return Ok(def);
                }
            }
        }
    }
    value
        .try_into()
        .map_err(|e: toml::de::Error| format!("invalid workflow: {e}"))
}

/// Replace every `system_prompt_file` / `prompt_file` with the file's text.
///
/// An inline value wins over its file, matching how a CLI flag wins over a
/// config file: the more specific source is the one written in place.
pub fn inline_prompt_files(def: &mut WorkflowDefinition, base_dir: &Path) -> Result<(), String> {
    for phase in &mut def.phases {
        if let Some(rel) = phase.system_prompt_file.clone() {
            if phase.system_prompt.trim().is_empty() {
                phase.system_prompt = read_contained(base_dir, &rel)
                    .map_err(|e| format!("phase '{}': {e}", phase.name))?;
            }
        }
        if let Some(rel) = phase.prompt_file.clone() {
            if phase.prompt.trim().is_empty() {
                phase.prompt = read_contained(base_dir, &rel)
                    .map_err(|e| format!("phase '{}': {e}", phase.name))?;
            }
        }
    }
    Ok(())
}

/// Read `rel` under `base`, refusing anything that could escape it.
fn read_contained(base: &Path, rel: &str) -> Result<String, String> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute()
        || rel_path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(format!(
            "prompt file '{rel}' must be a relative path inside the workflow directory"
        ));
    }
    let full = base.join(rel_path);
    std::fs::read_to_string(&full).map_err(|e| format!("cannot read prompt file '{rel}': {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINEAR: &str = r#"
description = "Two phases."

[[phases]]
name = "plan"
next = "build"

[[phases]]
name = "build"
"#;

    #[test]
    fn a_single_file_loads_with_its_stem_as_the_name() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("two_step.toml"), LINEAR).unwrap();
        let (loaded, errors) = load_workflow_dir(tmp.path());
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "two_step");
        assert_eq!(loaded[0].definition.phases.len(), 2);
        assert!(loaded[0].definition.is_phased());
    }

    #[test]
    fn an_explicit_name_key_wins_over_the_stem() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("file.toml"),
            format!("name = \"renamed\"\n{LINEAR}"),
        )
        .unwrap();
        let (loaded, _) = load_workflow_dir(tmp.path());
        assert_eq!(loaded[0].name, "renamed");
    }

    #[test]
    fn the_config_table_spelling_is_accepted() {
        let def = parse_workflow_toml(
            "[workflows.copied]\ndescription = \"d\"\n[[workflows.copied.phases]]\nname = \"only\"\n",
        )
        .unwrap();
        assert_eq!(def.name.as_deref(), Some("copied"));
        assert_eq!(def.phases[0].name, "only");
    }

    #[test]
    fn a_package_inlines_its_prompt_files() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("pkg_flow");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("design.md"), "Design carefully.").unwrap();
        std::fs::write(
            pkg.join(PACKAGE_MANIFEST),
            "[[phases]]\nname = \"design\"\nsystem_prompt_file = \"design.md\"\n",
        )
        .unwrap();
        let (loaded, errors) = load_workflow_dir(tmp.path());
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(loaded[0].name, "pkg_flow");
        assert_eq!(
            loaded[0].definition.phases[0].system_prompt,
            "Design carefully."
        );
    }

    #[test]
    fn a_prompt_file_cannot_escape_the_workflow_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("evil");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join(PACKAGE_MANIFEST),
            "[[phases]]\nname = \"x\"\nprompt_file = \"../../etc/passwd\"\n",
        )
        .unwrap();
        let (loaded, errors) = load_workflow_dir(tmp.path());
        assert!(loaded.is_empty());
        assert!(errors[0].message.contains("inside the workflow directory"));
    }

    #[test]
    fn drafts_and_hidden_entries_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("_draft.toml"), LINEAR).unwrap();
        std::fs::write(tmp.path().join(".backup.toml"), LINEAR).unwrap();
        std::fs::write(tmp.path().join("notes.md"), "not a workflow").unwrap();
        let (loaded, errors) = load_workflow_dir(tmp.path());
        assert!(loaded.is_empty());
        assert!(errors.is_empty());
    }

    #[test]
    fn a_broken_file_is_reported_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("good.toml"), LINEAR).unwrap();
        std::fs::write(tmp.path().join("bad.toml"), "phases = 3").unwrap();
        let (loaded, errors) = load_workflow_dir(tmp.path());
        assert_eq!(loaded.len(), 1);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].path.ends_with("bad.toml"));
    }

    #[test]
    fn a_missing_directory_is_empty() {
        let (loaded, errors) = load_workflow_dir(Path::new("/definitely/not/here"));
        assert!(loaded.is_empty() && errors.is_empty());
    }
}
