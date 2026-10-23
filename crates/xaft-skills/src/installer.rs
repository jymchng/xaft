//! Install skills from a local path, an HTTPS URL or a Git repository.
//!
//! The installer behind `xaft skills add` (parity matrix row 46), ported from
//! `agenthicc.skills.installer`. It accepts the same four kinds of source:
//!
//! | source | example | how it is read |
//! | --- | --- | --- |
//! | local skill | `./my-skill`, `./my-skill/SKILL.md` | copied from disk |
//! | direct URL | `https://host/path/SKILL.md`, a GitHub `/blob/` URL | one HTTPS `GET` |
//! | GitHub repository | `owner/repo`, `https://github.com/o/r`, `…/tree/<rev>/<dir>` | `git clone --depth 1` |
//! | local repository | a directory with no `SKILL.md` at its root | scanned in place |
//!
//! A repository is scanned for skill directories (any directory holding a
//! `SKILL.md`, at most five levels deep, never following symlinks), duplicates
//! are collapsed by canonical name, and every skill found is installed unless
//! `--skill` narrows the set.
//!
//! # Where skills land, and why that is the whole point
//!
//! Skills are installed as directories, `<root>/<slug>/SKILL.md`, under
//! [`SkillLoader::project_skills_dir`] (default) or
//! [`SkillLoader::global_skills_dir`] (`--global`) — the two directories the
//! loader reads, by construction. Every candidate is validated with
//! [`parse_skill_text`], the loader's own rule, before it is moved into place,
//! so an install that reports success is a skill the next session loads.
//!
//! # Safety properties, all carried over from agenthicc
//!
//! * Remote sources must be HTTPS; a Git clone runs with prompts disabled,
//!   `protocol.ext` forbidden and only HTTPS allowed.
//! * A skill is at most 1 MiB and must be UTF-8.
//! * Symlinks inside a skill are refused rather than followed, so a skill
//!   cannot smuggle a link to `~/.ssh` into the skills directory.
//! * An existing skill is never overwritten, and a repository install checks
//!   every target before writing the first one.
//! * Each skill is staged in a temporary directory inside the target root and
//!   renamed into place, so a failed install leaves nothing behind.
//!
//! # Deliberate divergences from agenthicc
//!
//! * **A local skill directory is copied whole.** agenthicc's single-skill
//!   path copies only `SKILL.md` from a local directory, silently dropping the
//!   scripts and references a skill ships with. xaft copies the tree, exactly
//!   as agenthicc already does for a repository skill.
//! * **An existing local path wins over the `owner/repo` shorthand.**
//!   agenthicc reads `a/b` as GitHub `a/b` even when `./a/b` exists. xaft
//!   checks the filesystem first, so pointing at a local directory never
//!   triggers a network clone.
//! * **A skill at a repository's root is named after the repository.**
//!   agenthicc clones into a directory called `repository`, so a root-level
//!   `SKILL.md` installs as `repository`. xaft clones into a directory named
//!   after the repository.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::loader::{SkillLoader, parse_skill_text};

/// Largest skill source accepted, in bytes (agenthicc's `_MAX_SKILL_BYTES`).
pub const MAX_SKILL_SOURCE_BYTES: usize = 1_048_576;

/// Longest canonical skill name (agenthicc's `MAX_SKILL_NAME_LENGTH`).
pub const MAX_SKILL_NAME_LENGTH: usize = 64;

/// How long a repository clone may take.
const GIT_CLONE_TIMEOUT: Duration = Duration::from_secs(300);

/// How long one HTTPS download may take.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Repository directories deeper than this are not scanned.
const MAX_DISCOVERY_DEPTH: usize = 5;

/// Directories never descended into while scanning a repository.
const DISCOVERY_SKIP_DIRS: &[&str] = &[".git", "node_modules", "dist", "build", "__pycache__"];

/// Conventional skill containers, scanned with priority after `skills/`.
const KNOWN_SKILL_CONTAINERS: &[&str] = &[
    ".agents/skills",
    ".claude/skills",
    ".cline/skills",
    ".codex/skills",
    ".github/skills",
    ".opencode/skills",
    ".openhands/skills",
    ".roo/skills",
    ".windsurf/skills",
    ".xaft/skills",
];

/// Why an install was refused or failed.
///
/// The message is the whole contract: the CLI prints `error: <message>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInstallError(pub String);

impl SkillInstallError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for SkillInstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SkillInstallError {}

/// One installed skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInstallResult {
    /// The canonical name the skill was installed under.
    pub slug: String,
    /// The installed skill directory.
    pub path: PathBuf,
    /// `"project"` or `"global"`.
    pub scope: &'static str,
}

/// An install request, as `xaft skills add` builds it.
#[derive(Debug, Clone, Default)]
pub struct InstallRequest {
    /// Local path, HTTPS URL, GitHub URL or `owner/repo`.
    pub source: String,
    /// Install into the user-global skills directory.
    pub global_scope: bool,
    /// Install into the project skills directory (the default).
    pub project_scope: bool,
    /// Install under this name instead of the inferred one.
    pub name: String,
    /// Repository skills to install; empty means all of them.
    pub skill_names: Vec<String>,
    /// The project directory `.xaft/skills` is resolved against.
    pub project_dir: PathBuf,
    /// Override for the global skills directory (tests); `None` uses
    /// [`SkillLoader::global_skills_dir`].
    pub global_dir: Option<PathBuf>,
}

/// Fetches the bytes behind a direct HTTPS skill URL.
///
/// A trait so the install pipeline can be tested without a network; the real
/// implementation is [`HttpFetcher`].
pub trait SkillFetcher: Send + Sync {
    /// GET `url` and return the response body.
    fn fetch(&self, url: &str) -> impl Future<Output = Result<Vec<u8>, SkillInstallError>> + Send;
}

/// The real [`SkillFetcher`]: one HTTPS `GET` with a 30-second timeout.
#[derive(Debug, Clone, Default)]
pub struct HttpFetcher;

