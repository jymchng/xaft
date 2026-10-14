//! Provider connection-profile extras: secret references, per-request options,
//! protocol selection and capability flags.
//!
//! Parity with agenthicc's `config.py` types `SecretReference`,
//! `RequestOptionSettings` and `ResolvedProviderProfile`. agenthicc's
//! `ProviderProfile` is [`ProviderConfig`](crate::types::ProviderConfig); this
//! module supplies the pieces that type was missing, plus the runtime
//! [`ResolvedProviderProfile`] that `ProviderConfig::resolve_profile` builds.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::types::{ProviderConfig, ProviderType};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Capability names agenthicc accepts in `providers.<name>.capabilities`.
///
/// agenthicc raises on an unknown name; [`ProviderCapabilities`] gets the same
/// behaviour from `#[serde(deny_unknown_fields)]`.
pub const CAPABILITY_NAMES: &[&str] = &[
    "tools",
    "streaming",
    "structured_output",
    "thinking",
    "vision",
    "embeddings",
];

/// Request keys xaft computes itself, and which a `provider` or `extra_body`
/// table may therefore not restate.
///
/// Restating one of these is how a config ends up sending `model` twice, or
/// sending an `extra_body` model that silently wins over the resolved one.
pub const CANONICAL_REQUEST_KEYS: &[&str] = &[
    "model",
    "messages",
    "stream",
    "max_tokens",
    "max_completion_tokens",
    "temperature",
    "top_p",
    "tools",
    "tool_choice",
    "stop",
    "stop_sequences",
    "thinking",
    "options",
    "system",
];

/// Keys accepted in `providers.<name>.client_options`.
pub const CLIENT_OPTION_KEYS: &[&str] = &[
    "follow_redirects",
    "http2",
    "verify",
    "trust_env",
    "max_redirects",
    "local_address",
];

/// Longest session identity that may be sent as a dynamic session header.
pub const MAX_SESSION_ID_BYTES: usize = 256;

/// Substrings that make an option key sensitive for redaction purposes.
///
/// Matches agenthicc's `_SENSITIVE_KEY_PARTS`.
const SENSITIVE_KEY_PARTS: &[&str] = &[
    "token",
    "secret",
    "password",
    "credential",
    "authorization",
    "api_key",
];

/// Characters RFC 9110 permits in a field name (the tchar set).
fn is_tchar(c: char) -> bool {
    c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c)
}

// ── SecretReference ───────────────────────────────────────────────────────────

/// A symbolic reference to a secret held in the process environment.
///
/// The value never enters the config: only the *name* of the variable does, so
/// a config file stays safe to print, diff and commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef {
    /// Environment variable holding the value.
    pub env: String,
}

impl SecretRef {
    /// Build a reference, rejecting names that are not `IDENT` shaped.
    pub fn new(env: &str, section: &str) -> Result<Self, ConfigError> {
        let name = env.trim();
        let mut chars = name.chars();
        let valid = match chars.next() {
            Some(first) if first.is_ascii_alphabetic() || first == '_' => {
                chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
            }
            _ => false,
        };
        if !valid {
            return Err(ConfigError::validation(
                section,
                "must be a valid environment variable name",
            ));
        }
        Ok(Self {
            env: name.to_string(),
        })
    }

    /// Look the variable up through an injected lookup.
    ///
    /// The indirection exists so a caller can resolve against a snapshot rather
    /// than the live process environment, and so tests need not mutate global
    /// state. An unset *or empty* variable is an error: agenthicc treats a
    /// blank credential as missing, which is what stops an empty header value
    /// from reaching the wire.
    pub fn resolve_with<F>(&self, lookup: F, section: &str) -> Result<String, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        match lookup(&self.env) {
            Some(value) if !value.is_empty() => Ok(value),
            _ => Err(ConfigError::validation(
                section,
                format!("missing secret environment variable '{}'", self.env),
            )),
        }
    }

    /// Look the variable up in the process environment.
    pub fn resolve(&self, section: &str) -> Result<String, ConfigError> {
        self.resolve_with(|name| std::env::var(name).ok(), section)
    }

    /// The only representation that may be printed: the variable *name*.
    pub fn redacted(&self) -> BTreeMap<&'static str, String> {
        BTreeMap::from([("env", self.env.clone())])
    }
}

