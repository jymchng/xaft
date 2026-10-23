//! Account sign-in: OAuth 2.0 authorization code + PKCE (parity rows 54-56).
//!
//! A port of agenthicc's `auth.py` (`AuthClient`, `TokenStore`,
//! `TokenBundle`) and of the `login` / `logout` / `whoami` commands built on
//! it. The flow is agenthicc's:
//!
//! 1. A PKCE verifier (64 random bytes, URL-safe base64) and its S256
//!    challenge are generated, and a loopback listener is bound.
//! 2. The browser is opened at the authorize URL (`client_id`,
//!    `redirect_uri`, `response_type=code`, `code_challenge`,
//!    `code_challenge_method=S256`, `scope`); the URL is printed too, for a
//!    machine without a browser.
//! 3. The provider redirects to `http://127.0.0.1:<port>/callback?code=…`;
//!    the code is exchanged at the token endpoint with the verifier.
//! 4. The [`TokenBundle`] (`access_token`, `refresh_token`, `expires_at`,
//!    `plan`, `email`, `user_id`) is stored by [`TokenStore`].
//!
//! A token within 60 seconds of expiry is refreshed before use
//! ([`AuthClient::access_token`]); logout revokes at the provider
//! (best-effort) and deletes the local tokens.
//!
//! # Where it differs from agenthicc, and why
//!
//! - **The provider is configured, not hard-coded** (`[auth]`, see
//!   [`xaft_config::AuthConfig`]): xaft has no hosted account service of its
//!   own. Unconfigured, `login` says which keys to set.
//! - **The flow sends and checks `state`.** agenthicc omits it; RFC 6749
//!   §10.12 asks for it, and it costs nothing. A callback with the wrong state
//!   ends the login instead of being trusted.
//! - **An `error=` callback ends the login at once** with the provider's
//!   reason; agenthicc ignores it and waits out its 120-second timeout.
//! - **The redirect uses `127.0.0.1`**, not `localhost` (RFC 8252 §7.3).
//! - **Logout revokes the refresh token as well as the access token**, so a
//!   copied token file cannot mint new access tokens after `xaft logout`.
//! - **The account email falls back** to the ID token's claims and the
//!   configured userinfo endpoint when the token response does not carry it,
//!   as standard OpenID Connect providers do not.
//! - **`whoami` refreshes an expired token** before printing it, so the
//!   expiry it shows is current; agenthicc prints the stored one.
//! - **Tokens live in `$XAFT_HOME/auth.json` (0600)**; agenthicc prefers the
//!   OS keyring. See [`store`]. agenthicc's cached account tier
//!   (`account-tier.json`) is not written: it exists only to decide whether
//!   to show agenthicc.ai's sponsored messages, which xaft does not have.

pub mod store;

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use xaft_config::{AuthConfig, XaftConfig};

pub use store::{TOKEN_FILE, TokenStore, xaft_home};

/// agenthicc's `TOKEN_REFRESH_BUFFER`: refresh this many seconds early.
pub const TOKEN_REFRESH_BUFFER_S: f64 = 60.0;
/// agenthicc's timeout for the code exchange and refresh calls.
const TOKEN_CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// agenthicc's timeout for the revocation call.
const REVOKE_TIMEOUT: Duration = Duration::from_secs(10);
/// What the browser shows once the code has arrived (agenthicc's text).
const SUCCESS_PAGE: &str = "Login successful! You can close this tab.";

/// Why an account operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    /// `[auth]` names no provider.
    #[error(
        "no account provider is configured: set [auth] authorize_url and token_url (and \
         client_id) in your xaft config — see docs/guides/auth.md"
    )]
    NotConfigured,
    /// agenthicc's `NotLoggedInError`.
    #[error("Not logged in. Run: xaft login")]
    NotLoggedIn,
    /// agenthicc's `AuthNetworkError`: the provider could not be reached.
    #[error("{0}")]
    Network(String),
    /// The provider answered with an error.
    #[error("{0}")]
    Rejected(String),
    /// The provider sent the browser back with `error=`.
    #[error("the sign-in was not completed: {0}")]
    Denied(String),
    /// A callback arrived for a different login attempt.
    #[error(
        "the login callback does not belong to this login (state mismatch); run xaft login again"
    )]
    StateMismatch,
    /// No callback within `[auth] login_timeout_s`.
    #[error("no login callback arrived within {0}s; run xaft login again")]
    Timeout(u64),
    /// The loopback listener could not be set up.
    #[error("cannot listen for the login callback: {0}")]
    Listener(String),
    /// The token file could not be read or written.
    #[error("token store: {0}")]
    Store(String),
}

