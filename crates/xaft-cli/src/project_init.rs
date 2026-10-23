//! Project scaffold for `xaft init` (parity matrix row 57).
//!
//! Port of `agenthicc.project_bootstrap.initialize_project` — the half of that
//! module the top-level `agenthicc init` command uses. It creates exactly two
//! files and one directory, and never touches anything else:
//!
//! * `AGENTS.md` at the project root, empty — the project-guidance file the
//!   agent reads;
//! * `.xaft/` — the project configuration directory;
//! * `.xaft/xaft.toml` — an **inert** configuration template in which every
//!   line is a comment.
//!
//! # Why the template is all comments
//!
//! agenthicc's `build_commented_config_template` documents the rule: a routine
//! initialization must never silently change runtime settings. A template that
//! set `log_level = "info"` would override the user's global config the moment
//! it was written, because the project layer outranks the user layer. Every
//! line is therefore commented, and the user uncomments only what they mean.
//!
//! The template is *derived* from [`XaftConfig::default`] rather than written
//! by hand, for the reason agenthicc gives: a hand-written list goes stale the
//! first time a setting is added. It is rendered through
//! [`XaftConfig::redacted`] and [`xaft_config::without_nulls`] — the same pair
//! `xaft config show --format toml` uses — so a key that cannot be printed by
//! one cannot appear in the other.
//!
//! # Idempotence
//!
//! Existing files are preserved unless `force` is set, so running `xaft init`
//! twice is harmless. Writes are atomic: content goes to a temporary file in
//! the same directory, which is then renamed over the target, so a crash can
//! never leave a truncated `AGENTS.md`.
//!
//! # One deliberate divergence
//!
//! agenthicc names the template `.agenthicc/.agenthicc.toml`. xaft's loader
//! reads `.xaft/xaft.toml` (or a flat `.xaft.toml`), so that is the file
//! written here: a template at a path the loader never reads would be inert
//! even after the user uncommented it.

use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use xaft_config::XaftConfig;

/// The project-guidance file `xaft init` creates.
pub const AGENTS_FILENAME: &str = "AGENTS.md";

/// The project configuration directory, relative to the project root.
pub const CONFIG_DIRNAME: &str = ".xaft";

/// The project configuration file, relative to [`CONFIG_DIRNAME`].
pub const CONFIG_FILENAME: &str = "xaft.toml";

/// Why a scaffold could not be created.
///
/// Mirrors agenthicc's `BootstrapError` / `BootstrapWriteError` pair: the first
/// is a refusal decided before anything is written, the second a write that
/// the filesystem rejected.
#[derive(Debug)]
pub enum BootstrapError {
    /// The target layout is unsafe or malformed; nothing was written.
    Refused(String),
    /// A write failed part-way; the message names the file.
    Write(String),
}

impl fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(msg) | Self::Write(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for BootstrapError {}

/// What `xaft init` created, overwrote and left alone.
///
/// Field-for-field agenthicc's `ProjectInitResult`. Each path is absolute; the
/// CLI prints them relative to [`Self::root`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInitResult {
    /// The resolved project root.
    pub root: PathBuf,
    /// `<root>/AGENTS.md`.
    pub agents_path: PathBuf,
    /// `<root>/.xaft`.
    pub config_dir: PathBuf,
    /// `<root>/.xaft/xaft.toml`.
    pub config_path: PathBuf,
    /// Paths that did not exist before this call.
    pub created: Vec<PathBuf>,
    /// Paths that existed and were left untouched.
    pub preserved: Vec<PathBuf>,
    /// Paths that existed and were replaced because `force` was set.
    pub overwritten: Vec<PathBuf>,
}

/// The inert configuration template written to `.xaft/xaft.toml`.
///
/// Every line is a comment — see the module docs for why. The body is the
/// redacted default configuration rendered as TOML, so it lists every
/// setting xaft has, with its default value.
pub fn commented_config_template() -> String {
    let rendered = toml::to_string_pretty(&xaft_config::without_nulls(
        XaftConfig::default().redacted(),
    ))
    .unwrap_or_default();

    let mut out = String::new();
    for line in [
        "xaft project configuration template",
        "",
        "Every line in this file is commented intentionally. Uncomment only the",
        "settings you need; anything left commented keeps its built-in default.",
        "Secrets belong in environment variables, referenced as ${ENV_VAR} or",
        "through api_key_env / token_env, never as literal values in this file.",
        "Run `xaft config show` to see the effective configuration.",
        "",
    ] {
        push_comment(&mut out, line);
    }
    for line in rendered.lines() {
        push_comment(&mut out, line);
    }
    out
}

fn push_comment(out: &mut String, line: &str) {
    if line.is_empty() {
        out.push_str("#\n");
    } else {
        out.push_str("# ");
        out.push_str(line);
        out.push('\n');
    }
}

