//! Integration tests for the provider connection-profile extras.
//!
//! Parity targets in agenthicc's `config.py`: `SecretReference`,
//! `RequestOptionSettings`, the `ProviderProfile` request-shaped fields, and
//! `ResolvedProviderProfile`.
//!
//! Every environment lookup goes through an injected closure rather than
//! `std::env::set_var`. That is deliberate: the process environment is global,
//! so tests that mutate it race each other under `cargo test`'s threads, and a
//! flake here would be nearly impossible to attribute.

use std::collections::BTreeMap;

use xaft_config::provider::{
    redact_map, redact_value, resolve_headers, validate_header_name, validate_session_identity,
};
use xaft_config::{
    attach_session_header, ClientOptions, ConfigLoader, HeaderValue, ProviderCapabilities,
    ProviderConfig, ProviderProtocol, ProviderType, RequestOptionsConfig, ResolvedProviderProfile,
    SecretRef, Verify, CAPABILITY_NAMES, CANONICAL_REQUEST_KEYS, MAX_SESSION_ID_BYTES, REDACTED,
};
use tempfile::TempDir;

/// A fixed environment, so a test states exactly which secrets exist.
fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let owned: BTreeMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    move |name: &str| owned.get(name).cloned()
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).expect("test JSON literal")
}

// ── SecretReference ───────────────────────────────────────────────────────────

#[test]
fn secret_ref_holds_only_the_variable_name() {
    let secret = SecretRef::new("  OPENAI_API_KEY  ", "test").expect("valid name");
    assert_eq!(secret.env, "OPENAI_API_KEY", "name must be trimmed");
}

#[test]
fn secret_ref_rejects_names_that_are_not_ident_shaped() {
    for bad in [
        "",
        "   ",
        "1LEADING_DIGIT",
        "HAS-DASH",
        "HAS SPACE",
        "HAS.DOT",
        "HAS=EQUALS",
        "ACCEPTS\u{0}",
    ] {
        assert!(
            SecretRef::new(bad, "test").is_err(),
            "{bad:?} must not be accepted as an environment variable name"
        );
    }
}

#[test]
fn secret_ref_accepts_ident_shaped_names() {
    for good in ["A", "_", "_LEADING", "XAFT_PROVIDER_API_KEY", "Mixed_Case_9"] {
        assert!(SecretRef::new(good, "test").is_ok(), "{good:?} must be accepted");
    }
}

#[test]
fn secret_ref_resolves_through_the_injected_lookup() {
    let secret = SecretRef::new("TOKEN", "test").unwrap();
    let value = secret.resolve_with(env(&[("TOKEN", "s3cret")]), "test").unwrap();
    assert_eq!(value, "s3cret");
}

#[test]
fn secret_ref_treats_an_empty_variable_as_missing() {
    let secret = SecretRef::new("TOKEN", "test").unwrap();
    let error = secret
        .resolve_with(env(&[("TOKEN", "")]), "test")
        .expect_err("an empty value is not a usable secret");
    assert!(
        error.to_string().contains("TOKEN"),
        "the error must name the variable it could not read: {error}"
    );
}

#[test]
fn secret_ref_treats_an_unset_variable_as_missing() {
    let secret = SecretRef::new("TOKEN", "test").unwrap();
    assert!(secret.resolve_with(env(&[]), "test").is_err());
}

#[test]
fn secret_ref_redacted_view_is_the_name_only() {
    let secret = SecretRef::new("TOKEN", "test").unwrap();
    assert_eq!(
        secret.redacted().get("env").map(String::as_str),
        Some("TOKEN")
    );
}