// ── Header values ─────────────────────────────────────────────────────────────

/// A header value that is either a literal or an environment reference.
///
/// agenthicc spells this `str | SecretReference`; an untagged enum is the
/// closest Rust equivalent and keeps `{"env": "NAME"}` and `"NAME"` both
/// parseable from the same TOML key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HeaderValue {
    /// A literal value in the config file.
    Literal(String),
    /// A reference resolved just before the request goes out.
    Env(SecretRef),
}

impl HeaderValue {
    /// Parse one entry of a header table.
    ///
    /// A CR or LF in a literal is rejected here rather than at request time:
    /// it is the header-injection primitive, and by the time a request is built
    /// the offending config is several frames away.
    pub fn parse(value: &serde_json::Value, section: &str) -> Result<Self, ConfigError> {
        match value {
            serde_json::Value::String(text) => {
                validate_header_value(text, section)?;
                Ok(Self::Literal(text.clone()))
            }
            serde_json::Value::Object(map) if map.len() == 1 && map.contains_key("env") => {
                let name = map
                    .get("env")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| ConfigError::validation(section, "env must be a string"))?;
                Ok(Self::Env(SecretRef::new(name, &format!("{section}.env"))?))
            }
            _ => Err(ConfigError::validation(
                section,
                "must be a literal string or { env = \"NAME\" }",
            )),
        }
    }

    /// Resolve to the literal that goes on the wire.
    pub fn resolve_with<F>(&self, lookup: F, section: &str) -> Result<String, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        match self {
            Self::Literal(text) => {
                validate_header_value(text, section)?;
                Ok(text.clone())
            }
            Self::Env(secret) => {
                let resolved = secret.resolve_with(lookup, section)?;
                validate_header_value(&resolved, section)?;
                Ok(resolved)
            }
        }
    }

    /// The safe-to-print view of this value.
    pub fn redacted(&self) -> serde_json::Value {
        match self {
            Self::Literal(_) => serde_json::Value::String(REDACTED.into()),
            Self::Env(secret) => serde_json::to_value(secret.redacted()).unwrap_or_default(),
        }
    }
}

/// Placeholder substituted for any literal that might be a secret.
pub const REDACTED: &str = "<redacted>";

/// Reject CR/LF in a header value.
fn validate_header_value(value: &str, section: &str) -> Result<(), ConfigError> {
    if value.contains('\r') || value.contains('\n') {
        return Err(ConfigError::validation(
            section,
            "must not contain CR/LF characters",
        ));
    }
    Ok(())
}

/// Normalize and validate one header name.
pub fn validate_header_name(name: &str, section: &str) -> Result<String, ConfigError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || !trimmed.chars().all(is_tchar) {
        return Err(ConfigError::validation(
            section,
            "is not a valid HTTP header name",
        ));
    }
    Ok(trimmed.to_string())
}

// ── Client options ────────────────────────────────────────────────────────────

/// TLS verification: either a boolean switch or a CA bundle path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Verify {
    /// `verify = true|false`.
    Flag(bool),
    /// `verify = "/path/to/ca.pem"`.
    Path(String),
}

/// Transport-level knobs passed straight to the provider HTTP client.
///
/// Every field is optional so an unset option stays unset; an empty string is
/// not a way to spell "default", which would silently disable verification.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClientOptions {
    /// Follow HTTP redirects.
    pub follow_redirects: Option<bool>,
    /// Negotiate HTTP/2.
    pub http2: Option<bool>,
    /// TLS verification.
    pub verify: Option<Verify>,
    /// Honour `HTTP_PROXY` and friends.
    pub trust_env: Option<bool>,
    /// Redirect ceiling.
    pub max_redirects: Option<u32>,
    /// Local address to bind the socket to.
    pub local_address: Option<String>,
}