/// Create the minimal project scaffold under `root`.
///
/// Existing `AGENTS.md` and `.xaft/xaft.toml` files are preserved unless
/// `force` is set. Symlinked roots, symlinked targets and targets that exist
/// but are not regular files are refused before anything is written, exactly
/// as agenthicc refuses them: following a symlink here would let a repository
/// redirect `xaft init` into overwriting a file elsewhere.
pub fn initialize_project(root: &Path, force: bool) -> Result<ProjectInitResult, BootstrapError> {
    if !root.is_dir() {
        return Err(BootstrapError::Refused(format!(
            "Project root is not a directory: {}",
            root.display()
        )));
    }
    if is_symlink(root) {
        return Err(BootstrapError::Refused(format!(
            "Refusing to use symlink project root: {}",
            root.display()
        )));
    }
    let project_root = root.canonicalize().map_err(|e| {
        BootstrapError::Refused(format!(
            "Cannot resolve project root {}: {e}",
            root.display()
        ))
    })?;

    let config_dir = project_root.join(CONFIG_DIRNAME);
    if is_symlink(&config_dir) {
        return Err(BootstrapError::Refused(format!(
            "Refusing to use symlink directory: {}",
            config_dir.display()
        )));
    }
    if config_dir.exists() && !config_dir.is_dir() {
        return Err(BootstrapError::Refused(format!(
            "Project config path is not a directory: {}",
            config_dir.display()
        )));
    }

    let agents_path = project_root.join(AGENTS_FILENAME);
    let config_path = config_dir.join(CONFIG_FILENAME);
    // Validate both targets before creating anything, so a refused layout
    // leaves the tree exactly as it was.
    validate_target(&agents_path)?;
    validate_target(&config_path)?;

    let config_dir_existed = config_dir.exists();
    std::fs::create_dir_all(&config_dir).map_err(|e| {
        BootstrapError::Write(format!("Could not create {}: {e}", config_dir.display()))
    })?;

    let mut result = ProjectInitResult {
        root: project_root,
        agents_path: agents_path.clone(),
        config_dir: config_dir.clone(),
        config_path: config_path.clone(),
        created: Vec::new(),
        preserved: Vec::new(),
        overwritten: Vec::new(),
    };
    if config_dir_existed {
        result.preserved.push(config_dir);
    } else {
        result.created.push(config_dir);
    }

    let template = commented_config_template();
    for (path, content) in [(agents_path, ""), (config_path, template.as_str())] {
        match write_scaffold_file(&path, content, force)? {
            Outcome::Created => result.created.push(path),
            Outcome::Overwritten => result.overwritten.push(path),
            Outcome::Preserved => result.preserved.push(path),
        }
    }
    Ok(result)
}

/// The fate of one scaffold file.
enum Outcome {
    Created,
    Overwritten,
    Preserved,
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// Refuse a symlinked or non-regular scaffold target.
fn validate_target(path: &Path) -> Result<(), BootstrapError> {
    if is_symlink(path) {
        return Err(BootstrapError::Refused(format!(
            "Refusing to use symlink target: {}",
            path.display()
        )));
    }
    if path.exists() && !path.is_file() {
        return Err(BootstrapError::Refused(format!(
            "Scaffold target is not a regular file: {}",
            path.display()
        )));
    }
    Ok(())
}

/// Atomically create (or, with `force`, replace) one scaffold file.
fn write_scaffold_file(path: &Path, content: &str, force: bool) -> Result<Outcome, BootstrapError> {
    validate_target(path)?;
    let existed = path.exists();
    if existed && !force {
        return Ok(Outcome::Preserved);
    }
    write_atomic(path, content.as_bytes())
        .map_err(|e| BootstrapError::Write(format!("Could not write {}: {e}", path.display())))?;
    Ok(if existed {
        Outcome::Overwritten
    } else {
        Outcome::Created
    })
}

/// Write `bytes` to `path` through a temporary sibling and a rename.
///
/// The temporary file is created with `create_new`, so it can never clobber
/// an unrelated file, and it is removed again if any step fails. An existing
/// target keeps its permission bits; a new one gets `0o644`, matching
/// agenthicc.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "scaffold".to_string());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let temporary = parent.join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));

    let existing_permissions = std::fs::metadata(path).ok().map(|m| m.permissions());
    let outcome = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        match existing_permissions {
            Some(permissions) => std::fs::set_permissions(&temporary, permissions)?,
            None => set_default_mode(&temporary)?,
        }
        std::fs::rename(&temporary, path)
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    outcome
}

#[cfg(unix)]
fn set_default_mode(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))
}