/// The stored account tokens — agenthicc's `TokenBundle`, field for field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenBundle {
    /// Bearer token for the account's API.
    pub access_token: String,
    /// Token that mints new access tokens; empty when the provider issued none.
    pub refresh_token: String,
    /// Expiry as a Unix timestamp, seconds.
    pub expires_at: f64,
    /// `free`, `pro`, `enterprise` — whatever the provider reports.
    pub plan: String,
    /// The account's email.
    pub email: String,
    /// The account's id.
    pub user_id: String,
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

impl TokenBundle {
    /// agenthicc's `is_expired`: within [`TOKEN_REFRESH_BUFFER_S`] of expiry.
    pub fn is_expired(&self) -> bool {
        self.is_expired_at(now())
    }

    /// [`Self::is_expired`] at a given time.
    pub fn is_expired_at(&self, now: f64) -> bool {
        now >= self.expires_at - TOKEN_REFRESH_BUFFER_S
    }

    /// agenthicc's `is_pro`.
    pub fn is_pro(&self) -> bool {
        matches!(self.plan.as_str(), "pro" | "enterprise")
    }

    /// The name to greet: the email, else the id.
    pub fn display_name(&self) -> &str {
        if !self.email.is_empty() {
            &self.email
        } else if !self.user_id.is_empty() {
            &self.user_id
        } else {
            "(unknown account)"
        }
    }

    /// agenthicc's `whoami` line: `{email}  plan={plan}  token_expires={exp}`,
    /// the expiry in local time as `%Y-%m-%d %H:%M`.
    pub fn whoami_line(&self) -> String {
        let expires = chrono::DateTime::from_timestamp(self.expires_at as i64, 0)
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| "?".to_string());
        format!(
            "{}  plan={}  token_expires={expires}",
            self.display_name(),
            self.plan
        )
    }
}

/// Percent-encode a query or form value (RFC 3986 unreserved kept).
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = bytes
                    .get(i + 1..i + 3)
                    .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(v) => {
                        out.push(v);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn random_url_safe(bytes: usize) -> Result<String, AuthError> {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf)
        .map_err(|e| AuthError::Listener(format!("no secure randomness available: {e}")))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf))
}

/// The S256 PKCE challenge for `verifier` (RFC 7636 §4.2).
pub fn pkce_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// The claims of a JWT's payload, unverified — for display only (the
/// account's email and id), never for an authorization decision.
fn unverified_claims(jwt: &str) -> Option<Value> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn text_field(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Signs in to, refreshes and signs out of the configured account.
#[derive(Debug, Clone)]
pub struct AuthClient {
    config: AuthConfig,
    store: TokenStore,
    http: reqwest::Client,
}

/// What `whoami` found.
#[derive(Debug, Clone, PartialEq)]
pub enum WhoAmI {
    /// Nobody is signed in.
    SignedOut,
    /// The signed-in account, refreshed first if its token had expired.
    SignedIn(TokenBundle),
    /// The token has expired and could not be refreshed: the stored bundle
    /// and why.
    Expired(TokenBundle, AuthError),
}

/// What `logout` did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogoutOutcome {
    /// Tokens were stored and have been deleted.
    pub had_tokens: bool,
    /// Tokens the provider accepted for revocation (`refresh_token`,
    /// `access_token`).
    pub revoked: Vec<&'static str>,
}