impl ClientOptions {
    /// Whether any option is set at all.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

// ── Protocol ──────────────────────────────────────────────────────────────────

/// Wire protocol spoken to the endpoint.
///
/// An empty string is *not* a protocol: agenthicc treats it as "unset" and
/// derives one from the provider, and [`ResolvedProviderProfile::protocol`]
/// reproduces that derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderProtocol {
    /// OpenAI-style `chat/completions`.
    #[serde(rename = "chat_completions")]
    ChatCompletions,
    /// Anthropic-style `messages`.
    #[serde(rename = "messages")]
    Messages,
    /// Ollama's native `/api/chat`.
    #[serde(rename = "ollama")]
    Ollama,
    /// OpenAI.
    #[serde(rename = "openai")]
    Openai,
    /// Any OpenAI-shaped endpoint.
    #[serde(rename = "openai-compatible")]
    OpenaiCompatible,
    /// OpenCode Go proxy.
    #[serde(rename = "opencode-go")]
    OpencodeGo,
    /// Anthropic.
    #[serde(rename = "anthropic")]
    Anthropic,
    /// LiteLLM proxy.
    #[serde(rename = "litellm")]
    Litellm,
}

impl ProviderProtocol {
    /// The exact wire string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::Messages => "messages",
            Self::Ollama => "ollama",
            Self::Openai => "openai",
            Self::OpenaiCompatible => "openai-compatible",
            Self::OpencodeGo => "opencode-go",
            Self::Anthropic => "anthropic",
            Self::Litellm => "litellm",
        }
    }

    /// The protocol to use for `provider` when the profile names none.
    ///
    /// OpenAI is spelled `openai` as a provider but `openai-compatible` as a
    /// protocol, so the mapping is not the identity.
    pub const fn for_provider(provider: ProviderType) -> Self {
        match provider {
            ProviderType::Anthropic => Self::Anthropic,
            ProviderType::Openai | ProviderType::OpenaiCompatible => Self::OpenaiCompatible,
        }
    }
}

impl std::fmt::Display for ProviderProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── Capabilities ──────────────────────────────────────────────────────────────

/// Declared provider capabilities.
///
/// Every flag defaults to `false` — "not declared" and "declared absent" are
/// the same thing to a caller, and assuming a capability is present is how a
/// tools request gets rejected by an endpoint that cannot serve it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderCapabilities {
    /// Tool/function calling.
    pub tools: bool,
    /// Server-sent streaming.
    pub streaming: bool,
    /// JSON-schema structured output.
    pub structured_output: bool,
    /// Extended reasoning.
    pub thinking: bool,
    /// Image input.
    pub vision: bool,
    /// Embedding models.
    pub embeddings: bool,
}

impl ProviderCapabilities {
    /// Whether every capability is absent.
    ///
    /// Lets the field be skipped on serialization so a config that never
    /// declared a capability does not start claiming all six are `false`.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Whether `name` is one of the accepted capability names.
    pub fn is_known(name: &str) -> bool {
        CAPABILITY_NAMES.contains(&name)
    }

    /// Whether the named capability is declared present.
    pub fn has(&self, name: &str) -> bool {
        match name {
            "tools" => self.tools,
            "streaming" => self.streaming,
            "structured_output" => self.structured_output,
            "thinking" => self.thinking,
            "vision" => self.vision,
            "embeddings" => self.embeddings,
            _ => false,
        }
    }
}

// ── Request options ───────────────────────────────────────────────────────────

/// Provider request options (`providers.<name>.request_options`).
///
/// Parity with agenthicc's `RequestOptionSettings`. `timeout_s` and
/// `max_retries` default to `None` rather than a number: `None` means "do not
/// override", which is different from any particular value and is what lets a
/// profile inherit `[execution]`'s transport policy.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestOptionsConfig {
    /// Headers added to every request from this profile.
    pub extra_headers: BTreeMap<String, HeaderValue>,
    /// Query parameters added to every request.
    pub extra_query: BTreeMap<String, serde_json::Value>,
    /// Extra JSON body members.
    pub extra_body: BTreeMap<String, serde_json::Value>,
    /// Provider-SDK-specific options.
    pub provider: BTreeMap<String, serde_json::Value>,
    /// Per-request timeout, seconds.
    pub timeout_s: Option<f64>,
    /// SDK-internal retry budget.
    pub max_retries: Option<u32>,
    /// Return the unparsed provider response alongside the parsed one.
    pub include_raw_response: bool,
}

