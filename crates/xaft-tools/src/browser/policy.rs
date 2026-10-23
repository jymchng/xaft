//! Browser navigation and input policy.
//!
//! Port of agenthicc's `tools/cloakbrowser/policy.py`. The policy is
//! independent of the browser engine, so it is enforced (and unit-tested)
//! before any browser process exists: every destination is checked here
//! before navigation, again on the URL the page actually lands on, and — when
//! a restricted allow-list is configured — on every sub-resource request the
//! page makes (see `engine.rs`).
//!
//! # Where this deliberately differs from agenthicc
//!
//! * **URLs are parsed by the WHATWG standard** (`url` crate), not by a
//!   Python-`urlsplit`-style splitter. Chromium parses by WHATWG too, so the
//!   host this policy approves is the host the browser visits. agenthicc's
//!   splitter reads `http://evil.example\@allowed.example/` as host
//!   `allowed.example` while Chromium visits `evil.example`; here both read
//!   `evil.example`. Allow-list hosts are IDNA-normalised the same way, so
//!   `bücher.example` matches the `xn--bcher-kva.example` Chromium requests.
//! * **`localhost` and `*.localhost` resolve to loopback without DNS**, which
//!   is what Chromium itself does (RFC 6761). The loopback rule therefore
//!   judges the address the browser will really use.
//! * **NAT64 (`64:ff9b::/96`) addresses are judged by their embedded IPv4
//!   address**, so a hostname resolving to `64:ff9b::7f00:1` is loopback.
//! * **A loopback address is judged by the loopback rule alone.** CPython
//!   files `::1` under "reserved" too, so agenthicc's private/reserved rule
//!   refuses Playwright's always-allowed loopback whenever `localhost`
//!   resolves to `::1` (see `BrowserPolicy::check_address`).
//!
//! Everything else — the error kinds, the sentences, the bounds, the
//! allow-list grammar and the IP classification tables (CPython's
//! `ipaddress`) — is agenthicc's.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use regex::Regex;
use url::{Host, Url};

use super::errors::{BrowserErrorKind, BrowserToolError};

/// agenthicc `_SAFE_KEYS`: named keys the agent may press.
pub const SAFE_KEYS: &[&str] = &[
    "enter",
    "tab",
    "escape",
    "space",
    "backspace",
    "delete",
    "arrowleft",
    "arrowright",
    "arrowup",
    "arrowdown",
    "home",
    "end",
    "pageup",
    "pagedown",
];

/// The wait conditions `*_wait_for` accepts.
pub const WAIT_CONDITIONS: &[&str] = &["selector", "text", "url", "load_state"];

/// The load states `*_wait_for(condition="load_state")` accepts.
pub const LOAD_STATES: &[&str] = &["domcontentloaded", "load", "networkidle"];

/// A future resolving a hostname to addresses.
pub type ResolveFuture = Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>>;

/// A hostname resolver. The default uses the system resolver; tests inject a
/// fixed table, exactly as agenthicc's `BrowserPolicy(resolver=...)` does.
pub type Resolver = Arc<dyn Fn(String) -> ResolveFuture + Send + Sync>;

/// The system resolver (`getaddrinfo` through tokio).
pub fn system_resolver() -> Resolver {
    Arc::new(|host: String| -> ResolveFuture {
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let mut out: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();
            out.sort();
            out.dedup();
            Ok(out)
        })
    })
}

/// Which destination ports a policy admits for non-exact-origin rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowedPorts {
    /// Every port 1-65535.
    All,
    /// Exactly these ports.
    Only(BTreeSet<u16>),
}

impl AllowedPorts {
    /// agenthicc's CloakBrowser default for a restricted allow-list: `{80, 443}`.
    pub fn web_defaults() -> Self {
        Self::Only([80, 443].into_iter().collect())
    }

    fn contains(&self, port: u16) -> bool {
        match self {
            Self::All => port >= 1,
            Self::Only(set) => set.contains(&port),
        }
    }
}

/// Why an allow-list could not be built. Only ever logged; the agent sees the
/// fixed sentence "Browser allow-list configuration is invalid."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyConfigError(pub String);

impl fmt::Display for PolicyConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PolicyConfigError {}

/// One normalised bare-domain or CORS-style origin rule (agenthicc
/// `_AllowedOrigin`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct AllowedOrigin {
    host: String,
    scheme: Option<String>,
    port: Option<u16>,
    subdomains: bool,
    include_root: bool,
    exact_origin: bool,
}

impl AllowedOrigin {
    fn matches(&self, scheme: &str, host: &str, effective_port: u16, ports: &AllowedPorts) -> bool {
        if let Some(required) = &self.scheme {
            if scheme != required {
                return false;
            }
        }
        if self.exact_origin {
            if self.port != Some(effective_port) {
                return false;
            }
        } else if !ports.contains(effective_port) {
            return false;
        }
        if host == self.host {
            return self.include_root;
        }
        self.subdomains
            && host
                .strip_suffix(self.host.as_str())
                .is_some_and(|rest| rest.ends_with('.'))
    }
}

/// Normalise a host the way Chromium will see it: canonical IP text, or the
/// IDNA (punycode) form of a domain without a trailing dot.
///
/// A host that is not a valid WHATWG host is kept verbatim (lower-cased).
/// agenthicc accepts such an entry and it simply never matches; keeping that
/// behaviour means a typo narrows the allow-list rather than invalidating it.
fn normalize_host(raw: &str) -> String {
    let trimmed = raw.trim_end_matches('.');
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        return ip.to_string();
    }
    match Host::parse(trimmed) {
        Ok(Host::Domain(d)) => d.trim_end_matches('.').to_string(),
        Ok(Host::Ipv4(ip)) => ip.to_string(),
        Ok(Host::Ipv6(ip)) => ip.to_string(),
        Err(_) => trimmed.to_string(),
    }
}