impl SkillFetcher for HttpFetcher {
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, SkillInstallError> {
        let client = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|e| SkillInstallError(format!("skill download failed: {e}")))?;
        let response = client
            .get(url)
            .header(reqwest::header::ACCEPT, "text/markdown, text/plain")
            .send()
            .await
            .map_err(classify_http_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(SkillInstallError(format!(
                "skill download failed with HTTP status {}",
                status.as_u16()
            )));
        }
        let bytes = response.bytes().await.map_err(classify_http_error)?;
        Ok(bytes.to_vec())
    }
}

fn classify_http_error(error: reqwest::Error) -> SkillInstallError {
    if let Some(status) = error.status() {
        return SkillInstallError(format!(
            "skill download failed with HTTP status {}",
            status.as_u16()
        ));
    }
    if error.is_timeout() || error.is_connect() || error.is_request() {
        return SkillInstallError::new("skill download failed due to a network error");
    }
    SkillInstallError::new("skill download failed: transport error")
}

/// Install `request.source` with the real network fetcher.
pub async fn install_skills(
    request: &InstallRequest,
) -> Result<Vec<SkillInstallResult>, SkillInstallError> {
    install_skills_with(request, &HttpFetcher).await
}

/// Install `request.source`, fetching direct URLs through `fetcher`.
pub async fn install_skills_with<F: SkillFetcher>(
    request: &InstallRequest,
    fetcher: &F,
) -> Result<Vec<SkillInstallResult>, SkillInstallError> {
    if request.global_scope && request.project_scope {
        return Err(SkillInstallError::new(
            "choose only one target: --global or --project",
        ));
    }
    let source = request.source.trim();
    if source.is_empty() {
        return Err(SkillInstallError::new(
            "a skill URL or local path is required",
        ));
    }
    let root = skill_root(request)?;
    let scope = if request.global_scope {
        "global"
    } else {
        "project"
    };

    match classify_source(source, &request.project_dir) {
        Source::LocalSkill(path) => match local_skill(&path, source)? {
            LocalSkill::Directory { dir, inferred } => {
                let slug = normalise_install_name(&request.name, &inferred)?;
                Ok(vec![install_directory(&dir, &slug, &root, scope)?])
            }
            LocalSkill::File { content, inferred } => {
                let slug = normalise_install_name(&request.name, &inferred)?;
                Ok(vec![install_content(&content, &slug, &root, scope)?])
            }
        },
        Source::Remote(url) => {
            let raw_url = github_raw_url(&url)?;
            let bytes = fetcher.fetch(&raw_url).await?;
            let content = decode_skill(&bytes, source)?;
            let slug = normalise_install_name(&request.name, &remote_name(&url))?;
            Ok(vec![install_content(&content, &slug, &root, scope)?])
        }
        Source::Repository(repository) => {
            let checkout = Checkout::prepare(&repository).await?;
            install_from_repository(request, &repository, checkout.root(), &root, scope)
        }
    }
}

/// Where a request installs to.
fn skill_root(request: &InstallRequest) -> Result<PathBuf, SkillInstallError> {
    if request.global_scope {
        return request
            .global_dir
            .clone()
            .or_else(SkillLoader::global_skills_dir)
            .ok_or_else(|| {
                SkillInstallError::new("could not determine the user home directory for --global")
            });
    }
    Ok(SkillLoader::project_skills_dir(&request.project_dir))
}

/// What a source string turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// A local skill directory or `SKILL.md` file.
    LocalSkill(PathBuf),
    /// A direct HTTPS URL to one `SKILL.md`.
    Remote(String),
    /// A repository to scan.
    Repository(RepositorySource),
}

/// A repository source, normalised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositorySource {
    /// HTTPS clone URL, or an absolute local directory.
    pub url: String,
    /// Branch or tag to check out, if one was named.
    pub revision: Option<String>,
    /// Directory inside the repository to scan, if one was named.
    pub subpath: Option<String>,
    /// Whether `url` is a local directory rather than a remote.
    pub local: bool,
    /// The repository's own name, used for a root-level skill.
    pub name: String,
}

fn classify_source(source: &str, project_dir: &Path) -> Source {
    // An existing local path always wins: see the module docs.
    let candidate = expand_home(source, project_dir);
    if candidate.exists() {
        if candidate.is_dir() && !candidate.join("SKILL.md").is_file() {
            let resolved = candidate.canonicalize().unwrap_or(candidate);
            let name = resolved
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "repository".into());
            return Source::Repository(RepositorySource {
                url: resolved.display().to_string(),
                revision: None,
                subpath: None,
                local: true,
                name,
            });
        }
        return Source::LocalSkill(candidate);
    }
    if let Some(repository) = github_repository_source(source) {
        return Source::Repository(repository);
    }
    if url_scheme(source).is_some() {
        return Source::Remote(source.to_string());
    }
    Source::LocalSkill(candidate)
}

fn expand_home(source: &str, project_dir: &Path) -> PathBuf {
    let path = if let Some(rest) = source.strip_prefix("~/") {
        dirs::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(source))
    } else {
        PathBuf::from(source)
    };
    if path.is_absolute() {
        path
    } else {
        project_dir.join(path)
    }
}

/// The lowercase scheme of a URL-shaped string, if it has one.
fn url_scheme(source: &str) -> Option<String> {
    let (scheme, rest) = source.split_once(':')?;
    let valid = !scheme.is_empty()
        && scheme.len() > 1 // `C:` is a drive letter, not a scheme
        && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    (valid && rest.starts_with("//")).then(|| scheme.to_ascii_lowercase())
}

/// Split an `https://host/path?query#fragment` URL into its parts.
fn split_url(url: &str) -> Option<(String, String, String, String)> {
    let scheme = url_scheme(url)?;
    let rest = &url[scheme.len() + 3..];
    let (rest, fragment) = match rest.split_once('#') {
        Some((r, f)) => (r, f.to_string()),
        None => (rest, String::new()),
    };
    let rest = rest.split_once('?').map_or(rest, |(r, _)| r);
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, String::new()),
    };
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or(authority)
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    Some((scheme, host, path, fragment))
}

/// Path segments of a URL path, percent-decoded, empty segments dropped.
fn path_parts(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|p| !p.is_empty())
        .map(percent_decode)
        .collect()
}