impl RequestOptionsConfig {
    /// Reject the option combinations that cannot produce a single request.
    ///
    /// Two failure modes are caught here. A key present in *both* `provider`
    /// and `extra_body` has no defined winner. A key that is one of
    /// [`CANONICAL_REQUEST_KEYS`] is one xaft already fills in from the
    /// resolved profile, so accepting it would let a config disagree with the
    /// profile it is attached to.
    pub fn validate(&self, section: &str) -> Result<(), ConfigError> {
        if let Some(bad) = self.timeout_s.filter(|t| *t <= 0.0) {
            return Err(ConfigError::validation(
                section,
                format!("timeout_s must be greater than zero, got {bad}"),
            ));
        }

        let provider_keys: Vec<&str> = self.provider.keys().map(String::as_str).collect();
        let body_keys: Vec<&str> = self.extra_body.keys().map(String::as_str).collect();

        let overlap: Vec<&str> = provider_keys
            .iter()
            .filter(|key| body_keys.contains(key))
            .copied()
            .collect();
        if !overlap.is_empty() {
            return Err(ConfigError::validation(
                section,
                format!(
                    "duplicates option(s) in provider and extra_body: {}",
                    join_sorted(&overlap)
                ),
            ));
        }

        let canonical: Vec<&str> = provider_keys
            .iter()
            .chain(body_keys.iter())
            .copied()
            .filter(|key| CANONICAL_REQUEST_KEYS.contains(key))
            .collect();
        if !canonical.is_empty() {
            return Err(ConfigError::validation(
                section,
                format!(
                    "cannot redefine canonical request option(s): {}",
                    join_sorted(&canonical)
                ),
            ));
        }

        for name in self.extra_headers.keys() {
            validate_header_name(name, &format!("{section}.extra_headers"))?;
        }

        Ok(())
    }

    /// Ollama only accepts payload extensions; header, query and provider
    /// tables are dropped by its client.
    pub fn unsupported_by_ollama(&self) -> Vec<&'static str> {
        let mut unsupported = Vec::new();
        if !self.extra_headers.is_empty() {
            unsupported.push("extra_headers");
        }
        if !self.extra_query.is_empty() {
            unsupported.push("extra_query");
        }
        if !self.provider.is_empty() {
            unsupported.push("provider");
        }
        unsupported
    }

    /// The view that may be printed: every value replaced by [`REDACTED`]
    /// where the key looks sensitive, header literals never printed.
    pub fn redacted(&self) -> serde_json::Value {
        let headers: serde_json::Map<String, serde_json::Value> = self
            .extra_headers
            .iter()
            .map(|(name, value)| (name.clone(), value.redacted()))
            .collect();
        serde_json::json!({
            "extra_headers": serde_json::Value::Object(headers),
            "extra_query": redact_map(&self.extra_query),
            "extra_body": redact_map(&self.extra_body),
            "provider": redact_map(&self.provider),
            "timeout_s": self.timeout_s,
            "max_retries": self.max_retries,
            "include_raw_response": self.include_raw_response,
        })
    }
}

/// Resolve every header in a table to its literal value.
pub fn resolve_headers<F>(
    headers: &BTreeMap<String, HeaderValue>,
    lookup: F,
    section: &str,
) -> Result<BTreeMap<String, String>, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    headers
        .iter()
        .map(|(name, value)| {
            let resolved = value.resolve_with(&lookup, &format!("{section}.{name}"))?;
            Ok((name.clone(), resolved))
        })
        .collect()
}

/// Validate a session identity destined for a dynamic header.
pub fn validate_session_identity(value: &str, section: &str) -> Result<(), ConfigError> {
    let bad = value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
        || value.contains('\r')
        || value.contains('\n')
        || value.len() > MAX_SESSION_ID_BYTES;
    if bad {
        return Err(ConfigError::validation(
            section,
            "must be a non-empty safe identifier",
        ));
    }
    Ok(())
}

