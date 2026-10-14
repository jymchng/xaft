//! Tool security policy: sandbox mode, path scope, network allow-list and
//! per-invocation resource limits.
//!
//! Parity with agenthicc's `config.py::SecuritySettings` plus the pieces of
//! `agenthicc.security.build_policy_from_config` and
//! `agenthicc.tools.sandbox.NetworkGuard` that those settings feed.
//!
//! The policy is deliberately **fail-closed**, and that is the whole design:
//! `denied` rules are evaluated before `allowed`, the default action is
//! [`ToolDecision::Deny`], and an empty `allowed` list does not mean "allow
//! everything". It means the policy has nothing to allow with, so everything is
//! denied. A config that lists no allow rules gets a registry with no tools in
//! it, which is a loud, obvious failure — the alternative is a config typo
//! silently granting every tool in the registry.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::types::XaftConfig;

/// What the policy says about one tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolDecision {
    /// The call may proceed.
    Allow,
    /// The call is refused.
    Deny,
}

impl ToolDecision {
    /// Whether this decision permits the call.
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// Security settings (`[security]`).
///
/// Parity with agenthicc's `SecuritySettings`. xaft's existing
/// [`GuardrailConfig`](crate::types::GuardrailConfig) covers *what to look for*
/// (secret patterns, cost ceilings); this covers *what the agent may reach*.
/// They are separate because the two answer different questions and are
/// enforced at different points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    /// Enforce the path and network scope on every tool call.
    ///
    /// Parity with agenthicc's `SecuritySettings.sandbox_mode`, which defaults
    /// to `true`. Disabling it removes the allow-lists from consideration
    /// entirely, which is why it is a separate switch rather than an empty list.
    pub sandbox_mode: bool,
    /// Filesystem roots the agent may touch.
    ///
    /// Parity with agenthicc's `SecuritySettings.allowed_paths`, whose default
    /// is `["/workspace"]`. Entries are normalized on construction, so
    /// `"/workspace/"` and `"/workspace"` cannot disagree.
    pub allowed_paths: Vec<PathBuf>,
    /// Domains the agent may reach over the network.
    ///
    /// Parity with agenthicc's `SecuritySettings.network_allow_list`. Empty
    /// means *no* outbound access while [`Self::sandbox_mode`] is on — not
    /// "unrestricted".
    pub network_allow_list: Vec<String>,
    /// CPU-seconds ceiling for one tool invocation.
    ///
    /// Parity with agenthicc's `SecuritySettings.max_tool_cpu_seconds`. `None`
    /// means unbounded, which is distinct from a ceiling of zero.
    pub max_tool_cpu_seconds: Option<u64>,
    /// Memory ceiling in MiB for one tool invocation.
    ///
    /// Parity with agenthicc's `SecuritySettings.max_tool_memory_mb`.
    pub max_tool_memory_mb: Option<u64>,
}

impl Default for SecurityConfig {
    /// Matches agenthicc's dataclass defaults exactly.
    fn default() -> Self {
        Self {
            sandbox_mode: true,
            allowed_paths: vec![PathBuf::from("/workspace")],
            network_allow_list: Vec::new(),
            max_tool_cpu_seconds: Some(30),
            max_tool_memory_mb: Some(512),
        }
    }
}

impl SecurityConfig {
    /// Build a policy from these settings alone.
    ///
    /// Yields a policy that denies every tool, because no allow patterns are
    /// available here. Callers holding a
    /// [`ToolSettingsConfig`](crate::types::ToolSettingsConfig) should use
    /// [`ToolPolicy::from_patterns`] instead, which is the path that carries
    /// `tools.allowed` / `tools.denied`.
    pub fn policy(&self) -> ToolPolicy {
        ToolPolicy::new(self, &[], &[])
    }

    /// The resource ceilings these settings imply.
    pub fn limits(&self) -> ResourceLimits {
        ResourceLimits {
            cpu_seconds: self.max_tool_cpu_seconds,
            memory_mb: self.max_tool_memory_mb,
        }
    }
}