/// Parse a bare host or an HTTP(S) origin, refusing URL paths
/// (agenthicc `_parse_allowed_origin`).
fn parse_allowed_origin(value: &str) -> Result<AllowedOrigin, PolicyConfigError> {
    let raw = value.trim().to_lowercase();
    if raw.is_empty() || raw == "*" || raw == "null" {
        return Err(PolicyConfigError(
            "CloakBrowser allow-list entries must name an origin or host".into(),
        ));
    }

    if let Some((scheme, rest)) = raw.split_once("://") {
        let (authority, tail) = match rest.find(['/', '?', '#']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        let shape_error = || {
            PolicyConfigError(
                "CloakBrowser origins must be HTTP(S) host origins without credentials or paths"
                    .into(),
            )
        };
        if !matches!(scheme, "http" | "https") || authority.contains('@') {
            return Err(shape_error());
        }
        if !(tail.is_empty() || tail == "/") {
            return Err(shape_error());
        }
        let (host, port) = split_authority(authority)
            .map_err(|_| PolicyConfigError("CloakBrowser origin is malformed".into()))?;
        let host = host.trim_end_matches('.');
        if host.is_empty() {
            return Err(shape_error());
        }
        let wildcard = host.starts_with("*.");
        let host = if wildcard { &host[2..] } else { host };
        if host.is_empty() || host.contains('*') {
            return Err(PolicyConfigError(
                "CloakBrowser origin wildcard must be a leading '*.domain' prefix".into(),
            ));
        }
        let default_port = if scheme == "https" { 443 } else { 80 };
        return Ok(AllowedOrigin {
            host: normalize_host(host),
            scheme: Some(scheme.to_string()),
            port: Some(port.unwrap_or(default_port)),
            subdomains: wildcard,
            include_root: !wildcard,
            exact_origin: true,
        });
    }

    if raw.contains(['/', '?', '#', '@']) {
        return Err(PolicyConfigError(
            "CloakBrowser host entries must not contain paths or URL components".into(),
        ));
    }
    let wildcard = raw.starts_with("*.");
    let host = if wildcard {
        &raw[2..]
    } else {
        raw.trim_start_matches('.')
    };
    if host.is_empty() || host.contains('*') || host.chars().any(char::is_whitespace) {
        return Err(PolicyConfigError(
            "CloakBrowser host entry is malformed".into(),
        ));
    }
    Ok(AllowedOrigin {
        host: normalize_host(host),
        scheme: None,
        port: None,
        subdomains: true,
        include_root: !wildcard,
        exact_origin: false,
    })
}

/// Split `host[:port]` / `[v6]:port`; a non-numeric or out-of-range port is an
/// error, an empty one means "no port" (Python's `SplitResult.port`).
fn split_authority(authority: &str) -> Result<(&str, Option<u16>), ()> {
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let end = rest.find(']').ok_or(())?;
        let host = &rest[..end];
        let after = &rest[end + 1..];
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p),
            None if after.is_empty() => None,
            None => return Err(()),
        };
        (host, port)
    } else {
        if authority.contains(']') {
            return Err(());
        }
        match authority.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    let port = match port {
        None | Some("") => None,
        Some(p) if p.chars().all(|c| c.is_ascii_digit()) => {
            Some(p.parse::<u32>().ok().filter(|v| *v <= 65535).ok_or(())? as u16)
        }
        Some(_) => return Err(()),
    };
    Ok((host, port))
}

/// How an address is classified, using CPython `ipaddress` semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IpClass {
    /// `is_loopback`.
    pub loopback: bool,
    /// `is_private` (not globally reachable per the IANA special registries).
    pub private: bool,
    /// `is_link_local`.
    pub link_local: bool,
    /// `is_reserved`.
    pub reserved: bool,
}

fn v4_in(addr: Ipv4Addr, net: [u8; 4], prefix: u32) -> bool {
    let a = u32::from(addr);
    let n = u32::from(Ipv4Addr::from(net));
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    (a & mask) == (n & mask)
}

fn v6_in(addr: Ipv6Addr, net: [u16; 8], prefix: u32) -> bool {
    let a = u128::from(addr);
    let n = u128::from(Ipv6Addr::from(net));
    let mask = if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    };
    (a & mask) == (n & mask)
}

fn classify_v4(addr: Ipv4Addr) -> IpClass {
    const PRIVATE: &[([u8; 4], u32)] = &[
        ([0, 0, 0, 0], 8),
        ([10, 0, 0, 0], 8),
        ([127, 0, 0, 0], 8),
        ([169, 254, 0, 0], 16),
        ([172, 16, 0, 0], 12),
        ([192, 0, 0, 0], 24),
        ([192, 0, 0, 170], 31),
        ([192, 0, 2, 0], 24),
        ([192, 168, 0, 0], 16),
        ([198, 18, 0, 0], 15),
        ([198, 51, 100, 0], 24),
        ([203, 0, 113, 0], 24),
        ([240, 0, 0, 0], 4),
        ([255, 255, 255, 255], 32),
    ];
    const PRIVATE_EXCEPTIONS: &[([u8; 4], u32)] = &[([192, 0, 0, 9], 32), ([192, 0, 0, 10], 32)];
    let private = PRIVATE.iter().any(|(n, p)| v4_in(addr, *n, *p))
        && !PRIVATE_EXCEPTIONS.iter().any(|(n, p)| v4_in(addr, *n, *p));
    IpClass {
        loopback: v4_in(addr, [127, 0, 0, 0], 8),
        private,
        link_local: v4_in(addr, [169, 254, 0, 0], 16),
        reserved: v4_in(addr, [240, 0, 0, 0], 4),
    }
}