/// Copy static headers and attach one configured session identity.
///
/// A configured `session_header` with no identity is an error rather than a
/// silent no-op: the profile asked for per-session routing, and quietly not
/// doing it would route every session to whatever the upstream default is.
pub fn attach_session_header(
    headers: &BTreeMap<String, String>,
    header_name: &str,
    conversation_id: Option<&str>,
) -> Result<BTreeMap<String, String>, ConfigError> {
    let section = "session_header";
    // An unset session header is the empty string, not an invalid one, so the
    // emptiness check has to come before the tchar validation.
    if header_name.trim().is_empty() {
        return Ok(headers.clone());
    }
    let name = validate_header_name(header_name, section)?;
    let identity = conversation_id.ok_or_else(|| {
        ConfigError::validation(
            section,
            format!(
                "'{name}' requires a session conversation_id before the provider transport \
                 can be constructed"
            ),
        )
    })?;
    validate_session_identity(identity, "conversation_id")?;

    if let Some(conflict) = headers.keys().find(|key| key.eq_ignore_ascii_case(&name)) {
        return Err(ConfigError::validation(
            section,
            format!(
                "'{name}' conflicts with static header '{conflict}'; remove the static header \
                 and let xaft supply the session identity"
            ),
        ));
    }

    let mut result = headers.clone();
    result.insert(name, identity.to_string());
    Ok(result)
}

// ── ResolvedProviderProfile ────────────────────────────────────────────────────

/// Runtime provider profile with secrets resolved just before an LLM call.
///
/// Parity with agenthicc's `ResolvedProviderProfile`. Owned maps stand in for
/// agenthicc's `MappingProxyType`: the point of that wrapper is that a running
/// session cannot mutate the profile under the transport, and moving the
/// struct by value at the call site gives the same guarantee.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolvedProviderProfile {
    /// Profile name.
    pub name: String,
    /// Provider backend.
    pub provider: ProviderType,
    /// Model id.
    pub model: String,
    /// Endpoint base URL.
    pub base_url: String,
    /// Resolved API key.
    pub api_key: Option<String>,
    /// Static headers, secrets already resolved.
    pub default_headers: BTreeMap<String, String>,
    /// Default query parameters.
    pub default_query: BTreeMap<String, serde_json::Value>,
    /// Transport-level client options.
    pub client_options: ClientOptions,
    /// Per-request options, headers already resolved.
    pub request_options: Option<ResolvedRequestOptions>,
    /// Per-request timeout, seconds.
    pub timeout_s: Option<f64>,
    /// Retry budget.
    pub max_retries: Option<u32>,
    /// Default sampling temperature.
    pub temperature: Option<f32>,
    /// Default nucleus sampling.
    pub top_p: Option<f32>,
    /// Completion-token ceiling.
    pub max_completion_tokens: Option<u32>,
    /// Wire protocol, always concrete.
    pub protocol: ProviderProtocol,
    /// Declared capabilities.
    pub capabilities: ProviderCapabilities,
    /// Dynamic session header name, possibly empty.
    pub session_header: String,
}

/// [`RequestOptionsConfig`] after header resolution.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolvedRequestOptions {
    /// Headers added to every request, secrets already resolved.
    pub extra_headers: BTreeMap<String, String>,
    /// Query parameters added to every request.
    pub extra_query: BTreeMap<String, serde_json::Value>,
    /// Extra JSON body members.
    pub extra_body: BTreeMap<String, serde_json::Value>,
    /// Provider-SDK-specific options.
    pub provider: BTreeMap<String, serde_json::Value>,
    /// Per-request timeout, seconds.
    pub timeout_s: Option<f64>,
    /// SDK-internal retry budget.
    pub max_retries: Option<u32>,
    /// Return the unparsed provider response too.
    pub include_raw_response: bool,
}