#[test]
fn secret_ref_round_trips_through_toml() {
    let parsed: SecretRef = toml::from_str(r#"env = "MY_TOKEN""#).expect("parse");
    assert_eq!(parsed, SecretRef::new("MY_TOKEN", "t").unwrap());
    let rendered = toml::to_string(&parsed).expect("render");
    assert!(rendered.contains("MY_TOKEN"));
}

// ── HeaderValue ───────────────────────────────────────────────────────────────

#[test]
fn header_value_parses_a_literal() {
    let parsed = HeaderValue::parse(&json(r#""plain""#), "h").expect("literal parses");
    assert_eq!(parsed, HeaderValue::Literal("plain".into()));
}

#[test]
fn header_value_parses_an_env_reference() {
    let parsed = HeaderValue::parse(&json(r#"{"env": "H_TOKEN"}"#), "h").expect("ref parses");
    assert_eq!(parsed, HeaderValue::Env(SecretRef::new("H_TOKEN", "t").unwrap()));
}

#[test]
fn header_value_rejects_crlf_in_a_literal() {
    // The whole point of checking here rather than at request-build time: a
    // literal header value is the header-injection primitive, and by the time a
    // request is assembled the offending config is several frames away.
    let error = HeaderValue::parse(&json(r#""a\r\nX-Injected: yes""#), "h")
        .expect_err("CRLF must be rejected");
    assert!(error.to_string().contains("CR/LF"), "{error}");
}

#[test]
fn header_value_rejects_a_broken_env_reference() {
    for bad in [
        r#"{"env": 7}"#,
        r#"{"env": "1BAD"}"#,
        r#"{"env": "OK", "extra": "no"}"#,
        r#"{}"#,
        "[]",
        "42",
        "null",
        "true",
    ] {
        assert!(
            HeaderValue::parse(&json(bad), "h").is_err(),
            "{bad} must not parse as a header value"
        );
    }
}

#[test]
fn header_value_resolves_a_reference_through_the_lookup() {
    let value = HeaderValue::Env(SecretRef::new("H", "t").unwrap());
    assert_eq!(value.resolve_with(env(&[("H", "v")]), "h").unwrap(), "v");
}

#[test]
fn header_value_redacts_a_literal_but_not_a_reference() {
    assert_eq!(
        HeaderValue::Literal("hunter2".into()).redacted(),
        json(r#""<redacted>""#)
    );
    assert_eq!(
        HeaderValue::Env(SecretRef::new("H", "t").unwrap()).redacted(),
        json(r#"{"env": "H"}"#)
    );
}

// ── Header names and session identity ─────────────────────────────────────────

#[test]
fn header_name_accepts_the_rfc_tchar_set() {
    for good in ["Authorization", "X-Trace-Id", "a_b", "x.y", "X!#$%&'*+^`|~"] {
        assert!(validate_header_name(good, "h").is_ok(), "{good:?} must be accepted");
    }
}

#[test]
fn header_name_is_trimmed_and_normalized() {
    assert_eq!(validate_header_name("  X-Trace  ", "h").unwrap(), "X-Trace");
}

#[test]
fn header_name_rejects_whitespace_separators_and_non_ascii() {
    for bad in ["", "   ", "has space", "has: colon", "has,comma", "naïve", "semi;colon"] {
        assert!(
            validate_header_name(bad, "h").is_err(),
            "{bad:?} must not be accepted as a header name"
        );
    }
}

#[test]
fn session_identity_rejects_unsafe_identifiers() {
    for bad in [
        "",
        ".",
        "..",
        "a/b",
        "a\\b",
        "a\0b",
        "a\rb",
        "a\nb",
    ] {
        assert!(
            validate_session_identity(bad, "conversation_id").is_err(),
            "{bad:?} must not be accepted as a session identity"
        );
    }
}

#[test]
fn session_identity_accepts_a_plain_id_and_enforces_the_byte_ceiling() {
    assert!(validate_session_identity("session-123_ABC", "conversation_id").is_ok());

    let at_limit = "a".repeat(MAX_SESSION_ID_BYTES);
    assert!(validate_session_identity(&at_limit, "conversation_id").is_ok());

    let over_limit = "a".repeat(MAX_SESSION_ID_BYTES + 1);
    assert!(validate_session_identity(&over_limit, "conversation_id").is_err());
}

// ── attach_session_header ─────────────────────────────────────────────────────

fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn no_session_header_leaves_the_static_headers_untouched() {
    let static_headers = headers(&[("X-Trace", "1")]);
    let result = attach_session_header(&static_headers, "", None).expect("no-op");
    assert_eq!(result, static_headers);
    assert!(!result.contains_key(""));
}

#[test]
fn session_header_without_a_conversation_id_is_an_error() {
    // Silently not sending the header would route every session to whatever
    // the upstream default is, which is the failure this guards.
    let error = attach_session_header(&BTreeMap::new(), "X-Session-Id", None)
        .expect_err("a configured session header needs an identity");
    assert!(error.to_string().contains("X-Session-Id"), "{error}");
}

#[test]
fn session_header_is_attached_to_a_copy_of_the_static_headers() {
    let static_headers = headers(&[("X-Trace", "1")]);
    let result =
        attach_session_header(&static_headers, "X-Session-Id", Some("conv-7")).expect("attach");
    assert_eq!(result.get("X-Session-Id").map(String::as_str), Some("conv-7"));
    assert_eq!(result.get("X-Trace").map(String::as_str), Some("1"));
    assert_eq!(static_headers.len(), 1, "the caller's map must not be mutated");
}

#[test]
fn session_header_colliding_with_a_static_header_is_an_error() {
    let error = attach_session_header(&headers(&[("x-session-id", "static")]), "X-Session-Id", Some("c"))
        .expect_err("case-insensitive collision must be caught");
    assert!(error.to_string().contains("conflicts"), "{error}");
}

#[test]
fn session_header_collision_is_case_insensitive_in_both_directions() {
    assert!(attach_session_header(&headers(&[("X-SESSION-ID", "s")]), "x-session-id", Some("c")).is_err());
    assert!(attach_session_header(&headers(&[("x-session-id", "s")]), "X-Session-Id", Some("c")).is_err());
}

// ── ClientOptions ─────────────────────────────────────────────────────────────

#[test]
fn client_options_default_to_unset_and_report_empty() {
    let options = ClientOptions::default();
    assert!(options.is_empty());
    assert_eq!(options.follow_redirects, None);
    assert_eq!(options.verify, None);
    assert_eq!(options.max_redirects, None);
}

#[test]
fn client_options_report_non_empty_once_anything_is_set() {
    let options = ClientOptions {
        http2: Some(true),
        ..ClientOptions::default()
    };
    assert!(!options.is_empty());
}

#[test]
fn client_options_reject_unknown_keys() {
    // An unknown transport knob silently ignored is worse than a rejection:
    // the config says one thing and the client does another.
    let error = toml::from_str::<ClientOptions>("not_a_real_option = true")
        .expect_err("unknown client option must be rejected");
    assert!(error.to_string().contains("not_a_real_option"), "{error}");
}

#[test]
fn client_options_accept_every_documented_key() {
    let parsed: ClientOptions = toml::from_str(
        r#"
follow_redirects = false
http2 = true
verify = "/etc/ssl/certs/ca.pem"
trust_env = false
max_redirects = 3
local_address = "127.0.0.1"
"#,
    )
    .expect("all documented client options parse");
    assert_eq!(parsed.follow_redirects, Some(false));
    assert_eq!(parsed.http2, Some(true));
    assert_eq!(
        parsed.verify,
        Some(Verify::Path("/etc/ssl/certs/ca.pem".into()))
    );
    assert_eq!(parsed.trust_env, Some(false));
    assert_eq!(parsed.max_redirects, Some(3));
    assert_eq!(parsed.local_address.as_deref(), Some("127.0.0.1"));
    assert!(!parsed.is_empty());
}

#[test]
fn verify_accepts_a_boolean_flag_as_well_as_a_path() {
    let parsed: ClientOptions = toml::from_str("verify = false").expect("bool verify");
    assert_eq!(parsed.verify, Some(Verify::Flag(false)));
}

// ── Protocol ──────────────────────────────────────────────────────────────────

#[test]
fn protocol_serializes_to_the_exact_agenthicc_wire_strings() {
    let cases = [
        (ProviderProtocol::ChatCompletions, "chat_completions"),
        (ProviderProtocol::Messages, "messages"),
        (ProviderProtocol::Ollama, "ollama"),
        (ProviderProtocol::Openai, "openai"),
        (ProviderProtocol::OpenaiCompatible, "openai-compatible"),
        (ProviderProtocol::OpencodeGo, "opencode-go"),
        (ProviderProtocol::Anthropic, "anthropic"),
        (ProviderProtocol::Litellm, "litellm"),
    ];
    for (protocol, wire) in cases {
        assert_eq!(protocol.as_str(), wire);
        assert_eq!(protocol.to_string(), wire);
        // A bare enum is not a TOML table, so round-trip it as a value inside
        // one; that is also how it actually appears in a config file.
        let wrapped: toml::Value = toml::from_str(&format!("p = \"{wire}\""))
            .expect("parse as toml value");
        let back: ProviderProtocol = wrapped["p"].clone().try_into().expect("convert");
        assert_eq!(back, protocol, "{wire} must survive a TOML round trip");
    }
}

#[test]
fn protocol_is_derived_from_the_provider_when_unset() {
    // OpenAI is spelled `openai` as a provider but `openai-compatible` as a
    // protocol, so this mapping is deliberately not the identity.
    assert_eq!(
        ProviderProtocol::for_provider(ProviderType::Openai),
        ProviderProtocol::OpenaiCompatible
    );
    assert_eq!(
        ProviderProtocol::for_provider(ProviderType::OpenaiCompatible),
        ProviderProtocol::OpenaiCompatible
    );
    assert_eq!(
        ProviderProtocol::for_provider(ProviderType::Anthropic),
        ProviderProtocol::Anthropic
    );
}

#[test]
fn protocol_rejects_an_unknown_value() {
    let error = toml::from_str::<ProviderProtocol>(r#""grpc""#).expect_err("unknown protocol");
    assert!(error.to_string().contains("grpc"), "{error}");
}

// ── Capabilities ──────────────────────────────────────────────────────────────

#[test]
fn capabilities_default_to_all_false() {
    // "Not declared" and "declared absent" mean the same thing to a caller, and
    // assuming a capability is present is how a tools request gets rejected.
    let caps = ProviderCapabilities::default();
    for name in CAPABILITY_NAMES {
        assert!(!caps.has(name), "{name} must default to absent");
    }
    assert!(!caps.tools);
    assert!(!caps.embeddings);
}

#[test]
fn capabilities_report_each_named_flag() {
    let caps: ProviderCapabilities =
        toml::from_str("tools = true\nvision = true\nthinking = true").expect("parse");
    assert!(caps.tools);
    assert!(caps.vision);
    assert!(caps.thinking);
    assert!(!caps.streaming, "an omitted flag stays absent");
    assert!(caps.has("tools"));
    assert!(!caps.has("embeddings"));
    assert!(!caps.has("telepathy"), "an unknown name is simply absent");
}

#[test]
fn capabilities_reject_an_unknown_capability_name() {
    let error =
        toml::from_str::<ProviderCapabilities>("telepathy = true").expect_err("unknown capability");
    assert!(error.to_string().contains("telepathy"), "{error}");
}

#[test]
fn capability_names_match_agenthicc() {
    assert_eq!(
        CAPABILITY_NAMES,
        [
            "tools",
            "streaming",
            "structured_output",
            "thinking",
            "vision",
            "embeddings"
        ]
    );
}

// ── RequestOptionsConfig validation ───────────────────────────────────────────

fn options() -> RequestOptionsConfig {
    RequestOptionsConfig::default()
}

#[test]
fn request_options_default_to_nothing_set() {
    let opts = options();
    assert!(opts.extra_headers.is_empty());
    assert!(opts.extra_query.is_empty());
    assert!(opts.extra_body.is_empty());
    assert!(opts.provider.is_empty());
    assert_eq!(opts.timeout_s, None, "unset must not become a number");
    assert_eq!(opts.max_retries, None);
    assert!(!opts.include_raw_response);
}

#[test]
fn request_options_accept_an_ordinary_table() {
    let parsed: RequestOptionsConfig = toml::from_str(
        r#"
[extra_headers]
X-Trace = "1"

[extra_query]
"api-version" = "2024-01-01"

[extra_body]
some_extension = true
"#,
    )
    .expect("ordinary request options parse");
    assert_eq!(
        parsed.extra_headers.get("X-Trace"),
        Some(&HeaderValue::Literal("1".into()))
    );
    assert_eq!(parsed.extra_query["api-version"], json(r#""2024-01-01""#));
    assert!(parsed.validate("test").is_ok());
}

#[test]
fn request_options_reject_a_non_positive_timeout() {
    for bad in [0.0, -1.0] {
        let mut opts = options();
        opts.timeout_s = Some(bad);
        let error = opts.validate("test").expect_err("timeout must be positive");
        assert!(error.to_string().contains("timeout_s"), "{error}");
    }
}

#[test]
fn request_options_reject_a_key_stated_in_both_provider_and_extra_body() {
    let mut opts = options();
    opts.provider.insert("beta".into(), json("true"));
    opts.extra_body.insert("beta".into(), json("true"));
    let error = opts.validate("test").expect_err("ambiguous key");
    assert!(error.to_string().contains("duplicates option(s)"), "{error}");
    assert!(error.to_string().contains("beta"), "{error}");
}

#[test]
fn request_options_reject_restating_a_canonical_request_key() {
    // These are the keys xaft fills in from the resolved profile; letting a
    // config restate one is how a model in extra_body silently wins over the
    // model the profile actually resolved.
    for key in CANONICAL_REQUEST_KEYS {
        let mut in_provider = options();
        in_provider
            .provider
            .insert((*key).to_string(), json("1"));
        assert!(
            in_provider.validate("test").is_err(),
            "{key} must not be restated in provider"
        );

        let mut in_body = options();
        in_body.extra_body.insert((*key).to_string(), json("1"));
        assert!(
            in_body.validate("test").is_err(),
            "{key} must not be restated in extra_body"
        );
    }
}

#[test]
fn request_options_allow_a_non_canonical_key_in_provider() {
    let mut opts = options();
    opts.provider.insert("beta".into(), json("true"));
    assert!(opts.validate("test").is_ok());
}

#[test]
fn request_options_reject_an_invalid_header_name() {
    let mut opts = options();
    opts.extra_headers
        .insert("has space".into(), HeaderValue::Literal("v".into()));
    assert!(opts.validate("test").is_err());
}

#[test]
fn request_options_flag_ollama_unsupported_tables() {
    let mut opts = options();
    opts.extra_body.insert("options".into(), json("{}"));
    assert!(
        opts.unsupported_by_ollama().is_empty(),
        "extra_body is the one table Ollama does accept"
    );

    opts.extra_headers
        .insert("X-Trace".into(), HeaderValue::Literal("1".into()));
    opts.extra_query.insert("v".into(), json("1"));
    opts.provider.insert("beta".into(), json("true"));

    let mut unsupported = opts.unsupported_by_ollama();
    unsupported.sort_unstable();
    assert_eq!(unsupported, ["extra_headers", "extra_query", "provider"]);
}

// ── Redaction ─────────────────────────────────────────────────────────────────

#[test]
fn redaction_walks_arrays_and_objects() {
    assert_eq!(redact_value(&json("42"), ""), json("42"));
    assert_eq!(redact_value(&json(r#""plain""#), "harmless"), json(r#""plain""#));

    assert_eq!(
        redact_value(&json(r#"{"api_key": "abc"}"#), ""),
        json(r#"{"api_key": "<redacted>"}"#)
    );
    assert_eq!(
        redact_value(&json(r#"[{"password": "p"}]"#), ""),
        json(r#"[{"password": "<redacted>"}]"#)
    );
    assert_eq!(
        redact_value(&json(r#"[{"note": "p"}]"#), ""),
        json(r#"[{"note": "p"}]"#)
    );
}

#[test]
fn redaction_matches_agenthicc_sensitive_key_substrings() {
    for key in ["token", "secret", "password", "credential", "authorization", "api_key"] {
        assert_eq!(
            redact_value(&json(r#""v""#), key),
            json(r#""<redacted>""#),
            "{key} must be redacted"
        );
    }
}

#[test]
fn redaction_is_case_insensitive_and_substring_based() {
    assert_eq!(redact_value(&json(r#""v""#), "API_KEY"), json(r#""<redacted>""#));
    assert_eq!(
        redact_value(&json(r#""v""#), "my_refresh_token_name"),
        json(r#""<redacted>""#)
    );
    assert_eq!(redact_value(&json(r#""v""#), "model"), json(r#""v""#));
}

#[test]
fn redact_map_redacts_per_member_key() {
    let map: BTreeMap<String, serde_json::Value> = [
        ("model".to_string(), json(r#""gpt""#)),
        ("api_token".to_string(), json(r#""t""#)),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        redact_map(&map),
        json(r#"{"api_token": "<redacted>", "model": "gpt"}"#)
    );
}

#[test]
fn request_options_redacted_view_never_carries_a_secret() {
    let opts: RequestOptionsConfig = toml::from_str(
        r#"
[extra_headers]
Authorization = "Bearer hunter2"
X-Token = { env = "AUTH_TOKEN" }

[extra_query]
"api-version" = "2024-01-01"

[extra_body]
my_password = "hunter2"
"#,
    )
    .expect("parse");

    let rendered = opts.redacted().to_string();
    assert!(
        !rendered.contains("hunter2"),
        "no literal secret may survive redaction: {rendered}"
    );
    assert!(
        rendered.contains(REDACTED),
        "literals must be replaced by the placeholder: {rendered}"
    );
    assert!(
        rendered.contains("AUTH_TOKEN"),
        "an env reference is not a secret, so its name may be shown: {rendered}"
    );
    assert!(rendered.contains("api-version"), "query keys survive: {rendered}");
}

#[test]
fn provider_redacted_view_never_carries_a_secret() {
    let config: ProviderConfig = toml::from_str(
        r#"
type = "openai"
api_key = "sk-live-hunter2"
[headers]
Authorization = "Bearer hunter2"
"#,
    )
    .expect("parse");

    let rendered = config.redacted().to_string();
    assert!(!rendered.contains("hunter2"), "{rendered}");
    assert!(rendered.contains("Authorization"), "header names are not secrets");
}

#[test]
fn provider_redacted_view_reports_a_missing_key_as_null() {
    let config: ProviderConfig = toml::from_str(r#"type = "openai""#).expect("parse");
    assert_eq!(config.redacted()["api_key"], serde_json::Value::Null);
}

#[test]
fn provider_redacted_view_redacts_a_present_key() {
    let config: ProviderConfig =
        toml::from_str(r#"type = "openai"
api_key_env = "OPENAI_API_KEY""#)
        .expect("parse");
    assert_eq!(config.redacted()["api_key"], json(r#""<redacted>""#));
}

// ── resolve_headers ───────────────────────────────────────────────────────────

#[test]
fn resolve_headers_resolves_every_entry() {
    let map: BTreeMap<String, HeaderValue> = [
        ("X-Static".to_string(), HeaderValue::Literal("s".into())),
        ("X-Secret".to_string(), HeaderValue::Env(SecretRef::new("S", "t").unwrap())),
    ]
    .into_iter()
    .collect();

    let resolved = resolve_headers(&map, env(&[("S", "v")]), "provider.p").expect("resolve");
    assert_eq!(resolved.get("X-Static").map(String::as_str), Some("s"));
    assert_eq!(resolved.get("X-Secret").map(String::as_str), Some("v"));
}

#[test]
fn resolve_headers_fails_the_whole_table_on_one_missing_secret() {
    let map: BTreeMap<String, HeaderValue> = [
        ("X-Static".to_string(), HeaderValue::Literal("s".into())),
        ("X-Secret".to_string(), HeaderValue::Env(SecretRef::new("ABSENT", "t").unwrap())),
    ]
    .into_iter()
    .collect();
    assert!(resolve_headers(&map, env(&[]), "provider.p").is_err());
}

// ── ResolvedProviderProfile ───────────────────────────────────────────────────

fn provider(toml_text: &str) -> ProviderConfig {
    toml::from_str(toml_text).expect("parse provider")
}

#[test]
fn resolved_profile_derives_the_protocol_when_the_profile_omits_it() {
    let config = provider(r#"type = "openai"
base_url = "https://api.openai.com/v1""#);
    let resolved: ResolvedProviderProfile = config
        .resolve_profile("openai", env(&[]))
        .expect("resolve");
    assert_eq!(resolved.protocol, ProviderProtocol::OpenaiCompatible);
}

#[test]
fn resolved_profile_keeps_an_explicit_protocol() {
    let config = provider(r#"type = "openai"
base_url = "https://x"
protocol = "chat_completions""#);
    let resolved = config.resolve_profile("openai", env(&[])).expect("resolve");
    assert_eq!(resolved.protocol, ProviderProtocol::ChatCompletions);
}

#[test]
fn resolved_profile_carries_every_request_shaped_field() {
    let config = provider(
        r#"type = "anthropic"
base_url = "https://api.anthropic.com"
model = "claude-sonnet-4"
timeout_s = 30.0
max_retries = 5
temperature = 0.2
top_p = 0.9
max_completion_tokens = 4096
session_header = "X-Session-Id"

[default_query]
"api-version" = "2024-01-01"

[client_options]
http2 = true

[capabilities]
tools = true
streaming = true

[request_options]
include_raw_response = true

[request_options.extra_headers]
X-Trace = "1"
"#,
    );

    let resolved = config
        .resolve_profile("anthropic", env(&[]))
        .expect("resolve");

    assert_eq!(resolved.name, "anthropic");
    assert_eq!(resolved.model, "claude-sonnet-4");
    assert_eq!(resolved.timeout_s, Some(30.0));
    assert_eq!(resolved.max_retries, Some(5));
    assert_eq!(resolved.temperature, Some(0.2));
    assert_eq!(resolved.top_p, Some(0.9));
    assert_eq!(resolved.max_completion_tokens, Some(4096));
    assert_eq!(resolved.session_header, "X-Session-Id");
    assert_eq!(resolved.protocol, ProviderProtocol::Anthropic);
    assert_eq!(resolved.default_query["api-version"], json(r#""2024-01-01""#));
    assert_eq!(resolved.client_options.http2, Some(true));
    assert!(resolved.capabilities.tools);
    assert!(resolved.capabilities.streaming);
    assert!(!resolved.capabilities.vision);

    let request_options = resolved.request_options.expect("request options present");
    assert!(request_options.include_raw_response);
    assert_eq!(
        request_options.extra_headers.get("X-Trace").map(String::as_str),
        Some("1")
    );
}

#[test]
fn resolved_profile_defaults_are_absent_rather_than_zeroed() {
    let config = provider(r#"type = "openai"
base_url = "https://x""#);
    let resolved = config.resolve_profile("openai", env(&[])).expect("resolve");

    assert_eq!(resolved.timeout_s, None);
    assert_eq!(resolved.temperature, None);
    assert_eq!(resolved.top_p, None);
    assert_eq!(resolved.max_completion_tokens, None);
    assert!(resolved.session_header.is_empty());
    assert!(resolved.request_options.is_none());
    assert!(resolved.client_options.is_empty());
    assert!(resolved.default_query.is_empty());
    assert_eq!(resolved.api_key, None);
    assert_eq!(resolved.model, "");
}

#[test]
fn resolved_profile_resolves_request_option_secrets() {
    let config = provider(
        r#"type = "openai"
base_url = "https://x"

[request_options.extra_headers]
X-Auth = { env = "PROFILE_TOKEN" }
"#,
    );
    let resolved = config
        .resolve_profile("openai", env(&[("PROFILE_TOKEN", "live-value")]))
        .expect("resolve");
    let request_options = resolved.request_options.expect("request options");
    assert_eq!(
        request_options.extra_headers.get("X-Auth").map(String::as_str),
        Some("live-value")
    );
}

#[test]
fn resolved_profile_fails_when_a_request_option_secret_is_missing() {
    let config = provider(
        r#"type = "openai"
base_url = "https://x"

[request_options.extra_headers]
X-Auth = { env = "PROFILE_TOKEN" }
"#,
    );
    assert!(config.resolve_profile("openai", env(&[])).is_err());
}

#[test]
fn resolved_profile_resolves_the_api_key_from_the_environment() {
    let config = provider(r#"type = "openai"
base_url = "https://x"
api_key_env = "MY_KEY""#);
    let resolved = config
        .resolve_profile("openai", env(&[("MY_KEY", "sk-1")]))
        .expect("resolve");
    assert_eq!(resolved.api_key.as_deref(), Some("sk-1"));
}

#[test]
fn api_key_env_wins_over_an_inline_key() {
    let config = provider(r#"type = "openai"
base_url = "https://x"
api_key = "sk-inline"
api_key_env = "MY_KEY""#);
    let resolved = config
        .resolve_profile("openai", env(&[("MY_KEY", "sk-env")]))
        .expect("resolve");
    assert_eq!(resolved.api_key.as_deref(), Some("sk-env"));
}

#[test]
fn inline_api_key_is_used_when_the_named_variable_is_unset() {
    let config = provider(r#"type = "openai"
base_url = "https://x"
api_key = "sk-inline"
api_key_env = "MY_KEY""#);
    let resolved = config.resolve_profile("openai", env(&[])).expect("resolve");
    assert_eq!(resolved.api_key.as_deref(), Some("sk-inline"));
}

// ── Whole-file loading ────────────────────────────────────────────────────────

fn load(toml_text: &str) -> Result<xaft_config::XaftConfig, String> {
    let dir = TempDir::new().map_err(|e| e.to_string())?;
    let path = dir.path().join("xaft.toml");
    std::fs::write(&path, toml_text).map_err(|e| e.to_string())?;
    ConfigLoader::load_file(&path).map_err(|e| e.to_string())
}

/// Load and validate, the way `ConfigLoader::load` does at startup.
///
/// `load_file` alone is not enough: it returns a partial config and skips
/// `validate()`, so a test asserting a *validation* rejection has to run it.
///
/// The `anthropic` provider every built-in preset references is prepended to
/// each fixture. `validate` checks agents before providers, so without it every
/// one of these would stop on the agent cross-reference and the provider
/// assertion under test would never be reached.
fn load_validated(toml_text: &str) -> Result<xaft_config::XaftConfig, String> {
    let with_presets = format!(
        r#"
[provider.anthropic]
type = "anthropic"
base_url = "https://api.anthropic.com"
{toml_text}"#
    );
    let config = load(&with_presets)?;
    xaft_config::validate(&config).map_err(|e| e.to_string())?;
    Ok(config)
}

#[test]
fn a_full_provider_stanza_loads_from_a_config_file() {
    let config = load(
        r#"
[provider.azure]
type = "openai-compatible"
base_url = "https://example.invalid/v1"
model = "gpt-4o"
session_header = "X-Session-Id"

[provider.azure.default_query]
"api-version" = "2024-06-01"

[provider.azure.client_options]
follow_redirects = true
max_redirects = 5

[provider.azure.capabilities]
tools = true
streaming = true
structured_output = true

[provider.azure.request_options]
timeout_s = 45.0
max_retries = 2
include_raw_response = true

[provider.azure.request_options.extra_headers]
X-Trace = "1"
X-Auth = { env = "AZURE_TOKEN" }

[provider.azure.request_options.extra_body]
some_extension = true

[provider.azure.request_options.provider]
beta = true
"#,
    )
    .expect("load");

    let azure = &config.provider["azure"];
    assert_eq!(azure.provider_type, ProviderType::OpenaiCompatible);
    assert_eq!(azure.default_model(), Some("gpt-4o"));
    assert_eq!(azure.protocol, None, "unset is distinct from a value");
    assert_eq!(azure.session_header, "X-Session-Id");
    assert_eq!(azure.client_options.follow_redirects, Some(true));
    assert!(azure.capabilities.structured_output);
    assert!(!azure.capabilities.vision);

    let resolved = azure
        .resolve_profile("azure", env(&[("AZURE_TOKEN", "t")]))
        .expect("resolve");
    assert_eq!(resolved.protocol, ProviderProtocol::OpenaiCompatible);
    assert_eq!(resolved.request_options.as_ref().unwrap().timeout_s, Some(45.0));
}

#[test]
fn a_config_without_the_new_fields_still_loads() {
    // Backward compatibility: a file written before this section existed must
    // not need editing.
    let config = load(
        r#"
[provider.openai]
type = "openai"
base_url = "https://api.openai.com/v1"
api_key = "sk-test"
"#,
    )
    .expect("load");

    let openai = &config.provider["openai"];
    assert!(openai.default_query.is_empty());
    assert!(openai.client_options.is_empty());
    assert!(openai.request_options.is_none());
    assert_eq!(openai.timeout_s, None);
    assert_eq!(openai.protocol, None);
    assert_eq!(openai.session_header, "");
    assert_eq!(openai.capabilities, ProviderCapabilities::default());
}

#[test]
fn a_bad_request_option_table_is_rejected_at_validation() {
    let error = load_validated(
        r#"
[provider.openai]
type = "openai"
base_url = "https://x"

[provider.openai.request_options.extra_body]
model = "shadow"
"#,
    )
    .expect_err("restating a canonical key must be rejected");
    assert!(error.contains("model"), "{error}");
}

#[test]
fn a_non_positive_profile_timeout_is_rejected_at_validation() {
    for bad in ["0.0", "-1.0"] {
        let error = load_validated(&format!(
            r#"
[provider.openai]
type = "openai"
base_url = "https://x"
timeout_s = {bad}
"#
        ))
        .expect_err("a non-positive timeout must be rejected");
        assert!(error.contains("timeout_s"), "{error}");
    }
}

#[test]
fn a_good_request_option_table_passes_validation() {
    // The counterpart to the rejection tests: proves the new validation is not
    // rejecting ordinary configurations.
    load_validated(
        r#"
[provider.openai]
type = "openai"
base_url = "https://x"
timeout_s = 30.0

[provider.openai.request_options.extra_body]
some_extension = true

[provider.openai.request_options.provider]
beta = true
"#,
    )
    .expect("a well-formed request_options table must validate");
}

#[test]
fn the_new_fields_round_trip_through_toml() {
    let config = load(
        r#"
[provider.p]
type = "openai"
base_url = "https://x"
timeout_s = 12.5
protocol = "openai-compatible"
session_header = "X-Session-Id"

[provider.p.capabilities]
tools = true

[provider.p.request_options.extra_query]
beta = "1"
"#,
    )
    .expect("load");

    let rendered = toml::to_string(&config.provider["p"]).expect("render");
    let reparsed: ProviderConfig = toml::from_str(&rendered).expect("re-parse");

    assert_eq!(reparsed.timeout_s, Some(12.5));
    assert_eq!(reparsed.protocol, Some(ProviderProtocol::OpenaiCompatible));
    assert_eq!(reparsed.session_header, "X-Session-Id");
    assert!(reparsed.capabilities.tools);
    assert_eq!(
        reparsed
            .request_options
            .as_ref()
            .unwrap()
            .extra_query["beta"],
        json(r#""1""#)
    );
}

#[test]
fn unset_option_tables_are_not_serialized_back_out() {
    // Emitting `request_options = {}` where the user wrote nothing would make a
    // round-tripped config claim an override it never had.
    let config = load(
        r#"
[provider.p]
type = "openai"
base_url = "https://x"
"#,
    )
    .expect("load");
    let rendered = toml::to_string(&config.provider["p"]).expect("render");

    // Match the assignment, not the bare word: `timeout_secs` is a real,
    // different field that happens to start with `timeout_s`.
    for absent in [
        "request_options",
        "timeout_s =",
        "protocol =",
        "session_header =",
        "default_query",
        "client_options",
        "capabilities",
    ] {
        assert!(
            !rendered.contains(absent),
            "{absent} was never set and must not be written back: {rendered}"
        );
    }

    // The counterpart: once set, each of them must survive serialization.
    let config = load(
        r#"
[provider.p]
type = "openai"
base_url = "https://x"
timeout_s = 30.0
protocol = "openai-compatible"
session_header = "X-Session-Id"

[provider.p.default_query]
v = "1"

[provider.p.client_options]
http2 = true

[provider.p.capabilities]
tools = true

[provider.p.request_options.extra_query]
beta = "1"
"#,
    )
    .expect("load");
    let rendered = toml::to_string(&config.provider["p"]).expect("render");
    for present in [
        "timeout_s",
        "protocol",
        "session_header",
        "default_query",
        "client_options",
        "capabilities",
        "request_options",
    ] {
        assert!(
            rendered.contains(present),
            "{present} was set and must be written back: {rendered}"
        );
    }
}
