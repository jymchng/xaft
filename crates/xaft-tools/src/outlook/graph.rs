//! The Microsoft Graph mail and calendar client behind the Outlook tools.
//!
//! A port of agenthicc's `GraphApiOutlookBackend` (`tools/outlook/__init__.py`):
//! the same nine calls against `https://graph.microsoft.com/v1.0`, the same
//! `$select` lists, the same bearer token from `MSGRAPH_TOKEN`, and the same
//! result shapes. agenthicc's HTTP client (`agenthicc_http_client`) uses
//! `[tools] http_timeout_s` as the read timeout and a fixed 10 s connect
//! timeout; so does this one.
//!
//! # Where it deliberately differs
//!
//! - **Path segments and query values are percent-encoded.** agenthicc
//!   interpolates `folder` and `email_id` into the URL path verbatim, so an id
//!   containing `/` or `?` addresses a different resource. Graph ids and
//!   well-known folder names survive encoding unchanged.
//! - **An unset token is reported, not sent.** agenthicc sends `Bearer ` and
//!   reports Graph's 401; here the call stops before the network with the same
//!   recoverable envelope and a sentence naming `MSGRAPH_TOKEN`.
//! - **Attachments are attached.** agenthicc's Graph backend accepts
//!   `attachments` and silently drops them (only its Windows COM backend
//!   attaches files). Here each path — confined to the workspace — is sent as
//!   a Graph `fileAttachment`, up to Graph's inline limit.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};

/// agenthicc's `GRAPH_BASE`.
pub const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";
/// The environment variable agenthicc reads the Graph token from.
pub const TOKEN_ENV: &str = "MSGRAPH_TOKEN";
/// agenthicc's fixed connect timeout (`tools/http.py::_connect_timeout`).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// agenthicc's default `[tools] http_timeout_s`.
pub const DEFAULT_READ_TIMEOUT_S: f64 = 30.0;
/// Graph accepts at most 4 MB per request; base64 inflates by a third, so the
/// raw attachments of one message are capped below that.
pub const MAX_ATTACHMENT_BYTES: u64 = 3 * 1024 * 1024;
/// Graph's largest `$top` for message collections.
pub const MAX_TOP: u64 = 1000;

/// Why a Graph call produced no result.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutlookError {
    /// agenthicc's `_OutlookNetworkError` class — a transport failure, a
    /// timeout, or an HTTP error status (agenthicc's `is_network_error` counts
    /// `httpx.HTTPStatusError` too). The tools turn it into
    /// `{"ok": false, "error": ..., "recoverable": true}`.
    #[error("{0}")]
    Network(String),
    /// Graph answered 2xx with a body that is not the documented JSON.
    #[error("unexpected Microsoft Graph response: {0}")]
    Protocol(String),
    /// The call itself is malformed (an attachment outside the workspace, …).
    #[error("{0}")]
    Input(String),
}

impl OutlookError {
    /// Does agenthicc's `_safe_call` convert this into a recoverable result?
    pub fn is_recoverable(&self) -> bool {
        matches!(self, Self::Network(_))
    }
}

/// Where the bearer token comes from.
#[derive(Debug, Clone)]
pub enum TokenSource {
    /// Read the named environment variable on every call, as agenthicc builds
    /// a fresh backend from `os.getenv("MSGRAPH_TOKEN")` per call — so a token
    /// exported after start-up is picked up.
    Env(String),
    /// A fixed token (tests, embedders).
    Fixed(String),
}

impl TokenSource {
    fn resolve(&self) -> Option<String> {
        let token = match self {
            Self::Env(name) => std::env::var(name).ok()?,
            Self::Fixed(token) => token.clone(),
        };
        let token = token.trim().to_string();
        (!token.is_empty()).then_some(token)
    }

    /// Why no token is available, and how to provide one.
    fn missing(&self) -> String {
        let scopes = "scopes Mail.ReadWrite, Mail.Send, Calendars.ReadWrite";
        match self {
            Self::Env(name) => format!(
                "{name} is not set; export a Microsoft Graph access token ({scopes}) in {name} \
                 to use the Outlook tools"
            ),
            Self::Fixed(_) => format!("no Microsoft Graph access token ({scopes}) was configured"),
        }
    }
}