impl ProviderConfig {
    /// Resolve this profile into the form a transport can be built from.
    ///
    /// `lookup` reads environment variables. `model` supplies the profile's
    /// own default model; a profile that names none resolves to `None`, which
    /// is left to the agent preset rather than guessed at here.
    pub fn resolve_profile<F>(
        &self,
        name: &str,
        lookup: F,
    ) -> Result<ResolvedProviderProfile, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let section = format!("provider.{name}");
        let api_key = self.resolve_api_key_with(&lookup, name);
        let default_headers = resolve_headers(
            &self
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), HeaderValue::Literal(v.clone())))
                .collect(),
            &lookup,
            &section,
        )?;
        let request_options = self
            .request_options
            .as_ref()
            .map(|options| options.resolve(&lookup, &section))
            .transpose()?;

        Ok(ResolvedProviderProfile {
            name: name.to_string(),
            provider: self.provider_type,
            model: self.default_model().unwrap_or_default().to_string(),
            base_url: self.base_url.clone(),
            api_key,
            default_headers,
            default_query: self.default_query.clone(),
            client_options: self.client_options.clone(),
            request_options,
            timeout_s: self.timeout_s,
            max_retries: Some(self.max_retries),
            temperature: self.temperature,
            top_p: self.top_p,
            max_completion_tokens: self.max_completion_tokens,
            protocol: self
                .protocol
                .unwrap_or_else(|| ProviderProtocol::for_provider(self.provider_type)),
            capabilities: self.capabilities,
            session_header: self.session_header.clone(),
        })
    }

    /// The safe-to-print view of this profile.
    ///
    /// Never returns an API key, a header value or a sensitive option value —
    /// see [`REDACTED`].
    pub fn redacted(&self) -> serde_json::Value {
        let headers: serde_json::Map<String, serde_json::Value> = self
            .headers
            .keys()
            .map(|name| (name.clone(), serde_json::Value::String(REDACTED.into())))
            .collect();
        serde_json::json!({
            "provider": self.provider_type.as_str(),
            "model": self.model,
            "base_url": self.base_url,
            "api_key": if self.api_key.is_empty() && self.api_key_env.is_none() {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(REDACTED.into())
            },
            "api_key_env": self.api_key_env,
            "default_headers": serde_json::Value::Object(headers),
            "session_header": self.session_header,
            "default_query": redact_map(&self.default_query),
            "client_options": self.client_options,
            "request_options": self.request_options.as_ref().map(RequestOptionsConfig::redacted),
            "timeout_s": self.timeout_s,
            "max_retries": self.max_retries,
            "temperature": self.temperature,
            "top_p": self.top_p,
            "max_completion_tokens": self.max_completion_tokens,
            "protocol": self.protocol.map(ProviderProtocol::as_str),
            "capabilities": self.capabilities,
        })
    }
}

impl RequestOptionsConfig {
    /// Resolve header secrets, producing the transport-facing view.
    pub fn resolve<F>(
        &self,
        lookup: F,
        section: &str,
    ) -> Result<ResolvedRequestOptions, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        Ok(ResolvedRequestOptions {
            extra_headers: resolve_headers(&self.extra_headers, &lookup, section)?,
            extra_query: self.extra_query.clone(),
            extra_body: self.extra_body.clone(),
            provider: self.provider.clone(),
            timeout_s: self.timeout_s,
            max_retries: self.max_retries,
            include_raw_response: self.include_raw_response,
        })
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Whether a config key names something that must never be printed verbatim.
///
/// This is agenthicc's `_SENSITIVE_KEY_PARTS` test, and the single definition of
/// the rule in xaft: [`redact_value`] and the whole-config walk in
/// [`crate::redact`] both call it, so the two renderers cannot drift apart about
/// which keys are secret. `mcp get` masking `token` while `config show` printed
/// it is exactly the kind of divergence a second copy of this list produces.
pub fn key_is_sensitive(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    SENSITIVE_KEY_PARTS
        .iter()
        .any(|part| lowered.contains(part))
}

/// Recursively redact sensitive-looking option keys.
pub fn redact_value(value: &serde_json::Value, key: &str) -> serde_json::Value {
    if key_is_sensitive(key) {
        return serde_json::Value::String(REDACTED.into());
    }
    match value {
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(|item| redact_value(item, key)).collect())
        }
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(name, item)| (name.clone(), redact_value(item, name)))
            .collect::<serde_json::Map<_, _>>()
            .into(),
        _ => value.clone(),
    }
}

/// [`redact_value`] over a whole option table, keyed by its members.
pub fn redact_map(map: &BTreeMap<String, serde_json::Value>) -> serde_json::Value {
    serde_json::Value::Object(
        map.iter()
            .map(|(name, value)| (name.clone(), redact_value(value, name)))
            .collect(),
    )
}

/// Join and sort key names for an error message.
fn join_sorted(keys: &[&str]) -> String {
    let mut sorted = keys.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    sorted.join(", ")
}