/// Classify an address exactly as CPython's `ipaddress` does (3.13+ tables,
/// IPv4-mapped addresses delegating to their IPv4 address), plus NAT64.
pub fn classify_ip(addr: IpAddr) -> IpClass {
    match addr {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return classify_v4(mapped);
            }
            // NAT64 well-known prefix: judge the embedded IPv4 address, so a
            // DNS answer cannot reach loopback through a translator.
            if v6_in(v6, [0x64, 0xff9b, 0, 0, 0, 0, 0, 0], 96) {
                let o = v6.octets();
                return classify_v4(Ipv4Addr::new(o[12], o[13], o[14], o[15]));
            }
            const PRIVATE: &[([u16; 8], u32)] = &[
                ([0, 0, 0, 0, 0, 0, 0, 1], 128),
                ([0, 0, 0, 0, 0, 0, 0, 0], 128),
                ([0, 0, 0, 0, 0, 0xffff, 0, 0], 96),
                ([0x64, 0xff9b, 1, 0, 0, 0, 0, 0], 48),
                ([0x100, 0, 0, 0, 0, 0, 0, 0], 64),
                ([0x2001, 0, 0, 0, 0, 0, 0, 0], 23),
                ([0x2001, 0xdb8, 0, 0, 0, 0, 0, 0], 32),
                ([0x2002, 0, 0, 0, 0, 0, 0, 0], 16),
                ([0x3fff, 0, 0, 0, 0, 0, 0, 0], 20),
                ([0xfc00, 0, 0, 0, 0, 0, 0, 0], 7),
                ([0xfe80, 0, 0, 0, 0, 0, 0, 0], 10),
            ];
            const PRIVATE_EXCEPTIONS: &[([u16; 8], u32)] = &[
                ([0x2001, 1, 0, 0, 0, 0, 0, 1], 128),
                ([0x2001, 1, 0, 0, 0, 0, 0, 2], 128),
                ([0x2001, 3, 0, 0, 0, 0, 0, 0], 32),
                ([0x2001, 4, 0x112, 0, 0, 0, 0, 0], 48),
                ([0x2001, 0x20, 0, 0, 0, 0, 0, 0], 28),
                ([0x2001, 0x30, 0, 0, 0, 0, 0, 0], 28),
            ];
            const RESERVED: &[([u16; 8], u32)] = &[
                ([0, 0, 0, 0, 0, 0, 0, 0], 8),
                ([0x100, 0, 0, 0, 0, 0, 0, 0], 8),
                ([0x200, 0, 0, 0, 0, 0, 0, 0], 7),
                ([0x400, 0, 0, 0, 0, 0, 0, 0], 6),
                ([0x800, 0, 0, 0, 0, 0, 0, 0], 5),
                ([0x1000, 0, 0, 0, 0, 0, 0, 0], 4),
                ([0x4000, 0, 0, 0, 0, 0, 0, 0], 3),
                ([0x6000, 0, 0, 0, 0, 0, 0, 0], 3),
                ([0x8000, 0, 0, 0, 0, 0, 0, 0], 3),
                ([0xa000, 0, 0, 0, 0, 0, 0, 0], 3),
                ([0xc000, 0, 0, 0, 0, 0, 0, 0], 3),
                ([0xe000, 0, 0, 0, 0, 0, 0, 0], 4),
                ([0xf000, 0, 0, 0, 0, 0, 0, 0], 5),
                ([0xf800, 0, 0, 0, 0, 0, 0, 0], 6),
                ([0xfe00, 0, 0, 0, 0, 0, 0, 0], 9),
            ];
            let private = PRIVATE.iter().any(|(n, p)| v6_in(v6, *n, *p))
                && !PRIVATE_EXCEPTIONS.iter().any(|(n, p)| v6_in(v6, *n, *p));
            IpClass {
                loopback: v6 == Ipv6Addr::LOCALHOST,
                private,
                link_local: v6_in(v6, [0xfe80, 0, 0, 0, 0, 0, 0, 0], 10),
                reserved: RESERVED.iter().any(|(n, p)| v6_in(v6, *n, *p)),
            }
        }
    }
}

fn control_chars(value: &str) -> bool {
    value.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7f)
}

fn sensitive_field_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:password|passwd|passcode|secret|token|api[_-]?key|authorization|cookie|credit[_-]?card|card[_-]?number|cvv|ssn)",
        )
        .expect("static regex")
    })
}

fn scheme_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^([A-Za-z][A-Za-z0-9+.\-]*):").expect("static regex"))
}

/// The authority section of a special-scheme URL as WHATWG reads it: after
/// `scheme:` and any run of `/` or `\`, up to the next `/`, `\`, `?` or `#`.
fn raw_authority(trimmed: &str) -> &str {
    let after_scheme = match trimmed.find(':') {
        Some(i) => &trimmed[i + 1..],
        None => return "",
    };
    let after_slashes = after_scheme.trim_start_matches(['/', '\\']);
    match after_slashes.find(['/', '\\', '?', '#']) {
        Some(i) => &after_slashes[..i],
        None => after_slashes,
    }
}

/// Python `str.isprintable()` for one character, as far as `validate_key`
/// needs it: ASCII space is printable; controls, separators and format
/// characters are not.
fn is_printable_char(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    if c.is_control() || c.is_whitespace() {
        return false;
    }
    !matches!(
        c as u32,
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x180E | 0x200B..=0x200F
            | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB
            | 0xD800..=0xDFFF | 0xE000..=0xF8FF
    )
}

/// The host string a URL is matched on, as Chromium will use it.
fn url_host(url: &Url) -> Option<String> {
    match url.host()? {
        Host::Domain(d) => {
            let d = d.trim_end_matches('.');
            if d.is_empty() {
                None
            } else {
                Some(d.to_string())
            }
        }
        Host::Ipv4(ip) => Some(ip.to_string()),
        Host::Ipv6(ip) => Some(ip.to_string()),
    }
}

/// Fail-closed policy for browser destinations and bounded inputs
/// (agenthicc `BrowserPolicy`).
#[derive(Clone)]
pub struct BrowserPolicy {
    allowed_domains: Vec<String>,
    origins: Vec<AllowedOrigin>,
    allowed_ports: AllowedPorts,
    allow_loopback: bool,
    allow_private_addresses: bool,
    allow_all_domains: bool,
    /// Maximum URL length, in characters.
    pub max_url_chars: usize,
    /// Maximum selector length, in characters.
    pub max_selector_chars: usize,
    /// Maximum form-value length, in characters.
    pub max_value_chars: usize,
    /// Maximum wait-value length, in characters.
    pub max_wait_value_chars: usize,
    resolver: Resolver,
}

impl fmt::Debug for BrowserPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserPolicy")
            .field("allowed_domains", &self.allowed_domains)
            .field("allowed_ports", &self.allowed_ports)
            .field("allow_loopback", &self.allow_loopback)
            .field("allow_private_addresses", &self.allow_private_addresses)
            .field("allow_all_domains", &self.allow_all_domains)
            .finish_non_exhaustive()
    }
}