/// A Microsoft Graph mail and calendar client.
#[derive(Debug, Clone)]
pub struct GraphOutlookBackend {
    base_url: String,
    token: TokenSource,
    read_timeout: Option<Duration>,
    workspace_root: PathBuf,
}

/// Percent-encode everything but RFC 3986's unreserved characters.
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

/// `/a/{b}/c` with each dynamic segment encoded.
fn path(segments: &[&str]) -> String {
    segments.iter().fold(String::new(), |mut acc, seg| {
        acc.push('/');
        acc.push_str(seg);
        acc
    })
}

fn query(pairs: &[(&str, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn str_at<'a>(value: &'a Value, pointer: &str) -> &'a str {
    value.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}

fn addresses(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|r| str_at(r, "/emailAddress/address").to_string())
        .collect()
}

fn items(data: &Value) -> Result<&Vec<Value>, OutlookError> {
    data.get("value")
        .and_then(Value::as_array)
        .ok_or_else(|| OutlookError::Protocol("no `value` array in the collection".into()))
}

fn recipients(addresses: &[String]) -> Value {
    Value::Array(
        addresses
            .iter()
            .map(|a| json!({"emailAddress": {"address": a}}))
            .collect(),
    )
}

impl GraphOutlookBackend {
    /// agenthicc's default backend: Graph v1.0, `MSGRAPH_TOKEN`, a 30 s read
    /// timeout, attachments resolved under `workspace_root`.
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            base_url: GRAPH_BASE.to_string(),
            token: TokenSource::Env(TOKEN_ENV.to_string()),
            read_timeout: Some(Duration::from_secs_f64(DEFAULT_READ_TIMEOUT_S)),
            workspace_root: workspace_root.into(),
        }
    }

    /// Use another Graph root (a national cloud, or a test server).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    /// Take the bearer token from `source`.
    pub fn with_token(mut self, source: TokenSource) -> Self {
        self.token = source;
        self
    }

    /// `[tools] http_timeout_s`: the read timeout; `0` (or less) disables it,
    /// as agenthicc's `configure(0.0)` does.
    pub fn with_read_timeout_s(mut self, seconds: f64) -> Self {
        self.read_timeout =
            (seconds.is_finite() && seconds > 0.0).then(|| Duration::from_secs_f64(seconds));
        self
    }

    /// The read timeout in force (`None` = unbounded).
    pub fn read_timeout(&self) -> Option<Duration> {
        self.read_timeout
    }

    fn client(&self) -> Result<reqwest::Client, OutlookError> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(concat!("xaft/", env!("CARGO_PKG_VERSION")));
        if let Some(t) = self.read_timeout {
            builder = builder.read_timeout(t);
        }
        builder
            .build()
            .map_err(|e| OutlookError::Network(format!("ClientError: {e}")))
    }

    fn token(&self) -> Result<String, OutlookError> {
        self.token
            .resolve()
            .ok_or_else(|| OutlookError::Network(self.token.missing()))
    }

    /// agenthicc's `f"{type(exc).__name__}: {exc}"`, from a reqwest error.
    fn transport_error(err: &reqwest::Error) -> OutlookError {
        let kind = if err.is_timeout() {
            "Timeout"
        } else if err.is_connect() {
            "ConnectError"
        } else if err.is_decode() || err.is_body() {
            "ReadError"
        } else {
            "RequestError"
        };
        OutlookError::Network(format!("{kind}: {err}"))
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        url: &str,
    ) -> Result<Value, OutlookError> {
        let token = self.token()?;
        let response = request
            .bearer_auth(token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| Self::transport_error(&e))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|e| Self::transport_error(&e))?;
        if !status.is_success() {
            // agenthicc: `HTTPStatusError: Client error '401 Unauthorized' for
            // url '…'`; Graph's own `error.code`/`error.message` is appended
            // because it is the part that says what to fix.
            let detail = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| {
                    let code = str_at(&v, "/error/code").to_string();
                    let message = str_at(&v, "/error/message").to_string();
                    (!code.is_empty() || !message.is_empty()).then(|| {
                        format!(
                            ": {}",
                            [code, message]
                                .iter()
                                .filter(|s| !s.is_empty())
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(": ")
                        )
                    })
                })
                .unwrap_or_default();
            let class = if status.is_client_error() {
                "Client error"
            } else {
                "Server error"
            };
            return Err(OutlookError::Network(format!(
                "HTTPStatusError: {class} '{status}' for url '{url}'{detail}"
            )));
        }
        if body.is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_slice(&body).map_err(|e| OutlookError::Protocol(e.to_string()))
    }

    async fn get(&self, path_and_query: &str) -> Result<Value, OutlookError> {
        let url = format!("{}{}", self.base_url, path_and_query);
        let client = self.client()?;
        self.send(client.get(&url), &url).await
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value, OutlookError> {
        let url = format!("{}{}", self.base_url, path);
        let client = self.client()?;
        self.send(client.post(&url).json(body), &url).await
    }

    /// `list_emails` — `[{id, subject, from, date, unread, body_preview}]`.
    pub async fn list_emails(
        &self,
        folder: &str,
        n: u64,
        unread_only: bool,
    ) -> Result<Value, OutlookError> {
        let mut params = vec![
            ("$top", n.clamp(1, MAX_TOP).to_string()),
            (
                "$select",
                "id,subject,from,toRecipients,receivedDateTime,isRead,bodyPreview".to_string(),
            ),
        ];
        if unread_only {
            params.push(("$filter", "isRead eq false".to_string()));
        }
        let data = self
            .get(&format!(
                "{}?{}",
                path(&["me", "mailFolders", &encode(folder), "messages"]),
                query(&params)
            ))
            .await?;
        Ok(Value::Array(
            items(&data)?
                .iter()
                .map(|m| {
                    json!({
                        "id": str_at(m, "/id"),
                        "subject": str_at(m, "/subject"),
                        "from": str_at(m, "/from/emailAddress/address"),
                        "date": str_at(m, "/receivedDateTime"),
                        "unread": !m.get("isRead").and_then(Value::as_bool).unwrap_or(true),
                        "body_preview": str_at(m, "/bodyPreview"),
                    })
                })
                .collect(),
        ))
    }

    /// `read_email` — `{subject, from, to, cc, date, body_html, body_text}`.
    pub async fn read_email(&self, email_id: &str) -> Result<Value, OutlookError> {
        let m = self
            .get(&format!(
                "{}?{}",
                path(&["me", "messages", &encode(email_id)]),
                query(&[(
                    "$select",
                    "id,subject,from,toRecipients,ccRecipients,receivedDateTime,body".to_string()
                )])
            ))
            .await?;
        let content_type = str_at(&m, "/body/contentType").to_ascii_lowercase();
        let content = str_at(&m, "/body/content");
        Ok(json!({
            "subject": str_at(&m, "/subject"),
            "from": str_at(&m, "/from/emailAddress/address"),
            "to": addresses(&m, "toRecipients"),
            "cc": addresses(&m, "ccRecipients"),
            "date": str_at(&m, "/receivedDateTime"),
            "body_html": if content_type == "html" { content } else { "" },
            "body_text": if content_type == "text" { content } else { "" },
        }))
    }

    /// `send_email` — `{"ok": true, "message_id": ""}` (Graph's `sendMail`
    /// returns no id).
    pub async fn send_email(
        &self,
        to: &[String],
        subject: &str,
        body: &str,
        cc: &[String],
        attachments: &[String],
    ) -> Result<Value, OutlookError> {
        let mut message = json!({
            "subject": subject,
            "body": {"contentType": "Text", "content": body},
            "toRecipients": recipients(to),
            "ccRecipients": recipients(cc),
        });
        if !attachments.is_empty() {
            message["attachments"] = self.load_attachments(attachments)?;
        }
        self.post("/me/sendMail", &json!({"message": message}))
            .await?;
        Ok(json!({"ok": true, "message_id": ""}))
    }

    /// `reply_email` — `{"ok": true, "message_id": email_id}`.
    pub async fn reply_email(
        &self,
        email_id: &str,
        body: &str,
        reply_all: bool,
    ) -> Result<Value, OutlookError> {
        let endpoint = if reply_all { "replyAll" } else { "reply" };
        self.post(
            &path(&["me", "messages", &encode(email_id), endpoint]),
            &json!({"comment": body}),
        )
        .await?;
        Ok(json!({"ok": true, "message_id": email_id}))
    }

    /// `search_emails` — `[{id, subject, from, date, body_preview}]`.
    pub async fn search_emails(
        &self,
        query_text: &str,
        folder: &str,
        n: u64,
    ) -> Result<Value, OutlookError> {
        let data = self
            .get(&format!(
                "{}?{}",
                path(&["me", "mailFolders", &encode(folder), "messages"]),
                query(&[
                    ("$search", format!("\"{query_text}\"")),
                    ("$top", n.clamp(1, MAX_TOP).to_string()),
                    (
                        "$select",
                        "id,subject,from,receivedDateTime,bodyPreview".to_string()
                    ),
                ])
            ))
            .await?;
        Ok(Value::Array(
            items(&data)?
                .iter()
                .map(|m| {
                    json!({
                        "id": str_at(m, "/id"),
                        "subject": str_at(m, "/subject"),
                        "from": str_at(m, "/from/emailAddress/address"),
                        "date": str_at(m, "/receivedDateTime"),
                        "body_preview": str_at(m, "/bodyPreview"),
                    })
                })
                .collect(),
        ))
    }

    /// `move_email` — `{"ok": true}`.
    pub async fn move_email(
        &self,
        email_id: &str,
        destination: &str,
    ) -> Result<Value, OutlookError> {
        self.post(
            &path(&["me", "messages", &encode(email_id), "move"]),
            &json!({"destinationId": destination}),
        )
        .await?;
        Ok(json!({"ok": true}))
    }

    /// `list_folders` — `[{id, name, unread_count}]`.
    pub async fn list_folders(&self) -> Result<Value, OutlookError> {
        let data = self
            .get(&format!(
                "/me/mailFolders?{}",
                query(&[(
                    "$select",
                    "id,displayName,totalItemCount,unreadItemCount".to_string()
                )])
            ))
            .await?;
        Ok(Value::Array(
            items(&data)?
                .iter()
                .map(|f| {
                    json!({
                        "id": str_at(f, "/id"),
                        "name": str_at(f, "/displayName"),
                        "unread_count": f.get("unreadItemCount").and_then(Value::as_u64).unwrap_or(0),
                    })
                })
                .collect(),
        ))
    }

    /// `calendar_events` — `[{id, subject, start, end, location, attendees}]`.
    pub async fn calendar_events(
        &self,
        start_date: &str,
        end_date: &str,
    ) -> Result<Value, OutlookError> {
        let data = self
            .get(&format!(
                "/me/calendarView?{}",
                query(&[
                    ("startDateTime", start_date.to_string()),
                    ("endDateTime", end_date.to_string()),
                    (
                        "$select",
                        "id,subject,start,end,location,attendees".to_string()
                    ),
                ])
            ))
            .await?;
        Ok(Value::Array(
            items(&data)?
                .iter()
                .map(|e| {
                    json!({
                        "id": str_at(e, "/id"),
                        "subject": str_at(e, "/subject"),
                        "start": str_at(e, "/start/dateTime"),
                        "end": str_at(e, "/end/dateTime"),
                        "location": str_at(e, "/location/displayName"),
                        "attendees": addresses(e, "attendees"),
                    })
                })
                .collect(),
        ))
    }

    /// `create_event` — `{"ok": true, "event_id": …}`.
    pub async fn create_event(
        &self,
        subject: &str,
        start: &str,
        end: &str,
        attendees: &[String],
        body: Option<&str>,
    ) -> Result<Value, OutlookError> {
        let mut event = json!({
            "subject": subject,
            "start": {"dateTime": start, "timeZone": "UTC"},
            "end": {"dateTime": end, "timeZone": "UTC"},
            "attendees": attendees
                .iter()
                .map(|a| json!({"emailAddress": {"address": a}, "type": "required"}))
                .collect::<Vec<_>>(),
        });
        if let Some(body) = body.filter(|b| !b.is_empty()) {
            event["body"] = json!({"contentType": "Text", "content": body});
        }
        let result = self.post("/me/events", &event).await?;
        Ok(json!({"ok": true, "event_id": str_at(&result, "/id")}))
    }

    /// Read `paths` as Graph `fileAttachment`s.
    fn load_attachments(&self, paths: &[String]) -> Result<Value, OutlookError> {
        let mut total = 0u64;
        let mut out = Vec::with_capacity(paths.len());
        for raw in paths {
            let file = self.confine(raw)?;
            let meta = std::fs::metadata(&file)
                .map_err(|e| OutlookError::Input(format!("attachment {raw}: {e}")))?;
            if !meta.is_file() {
                return Err(OutlookError::Input(format!(
                    "attachment {raw} is not a regular file"
                )));
            }
            total += meta.len();
            if total > MAX_ATTACHMENT_BYTES {
                return Err(OutlookError::Input(format!(
                    "attachments exceed {} MiB, Microsoft Graph's limit for one message",
                    MAX_ATTACHMENT_BYTES / (1024 * 1024)
                )));
            }
            let bytes = std::fs::read(&file)
                .map_err(|e| OutlookError::Input(format!("attachment {raw}: {e}")))?;
            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| raw.clone());
            out.push(json!({
                "@odata.type": "#microsoft.graph.fileAttachment",
                "name": name,
                "contentBytes": base64::engine::general_purpose::STANDARD.encode(bytes),
            }));
        }
        Ok(Value::Array(out))
    }

    /// Resolve an attachment path inside the workspace.
    fn confine(&self, raw: &str) -> Result<PathBuf, OutlookError> {
        let candidate = Path::new(raw);
        if candidate
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(OutlookError::Input(format!(
                "attachment path must not contain '..': {raw}"
            )));
        }
        let resolved = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.workspace_root.join(candidate)
        };
        if !resolved.starts_with(&self.workspace_root) {
            return Err(OutlookError::Input(format!(
                "attachment is outside the workspace: {raw}"
            )));
        }
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_keeps_ids_and_neutralises_path_syntax() {
        assert_eq!(encode("AAMkAGI2-_.~"), "AAMkAGI2-_.~");
        assert_eq!(encode("a/b?c=d e"), "a%2Fb%3Fc%3Dd%20e");
        assert_eq!(encode("Ünï"), "%C3%9Cn%C3%AF");
        assert_eq!(
            query(&[("$filter", "isRead eq false".into())]),
            "$filter=isRead%20eq%20false"
        );
    }

    #[test]
    fn the_read_timeout_follows_http_timeout_s() {
        let b = GraphOutlookBackend::new("/w");
        assert_eq!(b.read_timeout(), Some(Duration::from_secs(30)));
        assert_eq!(
            b.clone().with_read_timeout_s(4.5).read_timeout(),
            Some(Duration::from_millis(4500))
        );
        assert_eq!(b.clone().with_read_timeout_s(0.0).read_timeout(), None);
        assert_eq!(b.with_read_timeout_s(f64::NAN).read_timeout(), None);
    }

    #[test]
    fn attachments_are_confined_and_bounded() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.txt"), b"hello").unwrap();
        let b = GraphOutlookBackend::new(tmp.path());
        let loaded = b.load_attachments(&["a.txt".into()]).unwrap();
        assert_eq!(loaded[0]["name"], json!("a.txt"));
        assert_eq!(loaded[0]["contentBytes"], json!("aGVsbG8="));
        assert_eq!(
            loaded[0]["@odata.type"],
            json!("#microsoft.graph.fileAttachment")
        );
        for bad in ["../x", "/etc/passwd", "missing.txt"] {
            assert!(
                matches!(
                    b.load_attachments(&[bad.into()]),
                    Err(OutlookError::Input(_))
                ),
                "{bad}"
            );
        }
        let big = tmp.path().join("big.bin");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_ATTACHMENT_BYTES + 1).unwrap();
        assert!(matches!(
            b.load_attachments(&["big.bin".into()]),
            Err(OutlookError::Input(m)) if m.contains("MiB")
        ));
    }
}
