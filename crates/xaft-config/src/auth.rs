//! `[auth]` — the OAuth 2.0 account `xaft login` signs in to.
//!
//! agenthicc hard-codes its account service (`AGENTHICC_CLIENT_ID =
//! "agenthicc-cli"`, `https://agenthicc.ai/oauth/{authorize,token,revoke}`,
//! scope `openid profile agent:run`). xaft has no hosted service of its own,
//! so the same flow — authorization code with PKCE, a loopback redirect,
//! refresh with a 60-second buffer, revocation on logout — is pointed at
//! whichever provider this section names. With no endpoints configured,
//! `xaft login` explains what to set instead of failing obscurely.
//!
//! Read by `xaft_runtime::auth` (the flow and the token store), which backs
//! `xaft login` / `logout` / `whoami` and the TUI's `/login` / `/logout`.

use serde::{Deserialize, Serialize};

/// `[auth]` settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// The OAuth client id xaft presents (agenthicc: `agenthicc-cli`).
    pub client_id: String,
    /// Authorization endpoint the browser is sent to. Empty means no account
    /// provider is configured.
    pub authorize_url: String,
    /// Token endpoint: exchanges the authorization code and refreshes tokens.
    pub token_url: String,
    /// RFC 7009 revocation endpoint, called on logout. Empty skips revocation
    /// (the local tokens are still deleted).
    pub revoke_url: String,
    /// OpenID Connect userinfo endpoint, used to learn the account's email
    /// and id when the token response does not carry them. Optional.
    pub userinfo_url: String,
    /// Scopes requested at login. Add `offline_access` for providers that
    /// only issue refresh tokens on request.
    pub scopes: Vec<String>,
    /// Loopback port for the redirect (`http://127.0.0.1:<port>/callback`).
    /// `0` picks a free port per login; set a fixed one when the provider
    /// requires the redirect URI to be registered exactly.
    pub redirect_port: u16,
    /// Seconds to wait for the browser to return (agenthicc: 120).
    pub login_timeout_s: u64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            client_id: "xaft-cli".to_string(),
            authorize_url: String::new(),
            token_url: String::new(),
            revoke_url: String::new(),
            userinfo_url: String::new(),
            scopes: vec!["openid".into(), "profile".into(), "email".into()],
            redirect_port: 0,
            login_timeout_s: 120,
        }
    }
}

/// Is `url` acceptable for an endpoint that receives codes or tokens:
/// `https://`, or plain `http://` to a loopback host only.
fn secure_endpoint(url: &str) -> bool {
    if let Some(rest) = url.strip_prefix("https://") {
        return !rest.is_empty();
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => authority.rsplit_once(':').map_or(authority, |(h, _)| h),
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

impl AuthConfig {
    /// Whether a provider is configured (both the authorize and the token
    /// endpoint are set).
    pub fn is_configured(&self) -> bool {
        !self.authorize_url.trim().is_empty() && !self.token_url.trim().is_empty()
    }

    /// Validate the section. Returns the first problem found, naming its key.
    pub fn validate(&self) -> Result<(), String> {
        let authorize = self.authorize_url.trim();
        let token = self.token_url.trim();
        if authorize.is_empty() != token.is_empty() {
            return Err("auth.authorize_url and auth.token_url must be set together".to_string());
        }
        for (key, url) in [
            ("authorize_url", authorize),
            ("token_url", token),
            ("revoke_url", self.revoke_url.trim()),
            ("userinfo_url", self.userinfo_url.trim()),
        ] {
            if !url.is_empty() && !secure_endpoint(url) {
                return Err(format!(
                    "auth.{key} must be an https:// URL (plain http:// is accepted only for \
                     127.0.0.1, localhost and ::1), got {url:?}"
                ));
            }
        }
        if self.is_configured() && self.client_id.trim().is_empty() {
            return Err("auth.client_id must not be empty".to_string());
        }
        if self.login_timeout_s == 0 {
            return Err("auth.login_timeout_s must be positive".to_string());
        }
        if self
            .scopes
            .iter()
            .any(|s| s.trim().is_empty() || s.contains(char::is_whitespace))
        {
            return Err("auth.scopes entries must be single, non-empty scope names".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_valid_and_unconfigured() {
        let c = AuthConfig::default();
        assert!(c.validate().is_ok());
        assert!(!c.is_configured());
        assert_eq!(c.client_id, "xaft-cli");
        assert_eq!(c.login_timeout_s, 120);
    }

    #[test]
    fn endpoints_must_be_https_or_loopback() {
        let ok = AuthConfig {
            authorize_url: "https://id.example.com/authorize".into(),
            token_url: "http://127.0.0.1:8080/token".into(),
            revoke_url: "http://localhost/revoke".into(),
            userinfo_url: "http://[::1]:9/userinfo".into(),
            ..AuthConfig::default()
        };
        assert!(ok.validate().is_ok(), "{:?}", ok.validate());
        assert!(ok.is_configured());

        let plain = AuthConfig {
            token_url: "http://id.example.com/token".into(),
            ..ok.clone()
        };
        let err = plain.validate().unwrap_err();
        assert!(err.starts_with("auth.token_url"), "{err}");
        let sneaky = AuthConfig {
            token_url: "http://127.0.0.1.evil.example/token".into(),
            ..ok.clone()
        };
        assert!(sneaky.validate().is_err());
    }

    #[test]
    fn half_a_provider_and_bad_values_are_rejected() {
        let half = AuthConfig {
            authorize_url: "https://id.example.com/authorize".into(),
            ..AuthConfig::default()
        };
        assert!(half.validate().unwrap_err().contains("set together"));
        let no_client = AuthConfig {
            authorize_url: "https://a.example/authorize".into(),
            token_url: "https://a.example/token".into(),
            client_id: " ".into(),
            ..AuthConfig::default()
        };
        assert!(no_client.validate().unwrap_err().contains("client_id"));
        let zero = AuthConfig {
            login_timeout_s: 0,
            ..AuthConfig::default()
        };
        assert!(zero.validate().is_err());
        let spaced = AuthConfig {
            scopes: vec!["openid profile".into()],
            ..AuthConfig::default()
        };
        assert!(spaced.validate().is_err());
    }

    #[test]
    fn a_partial_table_keeps_the_other_defaults() {
        let parsed: AuthConfig = toml::from_str("token_url = \"https://a.example/t\"\n").unwrap();
        assert_eq!(parsed.token_url, "https://a.example/t");
        assert_eq!(parsed.client_id, "xaft-cli");
        assert_eq!(parsed.scopes, ["openid", "profile", "email"]);
    }
}