impl BrowserPolicy {
    /// Build a policy. A malformed allow-list entry is an error: the caller
    /// (the session manager) turns it into "browser unavailable" rather than
    /// letting the rest of xaft fail to start, as agenthicc does.
    pub fn new(
        allowed_domains: &[String],
        allowed_ports: AllowedPorts,
        allow_loopback: bool,
        allow_private_addresses: bool,
        allow_all_domains: bool,
    ) -> Result<Self, PolicyConfigError> {
        if let AllowedPorts::Only(set) = &allowed_ports {
            if set.is_empty() || set.contains(&0) {
                return Err(PolicyConfigError(
                    "CloakBrowser allowed_ports contains an invalid port".into(),
                ));
            }
        }
        let normalized: BTreeSet<String> = allowed_domains
            .iter()
            .map(|d| d.trim().to_lowercase())
            .filter(|d| !d.is_empty())
            .collect();
        let allowed_domains: Vec<String> = normalized.into_iter().collect();
        let origins = allowed_domains
            .iter()
            .map(|d| parse_allowed_origin(d))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            allowed_domains,
            origins,
            allowed_ports,
            allow_loopback,
            allow_private_addresses,
            allow_all_domains,
            max_url_chars: 4096,
            max_selector_chars: 512,
            max_value_chars: 4000,
            max_wait_value_chars: 512,
            resolver: system_resolver(),
        })
    }

    /// agenthicc's CloakBrowser policy: with `allow_all_domains` every port,
    /// loopback and private addresses are reachable; otherwise only ports
    /// 80/443 of the listed origins, and never loopback or private addresses.
    pub fn cloakbrowser(
        allowed_domains: &[String],
        allow_all_domains: bool,
    ) -> Result<Self, PolicyConfigError> {
        Self::new(
            allowed_domains,
            if allow_all_domains {
                AllowedPorts::All
            } else {
                AllowedPorts::web_defaults()
            },
            allow_all_domains,
            allow_all_domains,
            allow_all_domains,
        )
    }

    /// agenthicc's Playwright policy: every port and loopback are always
    /// reachable (local dev and preview servers); private addresses only with
    /// `allow_all_domains`.
    pub fn playwright(
        allowed_domains: &[String],
        allow_all_domains: bool,
    ) -> Result<Self, PolicyConfigError> {
        Self::new(
            allowed_domains,
            AllowedPorts::All,
            true,
            allow_all_domains,
            allow_all_domains,
        )
    }

    /// The deny-everything policy agenthicc substitutes for an invalid allow-list.
    pub fn deny_all() -> Self {
        Self::new(
            &["invalid.agenthicc.invalid".to_string()],
            AllowedPorts::web_defaults(),
            false,
            false,
            false,
        )
        .expect("static deny-all policy")
    }

    /// Replace the resolver (tests use a fixed table).
    pub fn with_resolver(mut self, resolver: Resolver) -> Self {
        self.resolver = resolver;
        self
    }

    /// The normalised allow-list entries.
    pub fn allowed_domains(&self) -> &[String] {
        &self.allowed_domains
    }

    /// Whether every HTTP(S) host is admitted.
    pub fn allow_all_domains(&self) -> bool {
        self.allow_all_domains
    }

    /// Whether loopback destinations are admitted.
    pub fn allow_loopback(&self) -> bool {
        self.allow_loopback
    }

    /// Whether private, link-local and reserved destinations are admitted.
    pub fn allow_private_addresses(&self) -> bool {
        self.allow_private_addresses
    }

    /// The port rule for non-exact-origin entries.
    pub fn allowed_ports(&self) -> &AllowedPorts {
        &self.allowed_ports
    }

    /// Whether sub-resource requests need per-request checks.
    ///
    /// With `allow_all_domains` the only things the per-request guard could
    /// still refuse are non-HTTP(S) schemes and malformed URLs, which
    /// Chromium never sends as sub-resource requests, so the engine skips the
    /// interception round-trip. Navigation targets and the final URL of every
    /// navigation are still checked in both modes.
    pub fn needs_request_interception(&self) -> bool {
        !self.allow_all_domains
    }

    /// agenthicc `_parse_url`: bounds, characters, scheme, credentials, fragment.
    fn parse_url(&self, url: &str) -> Result<Url, BrowserToolError> {
        let trimmed = url.trim();
        if trimmed.is_empty() || url.chars().count() > self.max_url_chars {
            return Err(BrowserToolError::new(
                BrowserErrorKind::InvalidInput,
                "URL is empty or too long.",
            ));
        }
        if control_chars(url) {
            return Err(BrowserToolError::new(
                BrowserErrorKind::InvalidInput,
                "URL contains control characters.",
            ));
        }
        let not_http = || {
            BrowserToolError::new(
                BrowserErrorKind::PolicyDenied,
                "Only approved http(s) destinations are allowed.",
            )
        };
        let scheme = scheme_regex()
            .captures(trimmed)
            .map(|c| c[1].to_ascii_lowercase())
            .ok_or_else(not_http)?;
        if scheme != "http" && scheme != "https" {
            return Err(not_http());
        }
        let parsed = Url::parse(trimmed).map_err(|_| {
            BrowserToolError::new(BrowserErrorKind::InvalidInput, "URL is malformed.")
        })?;
        if url_host(&parsed).is_none() {
            return Err(not_http());
        }
        if raw_authority(trimmed).contains('@')
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(BrowserToolError::new(
                BrowserErrorKind::PolicyDenied,
                "URL credentials are not allowed.",
            ));
        }
        if parsed.fragment().is_some_and(|f| !f.is_empty()) {
            return Err(BrowserToolError::new(
                BrowserErrorKind::PolicyDenied,
                "URL fragments are not allowed.",
            ));
        }
        Ok(parsed)
    }

    /// The address rules. A loopback address is judged by the loopback rule
    /// alone: agenthicc's `(private and not loopback) or link_local or
    /// reserved` meant to exclude loopback from the private rule, but CPython
    /// also files `::1` under `::/8` *reserved*, so on a host whose resolver
    /// returns `::1` for `localhost` agenthicc refuses Playwright's own
    /// always-allowed loopback. The `not loopback` exclusion states the
    /// intent; this applies it to every clause.
    fn check_address(&self, address: IpAddr) -> Result<(), BrowserToolError> {
        let class = classify_ip(address);
        if class.loopback {
            if !self.allow_loopback {
                return Err(BrowserToolError::new(
                    BrowserErrorKind::PolicyDenied,
                    "Loopback destinations are blocked.",
                ));
            }
            return Ok(());
        }
        if (class.private || class.link_local || class.reserved) && !self.allow_private_addresses {
            return Err(BrowserToolError::new(
                BrowserErrorKind::PolicyDenied,
                "Private destinations are blocked.",
            ));
        }
        Ok(())
    }

    /// agenthicc `_check_origin`: allow-list and port rules, then the
    /// literal-address checks.
    fn check_origin(&self, url: &Url, host: &str) -> Result<(), BrowserToolError> {
        let scheme = url.scheme();
        let effective_port =
            url.port_or_known_default()
                .unwrap_or(if scheme == "https" { 443 } else { 80 });
        if self.allow_all_domains {
            if !self.allowed_ports.contains(effective_port) {
                return Err(BrowserToolError::new(
                    BrowserErrorKind::PolicyDenied,
                    "URL port is not allowed.",
                ));
            }
        } else if !self
            .origins
            .iter()
            .any(|o| o.matches(scheme, host, effective_port, &self.allowed_ports))
        {
            return Err(BrowserToolError::new(
                BrowserErrorKind::PolicyDenied,
                "Destination is not allow-listed.",
            ));
        }
        if let Ok(address) = host.parse::<IpAddr>() {
            self.check_address(address)?;
        }
        Ok(())
    }

    /// Resolve a host, answering `localhost` the way Chromium does.
    async fn resolve(&self, host: &str) -> std::io::Result<Vec<IpAddr>> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(vec![ip]);
        }
        if host == "localhost" || host.ends_with(".localhost") {
            return Ok(vec![
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                IpAddr::V6(Ipv6Addr::LOCALHOST),
            ]);
        }
        (self.resolver)(host.to_string()).await
    }

    /// Validate a destination and return its normalised form.
    ///
    /// DNS is resolved on every call, so an allow-listed hostname later
    /// rebound to a private address is refused (agenthicc behaviour).
    pub async fn validate_url(&self, url: &str) -> Result<String, BrowserToolError> {
        let parsed = self.parse_url(url)?;
        let host = url_host(&parsed).expect("parse_url guarantees a host");
        self.check_origin(&parsed, &host)?;
        let addresses = self.resolve(&host).await.map_err(|_| {
            BrowserToolError::new(
                BrowserErrorKind::Network,
                "Destination could not be resolved.",
            )
        })?;
        if addresses.is_empty() {
            return Err(BrowserToolError::new(
                BrowserErrorKind::Network,
                "Destination could not be resolved.",
            ));
        }
        for address in addresses {
            self.check_address(address)?;
        }
        let mut normalized = parsed;
        normalized.set_fragment(None);
        Ok(normalized.to_string())
    }

    /// agenthicc `validate_selector`.
    pub fn validate_selector(&self, selector: &str) -> Result<String, BrowserToolError> {
        if selector.trim().is_empty() || selector.chars().count() > self.max_selector_chars {
            return Err(BrowserToolError::new(
                BrowserErrorKind::InvalidInput,
                "Selector is empty or too long.",
            ));
        }
        if control_chars(selector) {
            return Err(BrowserToolError::new(
                BrowserErrorKind::InvalidInput,
                "Selector contains control characters.",
            ));
        }
        Ok(selector.trim().to_string())
    }

    /// agenthicc `validate_value`: the selector, the value bound, and the
    /// sensitive-field refusal.
    pub fn validate_value(
        &self,
        selector: &str,
        value: &str,
    ) -> Result<(String, String), BrowserToolError> {
        let safe_selector = self.validate_selector(selector)?;
        if value.chars().count() > self.max_value_chars {
            return Err(BrowserToolError::new(
                BrowserErrorKind::InvalidInput,
                "Form value is empty or too long.",
            ));
        }
        if Self::is_sensitive_target(&safe_selector) {
            return Err(sensitive_field_refusal());
        }
        Ok((safe_selector, value.to_string()))
    }

    /// agenthicc `validate_key`: a named safe key or one printable character.
    pub fn validate_key(&self, key: &str) -> Result<String, BrowserToolError> {
        if key.chars().count() > 32 || key.trim().is_empty() {
            return Err(BrowserToolError::new(
                BrowserErrorKind::InvalidInput,
                "Keyboard key is invalid.",
            ));
        }
        let normalized = key.trim();
        let lower = normalized.to_lowercase();
        let mut chars = normalized.chars();
        let single_printable =
            matches!((chars.next(), chars.next()), (Some(c), None) if is_printable_char(c));
        if !SAFE_KEYS.contains(&lower.as_str()) && !single_printable {
            return Err(BrowserToolError::new(
                BrowserErrorKind::PolicyDenied,
                "Keyboard key is not allow-listed.",
            ));
        }
        Ok(normalized.to_string())
    }

    /// agenthicc `validate_wait`.
    pub fn validate_wait(
        &self,
        condition: &str,
        value: &str,
    ) -> Result<(String, String), BrowserToolError> {
        if !WAIT_CONDITIONS.contains(&condition) {
            return Err(BrowserToolError::new(
                BrowserErrorKind::InvalidInput,
                "Wait condition is invalid.",
            ));
        }
        if value.trim().is_empty() || value.chars().count() > self.max_wait_value_chars {
            return Err(BrowserToolError::new(
                BrowserErrorKind::InvalidInput,
                "Wait value is empty or too long.",
            ));
        }
        if condition == "selector" {
            return Ok((condition.to_string(), self.validate_selector(value)?));
        }
        Ok((condition.to_string(), value.trim().to_string()))
    }

    /// agenthicc `is_sensitive_target`: does a selector name a credential,
    /// token or payment field?
    pub fn is_sensitive_target(selector: &str) -> bool {
        sensitive_field_regex().is_match(selector)
    }
}