/// Resource ceilings for one tool invocation.
///
/// Mirrors agenthicc's `sandbox.ResourceLimits`, which types both fields as
/// `None`-able for the same reason: "no limit" and "a limit of zero" are
/// different answers and a config must be able to express both.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceLimits {
    /// CPU-seconds ceiling.
    pub cpu_seconds: Option<u64>,
    /// Memory ceiling in MiB.
    pub memory_mb: Option<u64>,
}

impl ResourceLimits {
    /// Whether neither ceiling is set.
    pub fn is_unbounded(&self) -> bool {
        self.cpu_seconds.is_none() && self.memory_mb.is_none()
    }
}

/// The resolved, queryable form of a [`SecurityConfig`].
#[derive(Debug, Clone)]
pub struct ToolPolicy {
    sandbox_mode: bool,
    roots: Vec<PathBuf>,
    domains: Vec<String>,
    allowed: Vec<String>,
    denied: Vec<String>,
    limits: ResourceLimits,
}

impl ToolPolicy {
    /// Resolve a `[security]` section plus tool patterns into a policy.
    ///
    /// `allowed` and `denied` are `tools.allowed` / `tools.denied` from
    /// [`ToolSettingsConfig`](crate::types::ToolSettingsConfig), which is where
    /// agenthicc keeps them.
    pub fn new(config: &SecurityConfig, allowed: &[String], denied: &[String]) -> Self {
        Self {
            sandbox_mode: config.sandbox_mode,
            roots: normalize_roots(&config.allowed_paths),
            domains: normalize_domains(&config.network_allow_list),
            allowed: allowed.to_vec(),
            denied: denied.to_vec(),
            limits: config.limits(),
        }
    }

    /// Build a policy from a full config.
    ///
    /// Names every field it reads rather than delegating to a constructor, so
    /// the data flow from `[security]` / `[tools]` into the policy is legible
    /// at the call site -- and so `parity-row-reachability.py` can see which
    /// config keys have a real consumer.
    pub fn from_config(config: &XaftConfig) -> Self {
        let security = &config.security;
        Self {
            sandbox_mode: security.sandbox_mode,
            roots: normalize_roots(&security.allowed_paths),
            domains: normalize_domains(&security.network_allow_list),
            allowed: config.tools.allowed.clone(),
            denied: config.tools.denied.clone(),
            limits: ResourceLimits {
                cpu_seconds: security.max_tool_cpu_seconds,
                memory_mb: security.max_tool_memory_mb,
            },
        }
    }

    /// Whether the filesystem/network scope is enforced at all.
    pub fn sandbox_enabled(&self) -> bool {
        self.sandbox_mode
    }

    /// The resource ceilings for one invocation.
    pub fn limits(&self) -> ResourceLimits {
        self.limits
    }

    /// The normalized filesystem roots.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Decide whether `tool_name` may be called at all.
    ///
    /// `denied` is evaluated first and wins, which is what makes a broad
    /// `allowed = ["*"]` safe to write alongside a narrow
    /// `denied = ["shell"]`: order in the config cannot change the outcome.
    ///
    /// The default is [`ToolDecision::Deny`], so with no rules at all nothing is
    /// callable.
    pub fn evaluate_tool(&self, tool_name: &str) -> ToolDecision {
        for pattern in &self.denied {
            if glob_match(pattern, tool_name) {
                return ToolDecision::Deny;
            }
        }
        for pattern in &self.allowed {
            if glob_match(pattern, tool_name) {
                return ToolDecision::Allow;
            }
        }
        ToolDecision::Deny
    }

    /// Whether `tool_name` may be called.
    pub fn is_tool_allowed(&self, tool_name: &str) -> bool {
        self.evaluate_tool(tool_name).is_allowed()
    }