fn percent_decode(value: &str) -> String {
    fn hex(byte: u8) -> Option<u8> {
        (byte as char).to_digit(16).map(|d| d as u8)
    }
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(high * 16 + low);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse a GitHub repository URL, tree URL or `owner/repo` shorthand.
///
/// Port of agenthicc's `_github_repository_source`.
pub fn github_repository_source(source: &str) -> Option<RepositorySource> {
    if let Some((scheme, host, path, fragment)) = split_url(source) {
        if scheme != "https" || !matches!(host.as_str(), "github.com" | "www.github.com") {
            return None;
        }
        let parts = path_parts(&path);
        if parts.len() < 2 {
            return None;
        }
        let owner = &parts[0];
        let repository = parts[1].trim_end_matches(".git").to_string();
        if owner.is_empty() || repository.is_empty() {
            return None;
        }
        let url = format!("https://github.com/{owner}/{repository}.git");
        if parts.len() == 2 {
            return Some(RepositorySource {
                url,
                revision: (!fragment.is_empty()).then_some(fragment),
                subpath: None,
                local: false,
                name: repository,
            });
        }
        if parts.len() >= 4 && parts[2] == "tree" {
            let subpath = parts[4..].join("/");
            return Some(RepositorySource {
                url,
                revision: Some(parts[3].clone()),
                subpath: (!subpath.is_empty()).then_some(subpath),
                local: false,
                name: repository,
            });
        }
        return None;
    }

    if source.matches('/').count() == 1
        && !source.starts_with('.')
        && !source.starts_with('/')
        && !source.contains('\\')
        && !source.contains(':')
    {
        let (owner, repository) = source.split_once('/')?;
        let repository = repository.trim_end_matches(".git");
        if !owner.is_empty() && !repository.is_empty() {
            return Some(RepositorySource {
                url: format!("https://github.com/{owner}/{repository}.git"),
                revision: None,
                subpath: None,
                local: false,
                name: repository.to_string(),
            });
        }
    }
    None
}

/// Convert a direct skill URL into the URL to download.
///
/// GitHub `blob`/`tree` links become `raw.githubusercontent.com` URLs; any
/// other host must already point at a `SKILL.md`. Port of agenthicc's
/// `_github_raw_url`, including its error strings.
pub fn github_raw_url(source: &str) -> Result<String, SkillInstallError> {
    let Some((scheme, host, path, _)) = split_url(source) else {
        return Err(SkillInstallError::new(
            "remote skill sources must use HTTPS",
        ));
    };
    if scheme != "https" {
        return Err(SkillInstallError::new(
            "remote skill sources must use HTTPS",
        ));
    }
    let parts = path_parts(&path);
    if !matches!(host.as_str(), "github.com" | "www.github.com") {
        if path.to_ascii_lowercase().ends_with("/skill.md") {
            return Ok(source.to_string());
        }
        return Err(SkillInstallError::new(
            "HTTPS skill URLs must point directly to SKILL.md",
        ));
    }
    if parts.len() >= 5 && matches!(parts[2].as_str(), "blob" | "tree") {
        let (owner, repository, mode, revision) = (&parts[0], &parts[1], &parts[2], &parts[3]);
        let mut remainder: Vec<String> = parts[4..].to_vec();
        if mode == "tree" {
            remainder.push("SKILL.md".into());
        } else if !remainder
            .last()
            .is_some_and(|last| last.eq_ignore_ascii_case("skill.md"))
        {
            return Err(SkillInstallError::new(
                "GitHub blob URL must point to SKILL.md",
            ));
        }
        let mut segments = vec![owner.clone(), repository.clone(), revision.clone()];
        segments.extend(remainder);
        return Ok(format!(
            "https://raw.githubusercontent.com/{}",
            segments.join("/")
        ));
    }
    Err(SkillInstallError::new(
        "GitHub skill URLs must use /tree/<revision>/<skill> or /blob/<revision>/<skill>/SKILL.md",
    ))
}

/// The name a remote skill is inferred to have: its directory.
fn remote_name(source: &str) -> String {
    let Some((_, _, path, _)) = split_url(source) else {
        return String::new();
    };
    let parts = path_parts(&path);
    match parts.last() {
        Some(last) if last.eq_ignore_ascii_case("skill.md") => {
            parts.len().checked_sub(2).map(|i| parts[i].clone())
        }
        Some(last) => Some(last.clone()),
        None => None,
    }
    .unwrap_or_default()
}

/// Convert a name to agenthicc's canonical kebab-case slug.
///
/// Port of `canonical_skill_name`: lowercase, whitespace and `_` become `-`,
/// anything outside `[a-z0-9-]` becomes `-`, runs of `-` collapse, and leading
/// or trailing `-` are dropped.
pub fn canonical_skill_name(value: &str) -> String {
    let lowered = value.trim().trim_start_matches('/').to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    for c in lowered.chars() {
        let mapped = if c.is_ascii_lowercase() || c.is_ascii_digit() {
            c
        } else {
            '-'
        };
        if mapped == '-' && out.ends_with('-') {
            continue;
        }
        out.push(mapped);
    }
    out.trim_matches('-').to_string()
}

/// Whether `slug` is a valid canonical name (`^[a-z0-9]+(?:-[a-z0-9]+)*$`).
fn is_canonical(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= MAX_SKILL_NAME_LENGTH
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn normalise_install_name(name: &str, inferred: &str) -> Result<String, SkillInstallError> {
    let raw = if name.trim().is_empty() {
        inferred.trim()
    } else {
        name.trim()
    };
    let slug = canonical_skill_name(raw);
    if slug.is_empty() {
        return Err(SkillInstallError::new(
            "could not determine a skill name; pass --name NAME",
        ));
    }
    if slug.len() > MAX_SKILL_NAME_LENGTH {
        return Err(SkillInstallError(format!(
            "skill name is too long (maximum is {MAX_SKILL_NAME_LENGTH} characters)"
        )));
    }
    Ok(slug)
}

fn decode_skill(bytes: &[u8], source: &str) -> Result<String, SkillInstallError> {
    if bytes.len() > MAX_SKILL_SOURCE_BYTES {
        return Err(SkillInstallError(format!(
            "skill source is too large ({} bytes; maximum is {MAX_SKILL_SOURCE_BYTES})",
            bytes.len()
        )));
    }
    String::from_utf8(bytes.to_vec())
        .map_err(|_| SkillInstallError(format!("skill source is not valid UTF-8: {source}")))
}

/// A local skill source, resolved.
enum LocalSkill {
    /// A skill directory: installed whole.
    Directory {
        /// The directory holding `SKILL.md`.
        dir: PathBuf,
        /// The directory name.
        inferred: String,
    },
    /// A single file: installed as that skill's `SKILL.md`, exactly as
    /// agenthicc's `_local_source` installs it.
    File {
        /// The file's text.
        content: String,
        /// The name of the directory the file sits in.
        inferred: String,
    },
}

/// Resolve a local skill source.
fn local_skill(path: &Path, source: &str) -> Result<LocalSkill, SkillInstallError> {
    if !path.exists() {
        return Err(SkillInstallError(format!(
            "skill source does not exist: {source}"
        )));
    }
    if path.is_dir() {
        if !path.join("SKILL.md").is_file() {
            return Err(SkillInstallError(format!(
                "skill source does not contain a readable SKILL.md: {source}"
            )));
        }
        let dir = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let inferred = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        return Ok(LocalSkill::Directory { dir, inferred });
    }
    if !path.is_file() {
        return Err(SkillInstallError(format!(
            "skill source does not contain a readable SKILL.md: {source}"
        )));
    }
    let bytes = std::fs::read(path)
        .map_err(|e| SkillInstallError(format!("could not read skill source: {e}")))?;
    let content = decode_skill(&bytes, &path.display().to_string())?;
    let parent = path
        .canonicalize()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let inferred = parent
        .as_deref()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(LocalSkill::File { content, inferred })
}

/// A repository working copy that is removed when dropped (for a clone).
struct Checkout {
    root: PathBuf,
    _temporary: Option<tempfile::TempDir>,
}

impl Checkout {
    async fn prepare(source: &RepositorySource) -> Result<Self, SkillInstallError> {
        if source.local {
            return Ok(Self {
                root: PathBuf::from(&source.url),
                _temporary: None,
            });
        }
        if !source.url.starts_with("https://") {
            return Err(SkillInstallError::new(
                "remote skill sources must use HTTPS",
            ));
        }
        let temporary = tempfile::Builder::new()
            .prefix("xaft-skills-source-")
            .tempdir()
            .map_err(|e| SkillInstallError(format!("could not prepare a clone directory: {e}")))?;
        let dir_name = canonical_skill_name(&source.name);
        let root = temporary.path().join(if dir_name.is_empty() {
            "repository".to_string()
        } else {
            dir_name
        });
        clone_repository(source, &root).await?;
        Ok(Self {
            root,
            _temporary: Some(temporary),
        })
    }

    fn root(&self) -> &Path {
        &self.root
    }
}

/// The exact `git` argument list a clone runs.
///
/// Separate from [`clone_repository`] so the hardening flags are testable
/// without a network.
pub fn git_clone_args(source: &RepositorySource, destination: &Path) -> Vec<String> {
    let mut args = vec![
        "-c".to_string(),
        "protocol.ext.allow=never".to_string(),
        "clone".to_string(),
        "--depth".to_string(),
        "1".to_string(),
        "--no-tags".to_string(),
    ];
    if let Some(revision) = &source.revision {
        args.push("--branch".into());
        args.push(revision.clone());
    }
    args.push("--".into());
    args.push(source.url.clone());
    args.push(destination.display().to_string());
    args
}

/// Environment applied to the clone: no prompts, HTTPS only, no LFS fetches.
pub const GIT_CLONE_ENV: &[(&str, &str)] = &[
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_ALLOW_PROTOCOL", "https"),
    ("GIT_LFS_SKIP_SMUDGE", "1"),
];

async fn clone_repository(
    source: &RepositorySource,
    destination: &Path,
) -> Result<(), SkillInstallError> {
    let mut command = tokio::process::Command::new("git");
    command
        .args(git_clone_args(source, destination))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for (key, value) in GIT_CLONE_ENV {
        command.env(key, value);
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(SkillInstallError::new(
                "git is required to install a GitHub repository skill",
            ));
        }
        Err(e) => {
            return Err(SkillInstallError(format!("could not start git clone: {e}")));
        }
    };
    let output = match tokio::time::timeout(GIT_CLONE_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            return Err(SkillInstallError(format!("could not start git clone: {e}")));
        }
        Err(_) => {
            return Err(SkillInstallError::new("skill repository clone timed out"));
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("unknown error")
            .trim()
            .to_string();
        return Err(SkillInstallError(format!(
            "skill repository clone failed: {detail}"
        )));
    }
    Ok(())
}

/// A validated skill directory found in a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSkill {
    /// The skill directory.
    pub path: PathBuf,
    /// Canonical name, from the directory name.
    pub slug: String,
    /// The frontmatter `name`, or the directory name.
    pub display_name: String,
}