impl AuthClient {
    /// A client for `config`'s provider over `store`.
    pub fn new(config: &AuthConfig, store: TokenStore) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("xaft/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Self {
            config: config.clone(),
            store,
            http,
        }
    }

    /// A client for `config.auth` over `$XAFT_HOME/auth.json`.
    pub fn from_config(config: &XaftConfig) -> Result<Self, AuthError> {
        Ok(Self::new(&config.auth, TokenStore::default_location()?))
    }

    /// Whether a provider is configured.
    pub fn is_configured(&self) -> bool {
        self.config.is_configured()
    }

    /// The token store this client reads and writes.
    pub fn store(&self) -> &TokenStore {
        &self.store
    }

    /// agenthicc's `current_bundle`: the stored tokens, as they are.
    pub fn current_bundle(&self) -> Option<TokenBundle> {
        self.store.load()
    }

    /// Prepare a login: generate PKCE and `state`, bind the loopback
    /// listener, and build the authorize URL. Nothing is sent yet.
    pub fn start_login(&self) -> Result<PendingLogin, AuthError> {
        if !self.is_configured() {
            return Err(AuthError::NotConfigured);
        }
        let verifier = random_url_safe(64)?;
        let state = random_url_safe(24)?;
        let listener = std::net::TcpListener::bind(SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            self.config.redirect_port,
        )))
        .and_then(|l| l.set_nonblocking(true).map(|()| l))
        .map_err(|e| {
            AuthError::Listener(format!("127.0.0.1:{}: {e}", self.config.redirect_port))
        })?;
        let port = listener
            .local_addr()
            .map_err(|e| AuthError::Listener(e.to_string()))?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        let scope = self
            .config
            .scopes
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        let base = self.config.authorize_url.trim();
        let separator = if base.contains('?') { '&' } else { '?' };
        let authorize_url = format!(
            "{base}{separator}client_id={}&redirect_uri={}&response_type=code&code_challenge={}\
             &code_challenge_method=S256&scope={}&state={}",
            encode(&self.config.client_id),
            encode(&redirect_uri),
            pkce_challenge(&verifier),
            encode(&scope),
            encode(&state),
        );
        Ok(PendingLogin {
            client: self.clone(),
            authorize_url,
            redirect_uri,
            verifier,
            state,
            listener,
            timeout: Duration::from_secs(self.config.login_timeout_s),
        })
    }

    /// agenthicc's `get_token`: a valid access token, refreshed (and stored)
    /// first when it is within the refresh buffer of expiry.
    pub async fn access_token(&self) -> Result<String, AuthError> {
        let bundle = self.store.load().ok_or(AuthError::NotLoggedIn)?;
        if !bundle.is_expired() {
            return Ok(bundle.access_token);
        }
        let fresh = self.refresh(&bundle).await?;
        self.store.save(&fresh)?;
        Ok(fresh.access_token)
    }

    /// The signed-in account for `whoami`: the stored bundle, refreshed first
    /// when it has expired.
    pub async fn whoami(&self) -> WhoAmI {
        let Some(bundle) = self.store.load() else {
            return WhoAmI::SignedOut;
        };
        if !bundle.is_expired() {
            return WhoAmI::SignedIn(bundle);
        }
        match self.refresh(&bundle).await {
            Ok(fresh) => match self.store.save(&fresh) {
                Ok(()) => WhoAmI::SignedIn(fresh),
                Err(e) => WhoAmI::Expired(bundle, e),
            },
            Err(e) => WhoAmI::Expired(bundle, e),
        }
    }

    /// agenthicc's `logout`: revoke at the provider (best-effort), then delete
    /// the local tokens.
    pub async fn logout(&self) -> Result<LogoutOutcome, AuthError> {
        let mut outcome = LogoutOutcome::default();
        if let Some(bundle) = self.store.load() {
            outcome.had_tokens = true;
            if !bundle.refresh_token.is_empty()
                && self.revoke(&bundle.refresh_token, "refresh_token").await
            {
                outcome.revoked.push("refresh_token");
            }
            if self.revoke(&bundle.access_token, "access_token").await {
                outcome.revoked.push("access_token");
            }
        }
        self.store.clear()?;
        Ok(outcome)
    }

    async fn post_form(
        &self,
        url: &str,
        form: &[(&str, &str)],
        what: &str,
    ) -> Result<Value, AuthError> {
        let response = self
            .http
            .post(url)
            .timeout(TOKEN_CALL_TIMEOUT)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(form)
            .send()
            .await
            .map_err(|e| network_error(what, &e))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|e| network_error(what, &e))?;
        let json: Option<Value> = serde_json::from_slice(&body).ok();
        if !status.is_success() {
            // RFC 6749 §5.2: `error` and `error_description`.
            let detail = json
                .as_ref()
                .map(|v| {
                    [text_field(v, "error"), text_field(v, "error_description")]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(": ")
                })
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| status.to_string());
            return Err(AuthError::Rejected(format!(
                "{what} was rejected by the auth server ({detail}). Run xaft login again."
            )));
        }
        json.ok_or_else(|| {
            AuthError::Rejected(format!("{what}: the auth server did not answer with JSON"))
        })
    }

    async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenBundle, AuthError> {
        let data = self
            .post_form(
                &self.config.token_url,
                &[
                    ("grant_type", "authorization_code"),
                    ("client_id", &self.config.client_id),
                    ("code", code),
                    ("code_verifier", verifier),
                    ("redirect_uri", redirect_uri),
                ],
                "Auth request",
            )
            .await?;
        let access_token = text_field(&data, "access_token").ok_or_else(|| {
            AuthError::Rejected("the auth server's token response has no access_token".into())
        })?;
        let mut bundle = TokenBundle {
            refresh_token: text_field(&data, "refresh_token").unwrap_or_default(),
            expires_at: now()
                + data
                    .get("expires_in")
                    .and_then(Value::as_f64)
                    .unwrap_or(3600.0),
            plan: text_field(&data, "plan").unwrap_or_else(|| "free".to_string()),
            email: text_field(&data, "email").unwrap_or_default(),
            user_id: text_field(&data, "user_id").unwrap_or_default(),
            access_token,
        };
        self.fill_identity(&mut bundle, &data).await;
        Ok(bundle)
    }

    /// Standard OpenID Connect providers put the account's identity in the
    /// ID token or behind the userinfo endpoint, not in the token response.
    async fn fill_identity(&self, bundle: &mut TokenBundle, token_response: &Value) {
        let mut sources: Vec<Value> = Vec::new();
        if let Some(claims) = token_response
            .get("id_token")
            .and_then(Value::as_str)
            .and_then(unverified_claims)
        {
            sources.push(claims);
        }
        let incomplete = |b: &TokenBundle| b.email.is_empty() || b.user_id.is_empty();
        let apply = |b: &mut TokenBundle, claims: &Value| {
            if b.email.is_empty() {
                b.email = text_field(claims, "email").unwrap_or_default();
            }
            if b.user_id.is_empty() {
                b.user_id = text_field(claims, "user_id")
                    .or_else(|| text_field(claims, "sub"))
                    .unwrap_or_default();
            }
            if token_response.get("plan").is_none() {
                if let Some(plan) = text_field(claims, "plan") {
                    b.plan = plan;
                }
            }
        };
        for claims in &sources {
            apply(bundle, claims);
        }
        let userinfo = self.config.userinfo_url.trim();
        if incomplete(bundle) && !userinfo.is_empty() {
            if let Ok(response) = self
                .http
                .get(userinfo)
                .timeout(TOKEN_CALL_TIMEOUT)
                .bearer_auth(&bundle.access_token)
                .send()
                .await
            {
                if response.status().is_success() {
                    if let Ok(claims) = response.json::<Value>().await {
                        apply(bundle, &claims);
                    }
                }
            }
        }
    }

    /// agenthicc's `_refresh`: a new access token, keeping the refresh token,
    /// plan, email and id when the response omits them.
    async fn refresh(&self, bundle: &TokenBundle) -> Result<TokenBundle, AuthError> {
        if !self.is_configured() {
            return Err(AuthError::NotConfigured);
        }
        if bundle.refresh_token.is_empty() {
            return Err(AuthError::Rejected(
                "the session has expired and the provider issued no refresh token; run xaft login"
                    .into(),
            ));
        }
        let data = self
            .post_form(
                &self.config.token_url,
                &[
                    ("grant_type", "refresh_token"),
                    ("client_id", &self.config.client_id),
                    ("refresh_token", &bundle.refresh_token),
                ],
                "Token refresh",
            )
            .await?;
        let access_token = text_field(&data, "access_token").ok_or_else(|| {
            AuthError::Rejected("the auth server's refresh response has no access_token".into())
        })?;
        Ok(TokenBundle {
            access_token,
            refresh_token: text_field(&data, "refresh_token")
                .unwrap_or_else(|| bundle.refresh_token.clone()),
            expires_at: now()
                + data
                    .get("expires_in")
                    .and_then(Value::as_f64)
                    .unwrap_or(3600.0),
            plan: text_field(&data, "plan").unwrap_or_else(|| bundle.plan.clone()),
            email: bundle.email.clone(),
            user_id: bundle.user_id.clone(),
        })
    }

    /// agenthicc's `_revoke`: best-effort; `true` when the provider accepted it.
    async fn revoke(&self, token: &str, hint: &str) -> bool {
        let url = self.config.revoke_url.trim();
        if url.is_empty() {
            return false;
        }
        self.http
            .post(url)
            .timeout(REVOKE_TIMEOUT)
            .form(&[
                ("token", token),
                ("token_type_hint", hint),
                ("client_id", self.config.client_id.as_str()),
            ])
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }
}