/// The refusal for a sensitive form field.
pub fn sensitive_field_refusal() -> BrowserToolError {
    BrowserToolError::new(
        BrowserErrorKind::PolicyDenied,
        "Sensitive form fields cannot be filled by the browser agent.",
    )
}

/// A URL without credentials, query or fragment (agenthicc `redact_url`).
pub fn redact_url(value: &str) -> String {
    let Ok(parsed) = Url::parse(value) else {
        return "<invalid-url>".to_string();
    };
    let Some(host) = parsed.host_str() else {
        return "<invalid-url>".to_string();
    };
    if host.is_empty() {
        return "<invalid-url>".to_string();
    }
    let netloc = match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    let path = if parsed.path().is_empty() {
        "/"
    } else {
        parsed.path()
    };
    format!("{}://{}{}", parsed.scheme(), netloc, path)
}

/// Redact an absolute or relative link without exposing query secrets
/// (agenthicc `redact_link`). A relative or non-hierarchical link keeps only
/// its path, as `urlsplit(value).path` does.
pub fn redact_link(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    match Url::parse(value) {
        Ok(parsed) if parsed.host_str().is_some_and(|h| !h.is_empty()) => redact_url(value),
        Ok(_) | Err(url::ParseError::RelativeUrlWithoutBase) => {
            python_split_path(value).unwrap_or_else(|| "<invalid-link>".to_string())
        }
        Err(_) => "<invalid-link>".to_string(),
    }
}