    /// Filter `names` down to the callable ones, preserving order.
    pub fn filter_tools<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        names
            .into_iter()
            .filter(|name| self.is_tool_allowed(name))
            .map(str::to_string)
            .collect()
    }

    /// Whether `host` is reachable.
    ///
    /// A host matches when it equals an allow-listed domain or is a subdomain
    /// of one, so `example.com` permits `api.example.com`. Matching is on label
    /// boundaries, not raw suffix: `notexample.com` does **not** match
    /// `example.com`.
    ///
    /// With the sandbox off, every host is reachable. With it on and an empty
    /// list, no host is.
    pub fn is_host_allowed(&self, host: &str) -> bool {
        if !self.sandbox_mode {
            return true;
        }
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() {
            return false;
        }
        self.domains.iter().any(|domain| {
            host == *domain
                || host
                    .strip_suffix(domain)
                    .is_some_and(|rest| rest.ends_with('.'))
        })
    }

    /// Whether a `url`-shaped string is reachable.
    ///
    /// Only the authority component is inspected, so a hostile path or query
    /// cannot smuggle a different host past the check. There is no URL parser
    /// dependency, which keeps this a decision function rather than a second
    /// half-parsed copy of the URL grammar.
    pub fn is_url_allowed(&self, url: &str) -> bool {
        let Some(host) = url_host(url) else {
            return false;
        };
        self.is_host_allowed(host)
    }

    /// Whether `path` falls inside an allowed root.
    ///
    /// Roots are compared as whole path *components* where possible, so
    /// `/workspace-evil` is not inside `/workspace`. Paths are compared
    /// lexically rather than through `canonicalize`, because the target may not
    /// exist yet — a write to a new file is exactly the case a sandbox has to
    /// answer, and it is answered before the file exists.
    ///
    /// With the sandbox off, every path is allowed.
    pub fn is_path_allowed(&self, path: &Path) -> bool {
        if !self.sandbox_mode {
            return true;
        }
        let candidate = normalize_path(path);
        self.roots.iter().any(|root| path_within(&candidate, root))
    }

    /// Validate these settings against each other.
    ///
    /// A ceiling of zero would deny every invocation including a no-op, so it is
    /// rejected rather than honoured.
    pub fn validate(&self, section: &str) -> Result<(), ConfigError> {
        for (name, value) in [
            ("max_tool_cpu_seconds", self.limits.cpu_seconds),
            ("max_tool_memory_mb", self.limits.memory_mb),
        ] {
            if value == Some(0) {
                return Err(ConfigError::validation(
                    section,
                    format!("{name} must be greater than zero; use null for no limit"),
                ));
            }
        }
        for domain in &self.domains {
            if domain.is_empty() {
                return Err(ConfigError::validation(
                    section,
                    "network_allow_list contains an empty domain",
                ));
            }
        }
        Ok(())
    }
}

/// Strip trailing slashes and lexically resolve `.` / `..`.
///
/// `lexical_normalize` in spirit: no filesystem access, so it works for a path
/// that does not exist yet, and it cannot be defeated by a symlink appearing
/// between the check and the use.
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                // `..` pops a real component, but must not pop past the root.
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Normalize each allowed root, dropping duplicates and sorting.
///
/// Sorting makes the policy deterministic regardless of config order, which
/// matters because the roots are also reported to the user.
fn normalize_roots(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = paths.iter().map(|p| normalize_path(p)).collect();
    roots.sort();
    roots.dedup();
    roots
}