/// Scan `repository_root` (or `subpath` inside it) for skill directories.
///
/// Port of `_discover_repository_skills`: depth-limited, symlink-refusing,
/// ordered root first, then `skills/`, then known containers, then anything
/// else, and de-duplicated by canonical name with the first one winning.
pub fn discover_repository_skills(
    repository_root: &Path,
    subpath: Option<&str>,
    root_name: &str,
) -> Result<Vec<DiscoveredSkill>, SkillInstallError> {
    let repository_root = repository_root
        .canonicalize()
        .unwrap_or_else(|_| repository_root.to_path_buf());
    let search_root = match subpath {
        Some(sub) => {
            let joined = repository_root.join(sub);
            let resolved = joined.canonicalize().map_err(|_| {
                SkillInstallError(format!("repository skill path does not exist: {sub}"))
            })?;
            if !resolved.starts_with(&repository_root) {
                return Err(SkillInstallError::new(
                    "repository skill path must stay inside the repository",
                ));
            }
            resolved
        }
        None => repository_root.clone(),
    };
    if !search_root.is_dir() {
        return Err(SkillInstallError(format!(
            "repository skill path does not exist: {}",
            subpath.unwrap_or("")
        )));
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    walk(&search_root, 0, &mut candidates);
    candidates.sort_by_key(|path| discovery_priority(path, &repository_root));

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut discovered = Vec::new();
    for path in candidates {
        let dir_name = if path == repository_root {
            root_name.to_string()
        } else {
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let slug = canonical_skill_name(&dir_name);
        if !is_canonical(&slug) || seen.contains(&slug) {
            continue;
        }
        let Some(display_name) = validate_skill_dir(&path, &dir_name) else {
            continue;
        };
        seen.insert(slug.clone());
        discovered.push(DiscoveredSkill {
            path,
            slug,
            display_name,
        });
    }
    Ok(discovered)
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let skill_md = dir.join("SKILL.md");
    if std::fs::symlink_metadata(&skill_md).is_ok_and(|m| m.file_type().is_file()) {
        out.push(dir.to_path_buf());
    }
    if depth >= MAX_DISCOVERY_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| {
            let name = e.file_name();
            !DISCOVERY_SKIP_DIRS.contains(&name.to_string_lossy().as_ref())
        })
        .map(|e| e.path())
        .collect();
    children.sort();
    for child in children {
        walk(&child, depth + 1, out);
    }
}

fn discovery_priority(path: &Path, repository_root: &Path) -> (u8, usize, String) {
    let relative = path.strip_prefix(repository_root).unwrap_or(path);
    let text = relative.to_string_lossy().replace('\\', "/");
    let priority = if text.is_empty() {
        0
    } else if text == "skills" || text.starts_with("skills/") {
        1
    } else if KNOWN_SKILL_CONTAINERS
        .iter()
        .any(|c| text == *c || text.starts_with(&format!("{c}/")))
    {
        2
    } else {
        3
    };
    (priority, relative.components().count(), text)
}

/// Validate a skill directory with the loader's rule; return its display name.
fn validate_skill_dir(dir: &Path, default_name: &str) -> Option<String> {
    let skill_md = dir.join("SKILL.md");
    let metadata = std::fs::symlink_metadata(&skill_md).ok()?;
    if !metadata.file_type().is_file() || metadata.len() as usize > MAX_SKILL_SOURCE_BYTES {
        return None;
    }
    let content = std::fs::read_to_string(&skill_md).ok()?;
    let skill = parse_skill_text(
        &content,
        default_name,
        &skill_md.display().to_string(),
        MAX_SKILL_SOURCE_BYTES,
    )?;
    Some(skill.name)
}

/// Pick the requested skills out of a repository's discovered set.
pub fn select_repository_skills(
    available: Vec<DiscoveredSkill>,
    requested: &[String],
) -> Result<Vec<DiscoveredSkill>, SkillInstallError> {
    if requested.is_empty() {
        return Ok(available);
    }
    let wanted: BTreeSet<String> = requested.iter().map(|n| canonical_skill_name(n)).collect();
    let names: Vec<String> = available.iter().map(|s| s.slug.clone()).collect();
    let selected: Vec<DiscoveredSkill> = available
        .into_iter()
        .filter(|s| {
            wanted.contains(&s.slug) || wanted.contains(&canonical_skill_name(&s.display_name))
        })
        .collect();
    if selected.is_empty() {
        return Err(SkillInstallError(format!(
            "no matching skills found for: {}; available skills: {}",
            requested.join(", "),
            names.join(", ")
        )));
    }
    Ok(selected)
}

fn install_from_repository(
    request: &InstallRequest,
    repository: &RepositorySource,
    checkout: &Path,
    root: &Path,
    scope: &'static str,
) -> Result<Vec<SkillInstallResult>, SkillInstallError> {
    let discovered =
        discover_repository_skills(checkout, repository.subpath.as_deref(), &repository.name)?;
    if discovered.is_empty() {
        return Err(SkillInstallError::new(
            "repository does not contain a valid SKILL.md; pass a direct skill URL or a \
             repository path containing SKILL.md",
        ));
    }
    let selected = select_repository_skills(discovered, &request.skill_names)?;
    if !request.name.trim().is_empty() && selected.len() != 1 {
        return Err(SkillInstallError::new(
            "--name can only be used when installing one repository skill",
        ));
    }
    let planned: Vec<String> = selected
        .iter()
        .map(|skill| normalise_install_name(&request.name, &skill.slug))
        .collect::<Result<_, _>>()?;
    // Refuse before writing anything, so a repository install is all-or-none
    // with respect to name collisions.
    for slug in &planned {
        let target = root.join(slug);
        if target.exists() || is_symlink(&target) {
            return Err(SkillInstallError(format!(
                "skill already exists: {}",
                target.display()
            )));
        }
    }
    selected
        .iter()
        .zip(planned)
        .map(|(skill, slug)| install_directory(&skill.path, &slug, root, scope))
        .collect()
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// Stage a staging directory inside `root`, run `fill`, validate, rename.
fn stage_and_install(
    slug: &str,
    root: &Path,
    scope: &'static str,
    fill: impl FnOnce(&Path) -> Result<(), SkillInstallError>,
) -> Result<SkillInstallResult, SkillInstallError> {
    let target = root.join(slug);
    if target.exists() || is_symlink(&target) {
        return Err(SkillInstallError(format!(
            "skill already exists: {}",
            target.display()
        )));
    }
    std::fs::create_dir_all(root).map_err(|e| {
        SkillInstallError(format!(
            "could not install skill at {}: {e}",
            target.display()
        ))
    })?;
    let staging = tempfile::Builder::new()
        .prefix(&format!(".{slug}-install-"))
        .tempdir_in(root)
        .map_err(|e| {
            SkillInstallError(format!(
                "could not install skill at {}: {e}",
                target.display()
            ))
        })?;
    fill(staging.path())?;
    let content = std::fs::read_to_string(staging.path().join("SKILL.md")).unwrap_or_default();
    if parse_skill_text(&content, slug, "SKILL.md", MAX_SKILL_SOURCE_BYTES).is_none() {
        return Err(SkillInstallError::new(
            "downloaded SKILL.md failed skill metadata validation",
        ));
    }
    let staged = staging.keep();
    if let Err(e) = std::fs::rename(&staged, &target) {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(SkillInstallError(format!(
            "could not install skill at {}: {e}",
            target.display()
        )));
    }
    Ok(SkillInstallResult {
        slug: slug.to_string(),
        path: target,
        scope,
    })
}

/// Install one downloaded `SKILL.md`.
fn install_content(
    content: &str,
    slug: &str,
    root: &Path,
    scope: &'static str,
) -> Result<SkillInstallResult, SkillInstallError> {
    stage_and_install(slug, root, scope, |staging| {
        std::fs::write(staging.join("SKILL.md"), content)
            .map_err(|e| SkillInstallError(format!("could not write SKILL.md: {e}")))
    })
}

/// Install a whole skill directory.
fn install_directory(
    source: &Path,
    slug: &str,
    root: &Path,
    scope: &'static str,
) -> Result<SkillInstallResult, SkillInstallError> {
    stage_and_install(slug, root, scope, |staging| {
        copy_skill_tree(source, staging)
    })
}

/// Copy a skill directory, refusing symlinks and oversized files.
fn copy_skill_tree(source: &Path, destination: &Path) -> Result<(), SkillInstallError> {
    let entries = std::fs::read_dir(source).map_err(|e| {
        SkillInstallError(format!(
            "could not read skill directory {}: {e}",
            source.display()
        ))
    })?;
    let mut entries: Vec<std::fs::DirEntry> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| {
            SkillInstallError(format!("could not copy skill file {}: {e}", path.display()))
        })?;
        if file_type.is_symlink() {
            return Err(SkillInstallError(format!(
                "skill contains unsupported symlink: {}",
                path.display()
            )));
        }
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            if entry.file_name() == ".git" {
                continue;
            }
            std::fs::create_dir_all(&target).map_err(|e| {
                SkillInstallError(format!("could not copy skill file {}: {e}", path.display()))
            })?;
            copy_skill_tree(&path, &target)?;
        } else if file_type.is_file() {
            std::fs::copy(&path, &target).map_err(|e| {
                SkillInstallError(format!("could not copy skill file {}: {e}", path.display()))
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A fetcher that serves canned bodies and records what was asked for.
    #[derive(Default)]
    struct FakeFetcher {
        bodies: HashMap<String, Result<Vec<u8>, SkillInstallError>>,
        requested: Mutex<Vec<String>>,
    }

    impl FakeFetcher {
        fn serving(url: &str, body: &str) -> Self {
            let mut bodies = HashMap::new();
            bodies.insert(url.to_string(), Ok(body.as_bytes().to_vec()));
            Self {
                bodies,
                requested: Mutex::new(Vec::new()),
            }
        }
    }

    impl SkillFetcher for FakeFetcher {
        async fn fetch(&self, url: &str) -> Result<Vec<u8>, SkillInstallError> {
            self.requested.lock().unwrap().push(url.to_string());
            self.bodies
                .get(url)
                .cloned()
                .unwrap_or_else(|| Err(SkillInstallError::new("unexpected url")))
        }
    }

    fn request(source: &str, project: &Path) -> InstallRequest {
        InstallRequest {
            source: source.into(),
            project_dir: project.to_path_buf(),
            ..Default::default()
        }
    }

    fn write_skill(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), body).unwrap();
    }

    #[test]
    fn canonical_names_match_agenthicc() {
        assert_eq!(canonical_skill_name("My Skill"), "my-skill");
        assert_eq!(canonical_skill_name("/snake_case__name"), "snake-case-name");
        assert_eq!(canonical_skill_name("--a..b--"), "a-b");
        assert_eq!(canonical_skill_name("Ünïcode"), "n-code");
        assert!(is_canonical("a-b-1"));
        assert!(!is_canonical("a--b"));
        assert!(!is_canonical(""));
    }

    #[test]
    fn github_sources_are_recognised() {
        let shorthand = github_repository_source("owner/repo").unwrap();
        assert_eq!(shorthand.url, "https://github.com/owner/repo.git");
        assert_eq!(shorthand.name, "repo");

        let tree = github_repository_source("https://github.com/o/r/tree/main/skills/x").unwrap();
        assert_eq!(tree.revision.as_deref(), Some("main"));
        assert_eq!(tree.subpath.as_deref(), Some("skills/x"));

        let pinned = github_repository_source("https://github.com/o/r.git#v1").unwrap();
        assert_eq!(pinned.url, "https://github.com/o/r.git");
        assert_eq!(pinned.revision.as_deref(), Some("v1"));

        assert!(github_repository_source("https://github.com/o/r/blob/main/SKILL.md").is_none());
        assert!(github_repository_source("http://github.com/o/r").is_none());
        assert!(github_repository_source("./relative/path").is_none());
        assert!(github_repository_source("a/b/c").is_none());
    }

    #[test]
    fn raw_urls_follow_agenthicc_rules() {
        assert_eq!(
            github_raw_url("https://github.com/o/r/blob/main/skills/x/SKILL.md").unwrap(),
            "https://raw.githubusercontent.com/o/r/main/skills/x/SKILL.md"
        );
        assert_eq!(
            github_raw_url("https://github.com/o/r/tree/main/skills/x").unwrap(),
            "https://raw.githubusercontent.com/o/r/main/skills/x/SKILL.md"
        );
        assert_eq!(
            github_raw_url("https://example.com/a/SKILL.md").unwrap(),
            "https://example.com/a/SKILL.md"
        );
        assert_eq!(
            github_raw_url("https://example.com/a/readme.md")
                .unwrap_err()
                .to_string(),
            "HTTPS skill URLs must point directly to SKILL.md"
        );
        assert_eq!(
            github_raw_url("http://example.com/a/SKILL.md")
                .unwrap_err()
                .to_string(),
            "remote skill sources must use HTTPS"
        );
        assert!(
            github_raw_url("https://github.com/o/r/blob/main/x/README.md")
                .unwrap_err()
                .to_string()
                .contains("must point to SKILL.md")
        );
    }

    #[test]
    fn remote_names_come_from_the_skill_directory() {
        assert_eq!(remote_name("https://h/a/my-skill/SKILL.md"), "my-skill");
        assert_eq!(remote_name("https://h/a/other"), "other");
    }

    #[test]
    fn the_clone_is_hardened() {
        let source = github_repository_source("https://github.com/o/r/tree/v2").unwrap();
        let args = git_clone_args(&source, Path::new("/tmp/dest"));
        assert_eq!(
            args,
            vec![
                "-c",
                "protocol.ext.allow=never",
                "clone",
                "--depth",
                "1",
                "--no-tags",
                "--branch",
                "v2",
                "--",
                "https://github.com/o/r.git",
                "/tmp/dest"
            ]
        );
        assert!(GIT_CLONE_ENV.contains(&("GIT_TERMINAL_PROMPT", "0")));
        assert!(GIT_CLONE_ENV.contains(&("GIT_ALLOW_PROTOCOL", "https")));
    }

    #[tokio::test]
    async fn a_local_skill_directory_is_installed_whole_into_the_project() {
        let project = tempfile::tempdir().unwrap();
        let source = project.path().join("src-skill");
        write_skill(&source, "---\nname: Review\n---\nReview carefully.\n");
        std::fs::create_dir_all(source.join("scripts")).unwrap();
        std::fs::write(source.join("scripts/check.sh"), "echo ok").unwrap();

        let results = install_skills_with(
            &request("src-skill", project.path()),
            &FakeFetcher::default(),
        )
        .await
        .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].slug, "src-skill");
        assert_eq!(results[0].scope, "project");
        let installed = project.path().join(".xaft/skills/src-skill");
        assert_eq!(results[0].path, installed);
        assert!(installed.join("SKILL.md").is_file());
        assert!(installed.join("scripts/check.sh").is_file());

        // And the loader picks it up — the install is only real if this holds.
        let skills = SkillLoader::for_working_dir(project.path())
            .load_all()
            .await;
        assert!(skills.iter().any(|s| s.name == "Review"), "{skills:?}");
    }

    #[tokio::test]
    async fn a_single_file_is_installed_as_the_skill_md_of_its_directory_name() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join("notes")).unwrap();
        std::fs::write(project.path().join("notes/howto.md"), "How to do it.\n").unwrap();
        std::fs::write(project.path().join("notes/other.md"), "unrelated").unwrap();

        let results = install_skills_with(
            &request("notes/howto.md", project.path()),
            &FakeFetcher::default(),
        )
        .await
        .unwrap();
        assert_eq!(results[0].slug, "notes");
        let installed = project.path().join(".xaft/skills/notes");
        assert_eq!(
            std::fs::read_to_string(installed.join("SKILL.md")).unwrap(),
            "How to do it.\n"
        );
        assert!(
            !installed.join("other.md").exists(),
            "a single-file source must not pull in its siblings"
        );
    }

    #[tokio::test]
    async fn name_overrides_the_inferred_slug() {
        let project = tempfile::tempdir().unwrap();
        write_skill(&project.path().join("x"), "body");
        let mut req = request("x", project.path());
        req.name = "Better Name".into();
        let results = install_skills_with(&req, &FakeFetcher::default())
            .await
            .unwrap();
        assert_eq!(results[0].slug, "better-name");
    }

    #[tokio::test]
    async fn an_existing_skill_is_never_overwritten() {
        let project = tempfile::tempdir().unwrap();
        write_skill(&project.path().join("x"), "body");
        install_skills_with(&request("x", project.path()), &FakeFetcher::default())
            .await
            .unwrap();
        let err = install_skills_with(&request("x", project.path()), &FakeFetcher::default())
            .await
            .unwrap_err();
        assert!(
            err.to_string().starts_with("skill already exists:"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn global_scope_installs_into_the_global_directory() {
        let project = tempfile::tempdir().unwrap();
        let global = tempfile::tempdir().unwrap();
        write_skill(&project.path().join("x"), "body");
        let mut req = request("x", project.path());
        req.global_scope = true;
        req.global_dir = Some(global.path().to_path_buf());
        let results = install_skills_with(&req, &FakeFetcher::default())
            .await
            .unwrap();
        assert_eq!(results[0].scope, "global");
        assert!(global.path().join("x/SKILL.md").is_file());
        assert!(!project.path().join(".xaft").exists());
    }

    #[tokio::test]
    async fn both_scopes_are_refused() {
        let project = tempfile::tempdir().unwrap();
        let mut req = request("x", project.path());
        req.global_scope = true;
        req.project_scope = true;
        let err = install_skills_with(&req, &FakeFetcher::default())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "choose only one target: --global or --project"
        );
    }

    #[tokio::test]
    async fn an_empty_or_missing_source_is_refused() {
        let project = tempfile::tempdir().unwrap();
        let err = install_skills_with(&request("  ", project.path()), &FakeFetcher::default())
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "a skill URL or local path is required");
        let err = install_skills_with(
            &request("./missing", project.path()),
            &FakeFetcher::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "skill source does not exist: ./missing");
    }

    #[tokio::test]
    async fn a_skill_with_an_empty_body_fails_validation_and_leaves_nothing() {
        let project = tempfile::tempdir().unwrap();
        write_skill(&project.path().join("x"), "---\nname: x\n---\n");
        let err = install_skills_with(&request("x", project.path()), &FakeFetcher::default())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "downloaded SKILL.md failed skill metadata validation"
        );
        let root = project.path().join(".xaft/skills");
        let leftovers: Vec<_> = std::fs::read_dir(&root).unwrap().collect();
        assert!(leftovers.is_empty(), "staging was not cleaned up");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_inside_a_skill_is_refused() {
        let project = tempfile::tempdir().unwrap();
        let source = project.path().join("x");
        write_skill(&source, "body");
        std::os::unix::fs::symlink("/etc/passwd", source.join("leak")).unwrap();
        let err = install_skills_with(&request("x", project.path()), &FakeFetcher::default())
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("skill contains unsupported symlink")
        );
        assert!(!project.path().join(".xaft/skills/x").exists());
    }

    #[tokio::test]
    async fn a_direct_url_is_fetched_through_the_raw_url() {
        let project = tempfile::tempdir().unwrap();
        let fetcher = FakeFetcher::serving(
            "https://raw.githubusercontent.com/o/r/main/skills/fmt/SKILL.md",
            "---\ndescription: formats\n---\nFormat things.\n",
        );
        let results = install_skills_with(
            &request(
                "https://github.com/o/r/blob/main/skills/fmt/SKILL.md",
                project.path(),
            ),
            &fetcher,
        )
        .await
        .unwrap();
        assert_eq!(results[0].slug, "fmt");
        assert_eq!(
            std::fs::read_to_string(project.path().join(".xaft/skills/fmt/SKILL.md")).unwrap(),
            "---\ndescription: formats\n---\nFormat things.\n"
        );
        assert_eq!(fetcher.requested.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_oversized_or_non_utf8_download_is_refused() {
        let project = tempfile::tempdir().unwrap();
        let url = "https://example.com/big/SKILL.md";
        let mut fetcher = FakeFetcher::default();
        fetcher
            .bodies
            .insert(url.into(), Ok(vec![b'x'; MAX_SKILL_SOURCE_BYTES + 1]));
        let err = install_skills_with(&request(url, project.path()), &fetcher)
            .await
            .unwrap_err();
        assert!(err.to_string().starts_with("skill source is too large"));

        fetcher.bodies.insert(url.into(), Ok(vec![0xff, 0xfe]));
        let err = install_skills_with(&request(url, project.path()), &fetcher)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("skill source is not valid UTF-8: {url}")
        );
    }

    #[tokio::test]
    async fn a_plain_http_url_is_refused_before_any_fetch() {
        let project = tempfile::tempdir().unwrap();
        let fetcher = FakeFetcher::default();
        let err = install_skills_with(
            &request("http://example.com/SKILL.md", project.path()),
            &fetcher,
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "remote skill sources must use HTTPS");
        assert!(fetcher.requested.lock().unwrap().is_empty());
    }

    fn repository(dir: &Path) {
        write_skill(
            &dir.join("skills/alpha"),
            "---\nname: Alpha\n---\nalpha body\n",
        );
        write_skill(&dir.join(".claude/skills/beta"), "beta body\n");
        // A duplicate of alpha deeper down loses to the `skills/` copy.
        write_skill(&dir.join("vendor/x/alpha"), "duplicate alpha\n");
        // Ignored: inside node_modules, and too deep.
        write_skill(&dir.join("node_modules/pkg/gamma"), "gamma\n");
        write_skill(&dir.join("a/b/c/d/e/f/delta"), "delta\n");
        std::fs::write(dir.join("README.md"), "repo").unwrap();
    }

    #[test]
    fn discovery_orders_dedupes_and_skips() {
        let repo = tempfile::tempdir().unwrap();
        repository(repo.path());
        let found = discover_repository_skills(repo.path(), None, "repo").unwrap();
        let slugs: Vec<&str> = found.iter().map(|s| s.slug.as_str()).collect();
        assert_eq!(slugs, vec!["alpha", "beta"]);
        assert_eq!(found[0].display_name, "Alpha");
        assert!(found[0].path.ends_with("skills/alpha"));
    }

    #[test]
    fn discovery_refuses_a_subpath_outside_the_repository() {
        let outer = tempfile::tempdir().unwrap();
        let repo = outer.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        write_skill(&outer.path().join("escape"), "x");
        let err = discover_repository_skills(&repo, Some("../escape"), "repo").unwrap_err();
        assert_eq!(
            err.to_string(),
            "repository skill path must stay inside the repository"
        );
    }

    #[tokio::test]
    async fn a_local_repository_installs_every_skill() {
        let project = tempfile::tempdir().unwrap();
        let repo = project.path().join("repo");
        repository(&repo);
        let results =
            install_skills_with(&request("repo", project.path()), &FakeFetcher::default())
                .await
                .unwrap();
        let slugs: Vec<&str> = results.iter().map(|r| r.slug.as_str()).collect();
        assert_eq!(slugs, vec!["alpha", "beta"]);
        let skills = SkillLoader::for_working_dir(project.path())
            .load_all()
            .await;
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"Alpha") && names.contains(&"beta"),
            "{names:?}"
        );
    }

    #[tokio::test]
    async fn skill_selection_narrows_and_reports_what_exists() {
        let project = tempfile::tempdir().unwrap();
        let repo = project.path().join("repo");
        repository(&repo);
        let mut req = request("repo", project.path());
        req.skill_names = vec!["beta".into()];
        let results = install_skills_with(&req, &FakeFetcher::default())
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].slug, "beta");

        let mut req = request("repo", project.path());
        req.skill_names = vec!["nope".into()];
        let err = install_skills_with(&req, &FakeFetcher::default())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "no matching skills found for: nope; available skills: alpha, beta"
        );
    }

    #[tokio::test]
    async fn name_with_several_repository_skills_is_refused() {
        let project = tempfile::tempdir().unwrap();
        let repo = project.path().join("repo");
        repository(&repo);
        let mut req = request("repo", project.path());
        req.name = "one".into();
        let err = install_skills_with(&req, &FakeFetcher::default())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "--name can only be used when installing one repository skill"
        );
    }

    #[tokio::test]
    async fn a_repository_collision_writes_nothing() {
        let project = tempfile::tempdir().unwrap();
        let repo = project.path().join("repo");
        repository(&repo);
        std::fs::create_dir_all(project.path().join(".xaft/skills/beta")).unwrap();
        let err = install_skills_with(&request("repo", project.path()), &FakeFetcher::default())
            .await
            .unwrap_err();
        assert!(err.to_string().starts_with("skill already exists:"));
        assert!(
            !project.path().join(".xaft/skills/alpha").exists(),
            "alpha must not be installed when beta collides"
        );
    }

    #[tokio::test]
    async fn a_repository_without_skills_is_reported() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join("empty/src")).unwrap();
        let err = install_skills_with(&request("empty", project.path()), &FakeFetcher::default())
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("repository does not contain a valid SKILL.md")
        );
    }

    #[test]
    fn an_existing_local_path_beats_the_github_shorthand() {
        let project = tempfile::tempdir().unwrap();
        write_skill(&project.path().join("owner/repo"), "body");
        assert!(matches!(
            classify_source("owner/repo", project.path()),
            Source::LocalSkill(_)
        ));
        let elsewhere = tempfile::tempdir().unwrap();
        assert!(matches!(
            classify_source("owner/repo", elsewhere.path()),
            Source::Repository(RepositorySource { local: false, .. })
        ));
    }

    #[test]
    fn percent_decoding_handles_escapes_and_bad_escapes() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
}