/// The `path` component of Python's `urlsplit(value)`, or `None` where Python
/// would raise (an unbalanced IPv6 bracket in the netloc).
fn python_split_path(value: &str) -> Option<String> {
    let mut rest = value;
    if let Some(c) = scheme_regex().captures(rest) {
        rest = &rest[c[0].len()..];
    }
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        let netloc = &after[..end];
        if netloc.contains('[') != netloc.contains(']') {
            return None;
        }
        rest = &after[end..];
    }
    let end = rest.find(['?', '#']).unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A resolver answering from a fixed table (agenthicc tests' `resolve`).
    pub(crate) fn fixed_resolver(table: &[(&str, &str)]) -> Resolver {
        let map: HashMap<String, Vec<IpAddr>> = table
            .iter()
            .map(|(h, a)| (h.to_string(), vec![a.parse().unwrap()]))
            .collect();
        Arc::new(move |host: String| -> ResolveFuture {
            let answer = map.get(&host).cloned();
            Box::pin(async move {
                answer.ok_or_else(|| std::io::Error::other("no such host in the test table"))
            })
        })
    }

    pub(crate) fn public_resolver() -> Resolver {
        Arc::new(|_host: String| -> ResolveFuture {
            Box::pin(async { Ok(vec!["93.184.216.34".parse().unwrap()]) })
        })
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    fn policy(domains: &[&str]) -> BrowserPolicy {
        BrowserPolicy::cloakbrowser(&strings(domains), false)
            .unwrap()
            .with_resolver(public_resolver())
    }

    async fn denied(policy: &BrowserPolicy, url: &str) -> BrowserToolError {
        policy
            .validate_url(url)
            .await
            .expect_err(&format!("{url} should be refused"))
    }

    #[tokio::test]
    async fn an_empty_restricted_policy_denies_every_destination() {
        let p = policy(&[]);
        let err = denied(&p, "https://example.com/").await;
        assert_eq!(err.kind, BrowserErrorKind::PolicyDenied);
        assert_eq!(err.message, "Destination is not allow-listed.");
    }

    #[tokio::test]
    async fn unsafe_destinations_are_refused_with_agenthiccs_sentences() {
        let p = policy(&["example.com"]);
        let cases = [
            (
                "https://not-example.test/",
                BrowserErrorKind::PolicyDenied,
                "Destination is not allow-listed.",
            ),
            (
                "file:///etc/passwd",
                BrowserErrorKind::PolicyDenied,
                "Only approved http(s) destinations are allowed.",
            ),
            (
                "ftp://example.com/",
                BrowserErrorKind::PolicyDenied,
                "Only approved http(s) destinations are allowed.",
            ),
            (
                "not a url",
                BrowserErrorKind::PolicyDenied,
                "Only approved http(s) destinations are allowed.",
            ),
            (
                "https://example.com/#fragment",
                BrowserErrorKind::PolicyDenied,
                "URL fragments are not allowed.",
            ),
            (
                "https://user:pass@example.com/",
                BrowserErrorKind::PolicyDenied,
                "URL credentials are not allowed.",
            ),
            (
                "https://@example.com/",
                BrowserErrorKind::PolicyDenied,
                "URL credentials are not allowed.",
            ),
            (
                "",
                BrowserErrorKind::InvalidInput,
                "URL is empty or too long.",
            ),
            (
                "https://example.com/\n",
                BrowserErrorKind::InvalidInput,
                "URL contains control characters.",
            ),
            (
                "http://[::1",
                BrowserErrorKind::InvalidInput,
                "URL is malformed.",
            ),
            (
                "https://example.com:99999/",
                BrowserErrorKind::InvalidInput,
                "URL is malformed.",
            ),
        ];
        for (url, kind, message) in cases {
            let err = denied(&p, url).await;
            assert_eq!((err.kind, err.message.as_str()), (kind, message), "{url}");
        }
        let long = format!("https://example.com/{}", "x".repeat(4100));
        assert_eq!(denied(&p, &long).await.message, "URL is empty or too long.");
    }

    /// The WHATWG reading is the one Chromium uses: a backslash ends the
    /// authority, so the host is the part *before* it. agenthicc's splitter
    /// reads this as `example.com` and would approve a visit to `evil.test`.
    #[tokio::test]
    async fn the_policy_reads_the_host_chromium_will_visit() {
        let p = policy(&["example.com"]);
        let err = denied(&p, "http://evil.test\\@example.com/").await;
        assert_eq!(err.kind, BrowserErrorKind::PolicyDenied);
        assert!(
            err.message == "Destination is not allow-listed."
                || err.message == "URL credentials are not allowed.",
            "{}",
            err.message
        );
    }

    #[tokio::test]
    async fn exact_and_wildcard_cors_style_origins() {
        let exact = policy(&["https://example.com"]);
        assert_eq!(
            exact
                .validate_url("https://example.com/path")
                .await
                .unwrap(),
            "https://example.com/path"
        );
        exact
            .validate_url("https://example.com:443/path")
            .await
            .unwrap();
        denied(&exact, "http://example.com/path").await;
        denied(&exact, "https://sub.example.com/path").await;

        let custom_port = policy(&["https://example.com:8443"]);
        custom_port
            .validate_url("https://example.com:8443/path")
            .await
            .unwrap();
        denied(&custom_port, "https://example.com/path").await;

        let wildcard = policy(&["https://*.example.com"]);
        wildcard
            .validate_url("https://sub.example.com/path")
            .await
            .unwrap();
        denied(&wildcard, "https://example.com/path").await;
    }

    #[tokio::test]
    async fn bare_hosts_admit_subdomains_but_not_suffix_lookalikes() {
        let p = policy(&["example.com"]);
        p.validate_url("https://api.example.com/").await.unwrap();
        p.validate_url("http://example.com/").await.unwrap();
        denied(&p, "https://notexample.com/").await;
        // Restricted CloakBrowser ports are {80, 443}.
        let err = denied(&p, "https://example.com:8443/").await;
        assert_eq!(err.message, "Destination is not allow-listed.");
        let wildcard_only = policy(&["*.example.com"]);
        wildcard_only
            .validate_url("https://a.example.com/")
            .await
            .unwrap();
        denied(&wildcard_only, "https://example.com/").await;
    }

    #[tokio::test]
    async fn allow_list_hosts_are_idna_normalised_like_chromium() {
        let p = policy(&["bücher.example"]);
        p.validate_url("https://xn--bcher-kva.example/")
            .await
            .unwrap();
        p.validate_url("https://BÜCHER.example/").await.unwrap();
    }

    #[test]
    fn unsafe_allow_list_entries_are_rejected() {
        for value in [
            "*",
            "null",
            "https://user:secret@example.com",
            "https://example.com/path",
            "https://example.com?token=secret",
            "https://example.com/#fragment",
            "https://example.com:bad",
            "https://*.example.*",
            "ftp://example.com",
            "example.com/path",
            "a b.example",
        ] {
            assert!(
                BrowserPolicy::cloakbrowser(&strings(&[value]), false).is_err(),
                "{value} should be rejected"
            );
        }
        assert!(
            BrowserPolicy::new(
                &strings(&["example.com"]),
                AllowedPorts::Only([0].into_iter().collect()),
                false,
                false,
                false
            )
            .is_err()
        );
    }

    #[test]
    fn allow_list_entries_are_normalised_deduplicated_and_sorted() {
        let p = BrowserPolicy::cloakbrowser(
            &strings(&[
                " HTTPS://Example.com ",
                "https://*.example.org:8443",
                "https://example.com",
                "",
            ]),
            false,
        )
        .unwrap();
        assert_eq!(
            p.allowed_domains(),
            &[
                "https://*.example.org:8443".to_string(),
                "https://example.com".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn allow_all_domains_enforces_only_the_port_rule() {
        let p = BrowserPolicy::new(
            &[],
            AllowedPorts::Only([443].into_iter().collect()),
            true,
            true,
            true,
        )
        .unwrap()
        .with_resolver(public_resolver());
        let err = denied(&p, "http://example.com/").await;
        assert_eq!(err.message, "URL port is not allowed.");
        p.validate_url("https://anything.example/").await.unwrap();
    }

    /// agenthicc `test_default_is_enabled_with_liberal_browser_policy`.
    #[tokio::test]
    async fn the_liberal_default_admits_loopback_private_and_any_port() {
        let p = BrowserPolicy::cloakbrowser(&[], true).unwrap();
        assert!(p.allow_loopback() && p.allow_private_addresses() && p.allow_all_domains());
        assert_eq!(p.allowed_ports(), &AllowedPorts::All);
        assert_eq!(
            p.validate_url("http://127.0.0.1:3000/").await.unwrap(),
            "http://127.0.0.1:3000/"
        );
        p.validate_url("http://10.0.0.1:8080/").await.unwrap();
        assert!(!p.needs_request_interception());
    }

    #[tokio::test]
    async fn loopback_and_private_destinations_follow_the_family_rules() {
        let resolver = fixed_resolver(&[("internal.example", "192.168.1.4")]);
        let restricted =
            BrowserPolicy::cloakbrowser(&strings(&["localhost", "internal.example"]), false)
                .unwrap()
                .with_resolver(resolver.clone());
        assert_eq!(
            denied(&restricted, "http://localhost/").await.message,
            "Loopback destinations are blocked."
        );
        assert_eq!(
            denied(&restricted, "http://internal.example/")
                .await
                .message,
            "Private destinations are blocked."
        );
        assert!(restricted.needs_request_interception());

        // Playwright always admits loopback and every port (preview servers).
        let pw = BrowserPolicy::playwright(&strings(&["localhost"]), false).unwrap();
        pw.validate_url("http://localhost:3000/").await.unwrap();
        let pw_private = BrowserPolicy::playwright(&strings(&["internal.example"]), false)
            .unwrap()
            .with_resolver(resolver);
        assert_eq!(
            denied(&pw_private, "http://internal.example/")
                .await
                .message,
            "Private destinations are blocked."
        );
    }

    #[tokio::test]
    async fn literal_addresses_are_classified_before_resolution() {
        let p = BrowserPolicy::cloakbrowser(&strings(&["127.0.0.1", "10.0.0.1", "[::1]"]), false)
            .unwrap();
        assert_eq!(
            denied(&p, "http://127.0.0.1/").await.message,
            "Loopback destinations are blocked."
        );
        assert_eq!(
            denied(&p, "http://10.0.0.1/").await.message,
            "Private destinations are blocked."
        );
        assert_eq!(
            denied(&p, "http://[::1]/").await.message,
            "Loopback destinations are blocked."
        );
    }

    /// WHATWG parses the integer and hex IPv4 forms Chromium accepts, so a
    /// numeric spelling of 127.0.0.1 is still loopback.
    #[tokio::test]
    async fn numeric_ipv4_spellings_are_still_loopback() {
        let p = BrowserPolicy::cloakbrowser(&[], true).unwrap();
        let strict = BrowserPolicy::new(&[], AllowedPorts::All, false, false, true).unwrap();
        for url in ["http://2130706433/", "http://0x7f.1/", "http://127.1/"] {
            p.validate_url(url).await.unwrap();
            assert_eq!(
                denied(&strict, url).await.message,
                "Loopback destinations are blocked.",
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn resolution_failures_are_network_errors() {
        let unavailable: Resolver = Arc::new(|_h: String| -> ResolveFuture {
            Box::pin(async { Err(std::io::Error::other("dns down")) })
        });
        let p = policy(&["example.com"]).with_resolver(unavailable);
        let err = denied(&p, "https://example.com/").await;
        assert_eq!(err.kind, BrowserErrorKind::Network);
        assert_eq!(err.message, "Destination could not be resolved.");

        let empty: Resolver =
            Arc::new(|_h: String| -> ResolveFuture { Box::pin(async { Ok(Vec::new()) }) });
        let p = policy(&["example.com"]).with_resolver(empty);
        assert_eq!(
            denied(&p, "https://example.com/").await.kind,
            BrowserErrorKind::Network
        );
    }

    /// DNS rebinding: an allow-listed name that resolves privately is refused.
    #[tokio::test]
    async fn rebinding_to_a_private_address_is_refused() {
        let p =
            policy(&["example.com"]).with_resolver(fixed_resolver(&[("example.com", "10.1.2.3")]));
        assert_eq!(
            denied(&p, "https://example.com/").await.message,
            "Private destinations are blocked."
        );
        let nat64 = policy(&["example.com"])
            .with_resolver(fixed_resolver(&[("example.com", "64:ff9b::7f00:1")]));
        assert_eq!(
            denied(&nat64, "https://example.com/").await.message,
            "Loopback destinations are blocked."
        );
    }

    #[tokio::test]
    async fn localhost_names_resolve_to_loopback_without_dns() {
        // A resolver that fails: had it been consulted, the refusal would be a
        // `network` error, so a loopback refusal proves it was not.
        let failing: Resolver = Arc::new(|_h: String| -> ResolveFuture {
            Box::pin(async { Err(std::io::Error::other("must not resolve")) })
        });
        let strict = BrowserPolicy::new(&[], AllowedPorts::All, false, false, true)
            .unwrap()
            .with_resolver(failing);
        for url in ["http://localhost/", "http://app.localhost:3000/"] {
            assert_eq!(
                denied(&strict, url).await.message,
                "Loopback destinations are blocked."
            );
        }
    }

    /// CPython `ipaddress` parity for the ranges the policy depends on.
    #[test]
    fn ip_classification_matches_cpython() {
        let cases: &[(&str, IpClass)] = &[
            (
                "127.0.0.1",
                IpClass {
                    loopback: true,
                    private: true,
                    link_local: false,
                    reserved: false,
                },
            ),
            (
                "10.0.0.1",
                IpClass {
                    loopback: false,
                    private: true,
                    link_local: false,
                    reserved: false,
                },
            ),
            (
                "172.31.255.255",
                IpClass {
                    loopback: false,
                    private: true,
                    link_local: false,
                    reserved: false,
                },
            ),
            ("172.32.0.1", IpClass::default()),
            (
                "192.168.1.4",
                IpClass {
                    loopback: false,
                    private: true,
                    link_local: false,
                    reserved: false,
                },
            ),
            (
                "169.254.1.1",
                IpClass {
                    loopback: false,
                    private: true,
                    link_local: true,
                    reserved: false,
                },
            ),
            ("100.64.0.1", IpClass::default()),
            (
                "0.0.0.0",
                IpClass {
                    loopback: false,
                    private: true,
                    link_local: false,
                    reserved: false,
                },
            ),
            (
                "240.0.0.1",
                IpClass {
                    loopback: false,
                    private: true,
                    link_local: false,
                    reserved: true,
                },
            ),
            ("192.0.0.9", IpClass::default()),
            ("93.184.216.34", IpClass::default()),
            (
                "::1",
                IpClass {
                    loopback: true,
                    private: true,
                    link_local: false,
                    reserved: true,
                },
            ),
            (
                "::",
                IpClass {
                    loopback: false,
                    private: true,
                    link_local: false,
                    reserved: true,
                },
            ),
            (
                "::ffff:127.0.0.1",
                IpClass {
                    loopback: true,
                    private: true,
                    link_local: false,
                    reserved: false,
                },
            ),
            (
                "fe80::1",
                IpClass {
                    loopback: false,
                    private: true,
                    link_local: true,
                    reserved: false,
                },
            ),
            (
                "fc00::1",
                IpClass {
                    loopback: false,
                    private: true,
                    link_local: false,
                    reserved: false,
                },
            ),
            (
                "2001:db8::1",
                IpClass {
                    loopback: false,
                    private: true,
                    link_local: false,
                    reserved: false,
                },
            ),
            ("2606:4700::1111", IpClass::default()),
        ];
        for (addr, expected) in cases {
            assert_eq!(classify_ip(addr.parse().unwrap()), *expected, "{addr}");
        }
    }

    #[test]
    fn redaction_drops_credentials_query_and_fragment() {
        assert_eq!(
            redact_url("https://Example.com:8443/a?token=secret#fragment"),
            "https://example.com:8443/a"
        );
        assert_eq!(
            redact_url("https://u:p@example.com/a?b"),
            "https://example.com/a"
        );
        assert_eq!(redact_url("relative/path"), "<invalid-url>");
        assert_eq!(redact_url("https://[bad"), "<invalid-url>");
        assert_eq!(redact_url("mailto:someone@example.com"), "<invalid-url>");
        assert_eq!(redact_link(""), "");
        assert_eq!(redact_link("/next?token=secret#section"), "/next");
        assert_eq!(
            redact_link("https://example.com/next?token=secret"),
            "https://example.com/next"
        );
        assert_eq!(redact_link("//cdn.example.com/x?y=1"), "/x");
        assert_eq!(redact_link("http://[bad"), "<invalid-link>");
        assert_eq!(redact_link("#section"), "");
    }

    #[test]
    fn input_validators_enforce_agenthiccs_bounds() {
        let mut p = policy(&["example.com"]);
        p.max_selector_chars = 10;
        assert_eq!(
            p.validate_selector(" ").unwrap_err().message,
            "Selector is empty or too long."
        );
        assert_eq!(
            p.validate_selector("x".repeat(11).as_str())
                .unwrap_err()
                .message,
            "Selector is empty or too long."
        );
        assert_eq!(
            p.validate_selector("button\u{0}").unwrap_err().message,
            "Selector contains control characters."
        );
        assert_eq!(p.validate_selector(" body ").unwrap(), "body");

        let p = policy(&["example.com"]);
        let err = p.validate_value("input", &"x".repeat(4001)).unwrap_err();
        assert_eq!(err.message, "Form value is empty or too long.");
        assert_eq!(
            p.validate_value("#q", "").unwrap(),
            ("#q".to_string(), String::new())
        );
        let err = p.validate_value("input[name=password]", "x").unwrap_err();
        assert_eq!(err.kind, BrowserErrorKind::PolicyDenied);
        assert_eq!(
            err.message,
            "Sensitive form fields cannot be filled by the browser agent."
        );

        assert_eq!(p.validate_key(" Enter ").unwrap(), "Enter");
        assert_eq!(p.validate_key("x").unwrap(), "x");
        assert_eq!(
            p.validate_key(" ").unwrap_err().message,
            "Keyboard key is invalid."
        );
        for (key, message) in [
            ("", "Keyboard key is invalid."),
            (&"x".repeat(33)[..], "Keyboard key is invalid."),
            ("F13", "Keyboard key is not allow-listed."),
            ("Control+A", "Keyboard key is not allow-listed."),
        ] {
            assert_eq!(p.validate_key(key).unwrap_err().message, message, "{key}");
        }

        assert_eq!(
            p.validate_wait("bad", "value").unwrap_err().message,
            "Wait condition is invalid."
        );
        assert_eq!(
            p.validate_wait("text", " ").unwrap_err().message,
            "Wait value is empty or too long."
        );
        assert_eq!(
            p.validate_wait("selector", " body ").unwrap(),
            ("selector".to_string(), "body".to_string())
        );
        assert_eq!(
            p.validate_wait("url", " https://example.com ").unwrap(),
            ("url".to_string(), "https://example.com".to_string())
        );
    }

    #[test]
    fn sensitive_targets_cover_credentials_tokens_and_payment_fields() {
        for selector in [
            "[name=credit-card]",
            "#password",
            "input[name=PASSWD]",
            "#api_key",
            "#apikey",
            ".card-number",
            "[name=cvv]",
            "#ssn",
            "#auth-token",
        ] {
            assert!(BrowserPolicy::is_sensitive_target(selector), "{selector}");
        }
        assert!(!BrowserPolicy::is_sensitive_target("#search"));
    }
}