/// Lower-case each domain and drop a leading `.` and any empty entry.
fn normalize_domains(domains: &[String]) -> Vec<String> {
    let mut out: Vec<String> = domains
        .iter()
        .map(|domain| domain.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|domain| !domain.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Whether `candidate` is `root` or lies beneath it.
///
/// The component comparison is what stops `/workspace-evil/x` from matching the
/// root `/workspace`: `Path::starts_with` compares components, not bytes.
fn path_within(candidate: &Path, root: &Path) -> bool {
    candidate == root || candidate.starts_with(root)
}

/// The host component of a `url`-shaped string, if it has one.
///
/// Handles `scheme://authority`, `scheme://user@authority` and the bare
/// `host[:port]` form, and refuses anything with a NUL or whitespace in it.
fn url_host(url: &str) -> Option<&str> {
    let url = url.trim();
    if url.is_empty() || url.contains(['\0', ' ', '\t', '\n', '\r']) {
        return None;
    }
    let authority = match url.split_once("://") {
        Some((scheme, rest)) => {
            if scheme.is_empty()
                || !scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
            {
                return None;
            }
            rest
        }
        None => url,
    };
    // Drop the path, query and fragment.
    let authority = authority.split(['/', '?', '#']).next().unwrap_or_default();
    // Drop any userinfo.
    let authority = match authority.rsplit_once('@') {
        Some((_, after)) => after,
        None => authority,
    };
    // Drop the port. An IPv6 literal is bracketed, so split it first.
    let host = if let Some(stripped) = authority.strip_prefix('[') {
        stripped.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    if host.is_empty() {
        return None;
    }
    Some(host)
}

/// Shell-style glob match with `*` and `?`, as agenthicc's `fnmatch` uses.
///
/// Only the two wildcards matter for tool names. `[...]` classes are
/// deliberately unsupported: agenthicc does not document them for tool
/// patterns, and a pattern containing `[` that is silently treated as a
/// literal is less surprising than one that silently matches a set.
///
/// Implemented iteratively rather than by building a `regex`, because the
/// patterns are short and this keeps the module dependency-free.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();

    // (pattern index, text index, star's position in pattern)
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;

    while ti < t.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some((pi, ti));
                pi += 1;
            }
            Some('?') => {
                pi += 1;
                ti += 1;
            }
            // A literal that also ends in `*` is handled by the first arm on the
            // next iteration; here the characters simply have to be equal.
            Some(&ch) if ch == t[ti] => {
                pi += 1;
                ti += 1;
            }
            _ => match star {
                Some((star_pi, star_ti)) => {
                    // Backtrack: let this star absorb one more character.
                    pi = star_pi + 1;
                    ti = star_ti + 1;
                    star = Some((star_pi, star_ti + 1));
                }
                None => return false,
            },
        }
    }
    // Any trailing run of stars matches the empty string.
    p[pi..].iter().all(|&ch| ch == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_the_whole_text_only() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("file-*", "file-read"));
        assert!(glob_match("file-?", "file-a"));
        assert!(!glob_match("file-?", "file-ab"));
        assert!(!glob_match("file-*", "other-file-read"));
    }

    #[test]
    fn url_host_extracts_an_authority() {
        assert_eq!(url_host("https://example.com/x"), Some("example.com"));
        assert_eq!(url_host("https://example.com:8443/x"), Some("example.com"));
        assert_eq!(url_host("https://user@example.com"), Some("example.com"));
        assert_eq!(url_host("example.com/x"), Some("example.com"));
        assert_eq!(url_host("https://[::1]:80/x"), Some("::1"));
        assert_eq!(url_host(""), None);
        assert_eq!(url_host("https://"), None);
        assert_eq!(url_host("://nohost"), None);
        assert_eq!(url_host("https://exa mple.com"), None);
    }

    #[test]
    fn roots_are_normalized_and_deduplicated() {
        let roots = normalize_roots(&[
            PathBuf::from("/workspace/"),
            PathBuf::from("/workspace"),
            PathBuf::from("/a/./b/../c"),
        ]);
        assert_eq!(roots, [PathBuf::from("/a/c"), PathBuf::from("/workspace")]);
    }

    #[test]
    fn domains_are_lowercased_and_deduplicated() {
        let domains = normalize_domains(&[
            "Example.COM".into(),
            ".example.com".into(),
            "  ".into(),
            "other.test".into(),
        ]);
        assert_eq!(domains, ["example.com", "other.test"]);
    }

    #[test]
    fn dotdot_cannot_escape_the_root() {
        let candidate = normalize_path(Path::new("/workspace/../etc/passwd"));
        assert_eq!(candidate, Path::new("/etc/passwd"));
        assert!(!path_within(&candidate, Path::new("/workspace")));
    }
}