/// agenthicc's `AuthNetworkError` wording.
fn network_error(what: &str, err: &reqwest::Error) -> AuthError {
    let class = if err.is_timeout() {
        "ReadTimeout"
    } else if err.is_connect() {
        "ConnectError"
    } else {
        "RequestError"
    };
    if err.is_timeout() {
        let subject = if what == "Token refresh" {
            "Token refresh"
        } else {
            "Auth server"
        };
        AuthError::Network(format!(
            "{subject} timed out ({class}). Check your connection and try again."
        ))
    } else {
        AuthError::Network(format!(
            "{what} failed ({class}: {err}). Check your connection and try again."
        ))
    }
}

/// A login waiting for the browser.
#[derive(Debug)]
pub struct PendingLogin {
    client: AuthClient,
    authorize_url: String,
    redirect_uri: String,
    verifier: String,
    state: String,
    listener: std::net::TcpListener,
    timeout: Duration,
}

impl PendingLogin {
    /// The URL the user signs in at.
    pub fn authorize_url(&self) -> &str {
        &self.authorize_url
    }

    /// The loopback redirect URI the provider will call.
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Seconds the login waits for the browser.
    pub fn timeout_secs(&self) -> u64 {
        self.timeout.as_secs()
    }

    /// Wait for the callback, exchange the code and store the tokens.
    pub async fn complete(self) -> Result<TokenBundle, AuthError> {
        let listener = tokio::net::TcpListener::from_std(self.listener)
            .map_err(|e| AuthError::Listener(e.to_string()))?;
        let code = tokio::time::timeout(self.timeout, wait_for_code(&listener, &self.state))
            .await
            .map_err(|_| AuthError::Timeout(self.timeout.as_secs()))??;
        drop(listener);
        let bundle = self
            .client
            .exchange_code(&code, &self.verifier, &self.redirect_uri)
            .await?;
        self.client.store.save(&bundle)?;
        Ok(bundle)
    }
}