#[cfg(not(unix))]
fn set_default_mode(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_directory_gets_both_files_and_the_config_directory() {
        let dir = tempfile::tempdir().unwrap();
        let result = initialize_project(dir.path(), false).unwrap();

        assert!(result.agents_path.is_file());
        assert!(result.config_path.is_file());
        assert_eq!(std::fs::read_to_string(&result.agents_path).unwrap(), "");
        assert_eq!(
            result.created,
            vec![
                result.config_dir.clone(),
                result.agents_path.clone(),
                result.config_path.clone()
            ]
        );
        assert!(result.preserved.is_empty());
        assert!(result.overwritten.is_empty());
    }

    #[test]
    fn existing_files_are_preserved_without_force() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(AGENTS_FILENAME), "my guidance\n").unwrap();
        let result = initialize_project(dir.path(), false).unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join(AGENTS_FILENAME)).unwrap(),
            "my guidance\n"
        );
        assert!(result.preserved.contains(&result.agents_path));
        assert!(result.created.contains(&result.config_path));
    }

    #[test]
    fn force_overwrites_and_reports_it() {
        let dir = tempfile::tempdir().unwrap();
        initialize_project(dir.path(), false).unwrap();
        std::fs::write(dir.path().join(AGENTS_FILENAME), "edited").unwrap();

        let result = initialize_project(dir.path(), true).unwrap();
        assert_eq!(
            std::fs::read_to_string(&result.agents_path).unwrap(),
            "",
            "force must restore the scaffold content"
        );
        assert_eq!(
            result.overwritten,
            vec![result.agents_path.clone(), result.config_path.clone()]
        );
        assert_eq!(result.preserved, vec![result.config_dir.clone()]);
    }

    #[test]
    fn running_twice_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        initialize_project(dir.path(), false).unwrap();
        let before = std::fs::read_to_string(dir.path().join(".xaft/xaft.toml")).unwrap();
        let again = initialize_project(dir.path(), false).unwrap();
        assert!(again.created.is_empty());
        assert_eq!(again.preserved.len(), 3);
        let after = std::fs::read_to_string(dir.path().join(".xaft/xaft.toml")).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn a_missing_root_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let err = initialize_project(&dir.path().join("nope"), false).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("Project root is not a directory")
        );
    }

    #[test]
    fn a_config_path_that_is_a_file_is_refused_and_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_DIRNAME), "not a dir").unwrap();
        let err = initialize_project(dir.path(), false).unwrap_err();
        assert!(err.to_string().contains("is not a directory"));
        assert!(!dir.path().join(AGENTS_FILENAME).exists());
    }

    #[test]
    fn a_directory_at_the_agents_path_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(AGENTS_FILENAME)).unwrap();
        let err = initialize_project(dir.path(), true).unwrap_err();
        assert!(err.to_string().contains("is not a regular file"));
        assert!(
            !dir.path().join(CONFIG_DIRNAME).exists(),
            "a refused layout must not be half-created"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_target_is_refused_even_with_force() {
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere.md");
        std::fs::write(&elsewhere, "do not touch").unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.path().join(AGENTS_FILENAME)).unwrap();

        let err = initialize_project(dir.path(), true).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("Refusing to use symlink target")
        );
        assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "do not touch");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_config_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(CONFIG_DIRNAME)).unwrap();
        let err = initialize_project(dir.path(), false).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("Refusing to use symlink directory")
        );
    }

    #[cfg(unix)]
    #[test]
    fn force_keeps_the_existing_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join(AGENTS_FILENAME);
        std::fs::write(&agents, "x").unwrap();
        std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o600)).unwrap();
        initialize_project(dir.path(), true).unwrap();
        let mode = std::fs::metadata(&agents).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn every_template_line_is_a_comment() {
        let template = commented_config_template();
        assert!(
            template.lines().count() > 20,
            "template is suspiciously short"
        );
        for line in template.lines() {
            assert!(line.starts_with('#'), "uncommented template line: {line:?}");
        }
    }

    #[test]
    fn the_template_is_inert_for_the_loader() {
        // Every line is a comment, so the file parses to an empty table and
        // overrides nothing — the property that makes `init` safe to run.
        let parsed: toml::Table = toml::from_str(&commented_config_template()).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn the_template_names_real_sections_with_their_defaults() {
        let template = commented_config_template();
        for needle in ["# [core]", "# [security]", "log_level = \"info\""] {
            assert!(template.contains(needle), "template is missing {needle:?}");
        }
    }

    #[test]
    fn uncommenting_the_template_yields_a_loadable_config() {
        // The template is only useful if uncommenting it produces something
        // the typed loader accepts.
        let template = commented_config_template();
        let body: String = template
            .lines()
            .filter(|l| !l.starts_with("# xaft project") && !l.contains("Every line"))
            .map(|l| l.strip_prefix("# ").unwrap_or(l.trim_start_matches('#')))
            .collect::<Vec<_>>()
            .join("\n");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xaft.toml");
        // Drop the prose header lines that are not TOML.
        let toml_only: String = body
            .lines()
            .skip_while(|l| !l.starts_with('['))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, toml_only).unwrap();
        xaft_config::ConfigLoader::load_file(&path).expect("uncommented template must load");
    }
}