/// Serve the loopback redirect until a request to `/callback` decides the
/// login. Other paths (`/favicon.ico`, scanners) get a 404 and are ignored.
async fn wait_for_code(
    listener: &tokio::net::TcpListener,
    expected_state: &str,
) -> Result<String, AuthError> {
    loop {
        let (mut socket, _) = listener
            .accept()
            .await
            .map_err(|e| AuthError::Listener(e.to_string()))?;
        let mut buf = Vec::with_capacity(2048);
        let mut chunk = [0u8; 2048];
        // The request line is all that matters; cap what is read.
        let read = tokio::time::timeout(Duration::from_secs(5), async {
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
                match socket.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
        })
        .await;
        if read.is_err() {
            continue;
        }
        let head = String::from_utf8_lossy(&buf);
        let target = head
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("");
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        if path != "/callback" {
            respond(&mut socket, "404 Not Found", "Not found.").await;
            continue;
        }
        let params: Vec<(String, String)> = query
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                (decode(k), decode(v))
            })
            .collect();
        let get = |key: &str| {
            params
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        if let Some(error) = get("error") {
            let reason = match get("error_description") {
                Some(d) if !d.is_empty() => format!("{error}: {d}"),
                _ => error,
            };
            respond(
                &mut socket,
                "400 Bad Request",
                &format!("Login failed: {reason}"),
            )
            .await;
            return Err(AuthError::Denied(reason));
        }
        if get("state").as_deref() != Some(expected_state) {
            respond(
                &mut socket,
                "400 Bad Request",
                "Login failed: state mismatch.",
            )
            .await;
            return Err(AuthError::StateMismatch);
        }
        match get("code").filter(|c| !c.is_empty()) {
            Some(code) => {
                respond(&mut socket, "200 OK", SUCCESS_PAGE).await;
                return Ok(code);
            }
            None => respond(&mut socket, "400 Bad Request", "Missing code.").await,
        }
    }
}

async fn respond(socket: &mut tokio::net::TcpStream, status: &str, text: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n{text}",
        text.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;
}

/// Open `url` in the user's browser (agenthicc's `webbrowser.open`):
/// `$BROWSER` when set, else the platform opener. Best-effort; `false` when
/// nothing could be started, in which case the printed URL is the way in.
pub fn open_browser(url: &str) -> bool {
    use std::process::{Command, Stdio};
    let spawn = |program: &str, args: &[&str]| {
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    };
    if let Some(browser) = std::env::var_os("BROWSER").filter(|b| !b.is_empty()) {
        let browser = browser.to_string_lossy().into_owned();
        if spawn(&browser, &[url]) {
            return true;
        }
    }
    if cfg!(target_os = "macos") {
        spawn("open", &[url])
    } else if cfg!(windows) {
        // `start` would split the URL at its `&`s; the URL handler does not.
        spawn("rundll32", &["url.dll,FileProtocolHandler", url])
    } else {
        spawn("xdg-open", &[url])
    }
}

#[cfg(test)]
mod tests;
