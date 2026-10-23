//! The nine Outlook tools, one struct per verb over a shared backend.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{Tool, ToolContext, ToolResult};

use super::graph::{GraphOutlookBackend, OutlookError};

/// The nine tools, in agenthicc's `OUTLOOK_AGENT_TOOLS` order.
pub const OUTLOOK_TOOL_NAMES: [&str; 9] = [
    "list_emails",
    "read_email",
    "send_email",
    "reply_email",
    "search_emails",
    "move_email",
    "list_folders",
    "calendar_events",
    "create_event",
];

/// One of agenthicc's nine Outlook operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutlookVerb {
    /// `list_emails` (network read).
    ListEmails,
    /// `read_email` (network read).
    ReadEmail,
    /// `send_email` (network write).
    SendEmail,
    /// `reply_email` (network write).
    ReplyEmail,
    /// `search_emails` (network search).
    SearchEmails,
    /// `move_email` (network write).
    MoveEmail,
    /// `list_folders` (network read).
    ListFolders,
    /// `calendar_events` (network read).
    CalendarEvents,
    /// `create_event` (network write).
    CreateEvent,
}

impl OutlookVerb {
    /// Every verb, in [`OUTLOOK_TOOL_NAMES`] order.
    pub const ALL: [OutlookVerb; 9] = [
        Self::ListEmails,
        Self::ReadEmail,
        Self::SendEmail,
        Self::ReplyEmail,
        Self::SearchEmails,
        Self::MoveEmail,
        Self::ListFolders,
        Self::CalendarEvents,
        Self::CreateEvent,
    ];

    /// The tool name (agenthicc's function name).
    pub fn name(self) -> &'static str {
        match self {
            Self::ListEmails => "list_emails",
            Self::ReadEmail => "read_email",
            Self::SendEmail => "send_email",
            Self::ReplyEmail => "reply_email",
            Self::SearchEmails => "search_emails",
            Self::MoveEmail => "move_email",
            Self::ListFolders => "list_folders",
            Self::CalendarEvents => "calendar_events",
            Self::CreateEvent => "create_event",
        }
    }

    /// agenthicc's `tool_network_write` verbs: they change the mailbox or
    /// calendar, so each call is approved first.
    pub fn is_write(self) -> bool {
        matches!(
            self,
            Self::SendEmail | Self::ReplyEmail | Self::MoveEmail | Self::CreateEvent
        )
    }

    fn description(self) -> &'static str {
        match self {
            Self::ListEmails => {
                "List emails from an Outlook folder (Microsoft Graph). Returns \
                 [{id, subject, from, date, unread, body_preview}]."
            }
            Self::ReadEmail => {
                "Read the full content of an email by ID (as returned by list_emails or \
                 search_emails). Returns {subject, from, to, cc, date, body_html, body_text}."
            }
            Self::SendEmail => {
                "Send an email via Outlook. Attachments are workspace file paths. \
                 Returns {ok, message_id}."
            }
            Self::ReplyEmail => {
                "Reply to an email; reply_all replies to every recipient. Returns {ok, message_id}."
            }
            Self::SearchEmails => {
                "Search emails in an Outlook folder by keyword. Returns \
                 [{id, subject, from, date, body_preview}]."
            }
            Self::MoveEmail => {
                "Move an email to a different folder (a folder id or a well-known name such \
                 as \"archive\"). Returns {ok}."
            }
            Self::ListFolders => {
                "List all available Outlook mail folders. Returns [{id, name, unread_count}]."
            }
            Self::CalendarEvents => {
                "Retrieve calendar events within a date range. Returns \
                 [{id, subject, start, end, location, attendees}]."
            }
            Self::CreateEvent => {
                "Create a new calendar event in Outlook (times are UTC). Returns {ok, event_id}."
            }
        }
    }

    fn schema(self) -> Value {
        let s = |d: &str| json!({"type": "string", "description": d});
        let list =
            |d: &str| json!({"type": "array", "items": {"type": "string"}, "description": d});
        let (properties, required): (Value, Vec<&str>) = match self {
            Self::ListEmails => (
                json!({
                    "folder": {"type": "string", "default": "Inbox",
                               "description": "Folder name to list from (default: Inbox)."},
                    "n": {"type": "integer", "default": 20, "minimum": 1,
                          "description": "Maximum number of emails to return (default 20)."},
                    "unread_only": {"type": "boolean", "default": false,
                                    "description": "Only return unread messages when true."},
                }),
                vec![],
            ),
            Self::ReadEmail => (
                json!({"email_id": s("The email identifier returned by list_emails or search_emails.")}),
                vec!["email_id"],
            ),
            Self::SendEmail => (
                json!({
                    "to": list("Recipient email addresses."),
                    "subject": s("Email subject line."),
                    "body": s("Email body."),
                    "cc": list("Optional CC email addresses."),
                    "attachments": list("Optional workspace file paths to attach."),
                }),
                vec!["to", "subject", "body"],
            ),
            Self::ReplyEmail => (
                json!({
                    "email_id": s("The email identifier to reply to."),
                    "body": s("Reply body text."),
                    "reply_all": {"type": "boolean", "default": false,
                                  "description": "Reply to all recipients (default false)."},
                }),
                vec!["email_id", "body"],
            ),
            Self::SearchEmails => (
                json!({
                    "query": s("Search query string."),
                    "folder": {"type": "string", "default": "Inbox",
                               "description": "Folder to search within (default: Inbox)."},
                    "n": {"type": "integer", "default": 20, "minimum": 1,
                          "description": "Maximum number of results to return (default 20)."},
                }),
                vec!["query"],
            ),
            Self::MoveEmail => (
                json!({
                    "email_id": s("The email identifier to move."),
                    "destination": s("Destination folder id or well-known name (e.g. \"archive\")."),
                }),
                vec!["email_id", "destination"],
            ),
            Self::ListFolders => (json!({}), vec![]),
            Self::CalendarEvents => (
                json!({
                    "start_date": s("Start of the range in ISO-8601 format (e.g. \"2026-06-01\")."),
                    "end_date": s("End of the range in ISO-8601 format (e.g. \"2026-06-30\")."),
                }),
                vec!["start_date", "end_date"],
            ),
            Self::CreateEvent => (
                json!({
                    "subject": s("Event title."),
                    "start": s("Start datetime in ISO-8601 format (e.g. \"2026-06-15T09:00:00\")."),
                    "end": s("End datetime in ISO-8601 format (e.g. \"2026-06-15T10:00:00\")."),
                    "attendees": list("Optional attendee email addresses."),
                    "body": s("Optional event description / agenda."),
                }),
                vec!["subject", "start", "end"],
            ),
        };
        json!({"type": "object", "properties": properties, "required": required})
    }
}

/// One Outlook tool.
#[derive(Clone)]
pub struct OutlookTool {
    verb: OutlookVerb,
    backend: Arc<GraphOutlookBackend>,
}

impl OutlookTool {
    /// The tool for `verb` over `backend`.
    pub fn new(verb: OutlookVerb, backend: Arc<GraphOutlookBackend>) -> Self {
        Self { verb, backend }
    }

    /// Which operation this tool performs.
    pub fn verb(&self) -> OutlookVerb {
        self.verb
    }
}

impl std::fmt::Debug for OutlookTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutlookTool")
            .field("verb", &self.verb)
            .finish_non_exhaustive()
    }
}

/// All nine tools over one backend.
pub fn outlook_tools(backend: &Arc<GraphOutlookBackend>) -> Vec<OutlookTool> {
    OutlookVerb::ALL
        .into_iter()
        .map(|v| OutlookTool::new(v, Arc::clone(backend)))
        .collect()
}

/// The five read verbs (agenthicc's `tool_network_read` / `_search`).
pub fn read_only_outlook_tools(backend: &Arc<GraphOutlookBackend>) -> Vec<OutlookTool> {
    OutlookVerb::ALL
        .into_iter()
        .filter(|v| !v.is_write())
        .map(|v| OutlookTool::new(v, Arc::clone(backend)))
        .collect()
}

fn bad_input(tool: &str, reason: impl Into<String>) -> AgtrsError {
    AgtrsError::ToolCallFailed {
        tool_name: tool.to_string(),
        reason: reason.into(),
    }
}

fn required<'a>(input: &'a Value, field: &str, tool: &str) -> Result<&'a str, AgtrsError> {
    match input.get(field) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.as_str()),
        Some(Value::String(_)) => Err(bad_input(tool, format!("'{field}' must not be empty"))),
        _ => Err(bad_input(
            tool,
            format!("required field '{field}' is missing or not a string"),
        )),
    }
}

fn optional<'a>(
    input: &'a Value,
    field: &str,
    default: &'a str,
    tool: &str,
) -> Result<&'a str, AgtrsError> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(default),
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(_) => Err(bad_input(tool, format!("'{field}' must be a string"))),
    }
}

fn flag(input: &Value, field: &str, tool: &str) -> Result<bool, AgtrsError> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("true") => Ok(true),
        Some(Value::String(s)) if s.eq_ignore_ascii_case("false") => Ok(false),
        Some(_) => Err(bad_input(tool, format!("'{field}' must be a boolean"))),
    }
}

fn count(input: &Value, tool: &str) -> Result<u64, AgtrsError> {
    let n = match input.get("n") {
        None | Some(Value::Null) => return Ok(20),
        Some(Value::Number(n)) => n.as_u64().or_else(|| {
            n.as_f64()
                .filter(|f| f.fract() == 0.0 && *f >= 0.0)
                .map(|f| f as u64)
        }),
        Some(Value::String(s)) => s.trim().parse().ok(),
        Some(_) => None,
    };
    match n {
        Some(n) if n >= 1 => Ok(n),
        _ => Err(bad_input(tool, "'n' must be a positive integer")),
    }
}

/// A list of addresses or paths. A bare string is one entry: models often
/// send `"to": "a@example.com"` for a single recipient.
fn strings(input: &Value, field: &str, tool: &str) -> Result<Vec<String>, AgtrsError> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(vec![s.trim().to_string()]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| match v {
                Value::String(s) => Ok(s.trim().to_string()),
                _ => Err(bad_input(
                    tool,
                    format!("'{field}' must be a list of strings"),
                )),
            })
            .filter(|r| !matches!(r, Ok(s) if s.is_empty()))
            .collect(),
        Some(_) => Err(bad_input(
            tool,
            format!("'{field}' must be a list of strings"),
        )),
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// agenthicc's `_safe_call`, plus the xaft error flag: a recoverable network
/// failure becomes `{"ok": false, "error": ..., "recoverable": true}`.
fn envelope(result: Result<Value, OutlookError>, tool_use_id: &str) -> ToolResult {
    match result {
        Ok(value) => {
            let failed = value.get("ok").and_then(Value::as_bool) == Some(false);
            if failed {
                ToolResult::error(pretty(&value), tool_use_id)
            } else {
                ToolResult::ok(pretty(&value), tool_use_id)
            }
        }
        Err(err) => {
            let mut body = json!({"ok": false, "error": err.to_string()});
            if err.is_recoverable() {
                body["recoverable"] = json!(true);
            }
            ToolResult::error(pretty(&body), tool_use_id)
        }
    }
}

#[async_trait]
impl Tool for OutlookTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        self.verb.name()
    }

    fn description(&self) -> &str {
        self.verb.description()
    }

    fn schema(&self) -> Value {
        self.verb.schema()
    }

    fn requires_confirmation(&self) -> bool {
        self.verb.is_write()
    }

    // `skip_all`: mail bodies and addresses are personal data.
    #[tracing::instrument(name = "outlook_tool", skip_all, fields(tool = %self.verb.name()))]
    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        if ctx.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("{} cancelled before it started", self.verb.name()),
            });
        }
        let name = self.verb.name();
        let b = &self.backend;
        let call = async {
            Ok::<_, AgtrsError>(match self.verb {
                OutlookVerb::ListEmails => {
                    b.list_emails(
                        optional(&input, "folder", "Inbox", name)?,
                        count(&input, name)?,
                        flag(&input, "unread_only", name)?,
                    )
                    .await
                }
                OutlookVerb::ReadEmail => b.read_email(required(&input, "email_id", name)?).await,
                OutlookVerb::SendEmail => {
                    let to = strings(&input, "to", name)?;
                    if to.is_empty() {
                        return Err(bad_input(name, "'to' needs at least one recipient"));
                    }
                    b.send_email(
                        &to,
                        required(&input, "subject", name)?,
                        input.get("body").and_then(Value::as_str).ok_or_else(|| {
                            bad_input(name, "required field 'body' is missing or not a string")
                        })?,
                        &strings(&input, "cc", name)?,
                        &strings(&input, "attachments", name)?,
                    )
                    .await
                }
                OutlookVerb::ReplyEmail => {
                    b.reply_email(
                        required(&input, "email_id", name)?,
                        input.get("body").and_then(Value::as_str).ok_or_else(|| {
                            bad_input(name, "required field 'body' is missing or not a string")
                        })?,
                        flag(&input, "reply_all", name)?,
                    )
                    .await
                }
                OutlookVerb::SearchEmails => {
                    b.search_emails(
                        required(&input, "query", name)?,
                        optional(&input, "folder", "Inbox", name)?,
                        count(&input, name)?,
                    )
                    .await
                }
                OutlookVerb::MoveEmail => {
                    b.move_email(
                        required(&input, "email_id", name)?,
                        required(&input, "destination", name)?,
                    )
                    .await
                }
                OutlookVerb::ListFolders => b.list_folders().await,
                OutlookVerb::CalendarEvents => {
                    b.calendar_events(
                        required(&input, "start_date", name)?,
                        required(&input, "end_date", name)?,
                    )
                    .await
                }
                OutlookVerb::CreateEvent => {
                    let body = optional(&input, "body", "", name)?;
                    b.create_event(
                        required(&input, "subject", name)?,
                        required(&input, "start", name)?,
                        required(&input, "end", name)?,
                        &strings(&input, "attendees", name)?,
                        (!body.is_empty()).then_some(body),
                    )
                    .await
                }
            })
        };
        let result = tokio::select! {
            biased;
            _ = ctx.cancel_token.cancelled() => {
                return Err(AgtrsError::Cancelled { reason: format!("{name} cancelled") });
            }
            r = call => r?,
        };
        Ok(envelope(result, &ctx.tool_use_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outlook::graph::TokenSource;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One request the mock Graph server saw.
    #[derive(Debug, Clone)]
    struct Seen {
        method: String,
        target: String,
        authorization: String,
        body: Value,
    }

    type Router = Arc<dyn Fn(&str, &str) -> (u16, String) + Send + Sync>;

    /// A minimal in-process HTTP/1.1 server standing in for Graph.
    async fn mock_graph(router: Router) -> (String, Arc<Mutex<Vec<Seen>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let router = Arc::clone(&router);
                let log = Arc::clone(&log);
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 8192];
                    let head_end = loop {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let header = |name: &str| {
                        head.lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.trim()
                                    .eq_ignore_ascii_case(name)
                                    .then(|| v.trim().to_string())
                            })
                            .unwrap_or_default()
                    };
                    let len: usize = header("content-length").parse().unwrap_or(0);
                    while buf.len() < head_end + len {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let body = &buf[head_end..(head_end + len).min(buf.len())];
                    let mut first = head.lines().next().unwrap_or("").split(' ');
                    let method = first.next().unwrap_or("").to_string();
                    let target = first.next().unwrap_or("").to_string();
                    log.lock().unwrap().push(Seen {
                        method: method.clone(),
                        target: target.clone(),
                        authorization: header("authorization"),
                        body: serde_json::from_slice(body).unwrap_or(Value::Null),
                    });
                    let (status, payload) = router(&method, &target);
                    let response = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = sock.write_all(response.as_bytes()).await;
                });
            }
        });
        (format!("http://{addr}/v1.0"), seen)
    }

    fn backend(base: &str, root: &std::path::Path) -> Arc<GraphOutlookBackend> {
        Arc::new(
            GraphOutlookBackend::new(root)
                .with_base_url(base)
                .with_token(TokenSource::Fixed("tok-123".into()))
                .with_read_timeout_s(5.0),
        )
    }

    async fn run(tool: &OutlookTool, input: Value) -> (Value, bool) {
        let r = tool.call(input, &ToolContext::new("call-1")).await.unwrap();
        (serde_json::from_str(&r.content).unwrap(), r.is_error)
    }

    fn tool(verb: OutlookVerb, b: &Arc<GraphOutlookBackend>) -> OutlookTool {
        OutlookTool::new(verb, Arc::clone(b))
    }

    #[tokio::test]
    async fn list_emails_maps_graph_messages_to_agenthiccs_shape() {
        let router: Router = Arc::new(|_, _| {
            (
                200,
                json!({"value": [{
                    "id": "m1", "subject": "Hello",
                    "from": {"emailAddress": {"address": "a@b.com"}},
                    "receivedDateTime": "2025-01-01T10:00:00Z",
                    "isRead": false, "bodyPreview": "Hi"
                }]})
                .to_string(),
            )
        });
        let (base, seen) = mock_graph(router).await;
        let tmp = tempfile::TempDir::new().unwrap();
        let b = backend(&base, tmp.path());
        let (body, is_error) = run(
            &tool(OutlookVerb::ListEmails, &b),
            json!({"folder": "Inbox", "n": 5, "unread_only": true}),
        )
        .await;
        assert!(!is_error, "{body}");
        assert_eq!(
            body,
            json!([{"id": "m1", "subject": "Hello", "from": "a@b.com",
                    "date": "2025-01-01T10:00:00Z", "unread": true, "body_preview": "Hi"}])
        );
        let req = seen.lock().unwrap()[0].clone();
        assert_eq!(req.method, "GET");
        assert_eq!(req.authorization, "Bearer tok-123");
        assert!(
            req.target
                .starts_with("/v1.0/me/mailFolders/Inbox/messages?$top=5&"),
            "{}",
            req.target
        );
        assert!(
            req.target.contains("$filter=isRead%20eq%20false"),
            "{}",
            req.target
        );
    }

    #[tokio::test]
    async fn read_search_folders_and_calendar_results() {
        let router: Router = Arc::new(|_, target| {
            let body = if target.starts_with("/v1.0/me/messages/") {
                json!({"subject": "S", "from": {"emailAddress": {"address": "f@x"}},
                       "toRecipients": [{"emailAddress": {"address": "t@x"}}],
                       "ccRecipients": [], "receivedDateTime": "d",
                       "body": {"contentType": "html", "content": "<p>hi</p>"}})
            } else if target.starts_with("/v1.0/me/mailFolders?") {
                json!({"value": [{"id": "f1", "displayName": "Inbox", "totalItemCount": 10, "unreadItemCount": 2}]})
            } else if target.starts_with("/v1.0/me/calendarView?") {
                json!({"value": [{"id": "e1", "subject": "Meeting",
                    "start": {"dateTime": "2025-01-01T09:00:00"}, "end": {"dateTime": "2025-01-01T10:00:00"},
                    "location": {"displayName": "Room A"},
                    "attendees": [{"emailAddress": {"address": "p@x"}}]}]})
            } else {
                json!({"value": [{"id": "m9", "subject": "Found", "from": {"emailAddress": {"address": "q@x"}},
                                  "receivedDateTime": "d", "bodyPreview": "p"}]})
            };
            (200, body.to_string())
        });
        let (base, seen) = mock_graph(router).await;
        let tmp = tempfile::TempDir::new().unwrap();
        let b = backend(&base, tmp.path());

        let (email, _) = run(
            &tool(OutlookVerb::ReadEmail, &b),
            json!({"email_id": "AAMk/1=="}),
        )
        .await;
        assert_eq!(
            email,
            json!({"subject": "S", "from": "f@x", "to": ["t@x"], "cc": [], "date": "d",
                   "body_html": "<p>hi</p>", "body_text": ""})
        );
        let (folders, _) = run(&tool(OutlookVerb::ListFolders, &b), json!({})).await;
        assert_eq!(
            folders,
            json!([{"id": "f1", "name": "Inbox", "unread_count": 2}])
        );
        let (events, _) = run(
            &tool(OutlookVerb::CalendarEvents, &b),
            json!({"start_date": "2025-01-01", "end_date": "2025-01-31"}),
        )
        .await;
        assert_eq!(
            events,
            json!([{"id": "e1", "subject": "Meeting", "start": "2025-01-01T09:00:00",
                    "end": "2025-01-01T10:00:00", "location": "Room A", "attendees": ["p@x"]}])
        );
        let (found, _) = run(
            &tool(OutlookVerb::SearchEmails, &b),
            json!({"query": "hello world", "n": 3}),
        )
        .await;
        assert_eq!(found[0]["id"], json!("m9"));

        let targets: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.target.clone())
            .collect();
        assert!(
            targets[0].starts_with("/v1.0/me/messages/AAMk%2F1%3D%3D?$select="),
            "an id cannot change the path: {}",
            targets[0]
        );
        assert!(targets[2].contains("startDateTime=2025-01-01&endDateTime=2025-01-31"));
        assert!(
            targets[3].contains("$search=%22hello%20world%22&$top=3"),
            "{}",
            targets[3]
        );
    }

    #[tokio::test]
    async fn writes_post_agenthiccs_payloads_and_return_its_results() {
        let router: Router = Arc::new(|_, target| {
            if target == "/v1.0/me/events" {
                (201, json!({"id": "new-event"}).to_string())
            } else if target.ends_with("/move") {
                (201, json!({"id": "moved"}).to_string())
            } else {
                (202, String::new())
            }
        });
        let (base, seen) = mock_graph(router).await;
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("notes.txt"), b"hi").unwrap();
        let b = backend(&base, tmp.path());

        let (sent, err) = run(
            &tool(OutlookVerb::SendEmail, &b),
            json!({"to": ["u@x.com"], "subject": "Subj", "body": "Body", "cc": ["c@x.com"],
                   "attachments": ["notes.txt"]}),
        )
        .await;
        assert!(!err);
        assert_eq!(sent, json!({"ok": true, "message_id": ""}));
        let (replied, _) = run(
            &tool(OutlookVerb::ReplyEmail, &b),
            json!({"email_id": "m1", "body": "OK", "reply_all": true}),
        )
        .await;
        assert_eq!(replied, json!({"ok": true, "message_id": "m1"}));
        let (moved, _) = run(
            &tool(OutlookVerb::MoveEmail, &b),
            json!({"email_id": "m1", "destination": "archive"}),
        )
        .await;
        assert_eq!(moved, json!({"ok": true}));
        let (created, _) = run(
            &tool(OutlookVerb::CreateEvent, &b),
            json!({"subject": "Stand-up", "start": "2025-01-01T09:00:00",
                   "end": "2025-01-01T09:30:00", "attendees": "p@x"}),
        )
        .await;
        assert_eq!(created, json!({"ok": true, "event_id": "new-event"}));

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen[0].target, "/v1.0/me/sendMail");
        let message = &seen[0].body["message"];
        assert_eq!(message["subject"], json!("Subj"));
        assert_eq!(
            message["body"],
            json!({"contentType": "Text", "content": "Body"})
        );
        assert_eq!(
            message["toRecipients"],
            json!([{"emailAddress": {"address": "u@x.com"}}])
        );
        assert_eq!(
            message["ccRecipients"],
            json!([{"emailAddress": {"address": "c@x.com"}}])
        );
        assert_eq!(message["attachments"][0]["name"], json!("notes.txt"));
        assert_eq!(seen[1].target, "/v1.0/me/messages/m1/replyAll");
        assert_eq!(seen[1].body, json!({"comment": "OK"}));
        assert_eq!(seen[2].body, json!({"destinationId": "archive"}));
        assert_eq!(
            seen[3].body,
            json!({"subject": "Stand-up",
                   "start": {"dateTime": "2025-01-01T09:00:00", "timeZone": "UTC"},
                   "end": {"dateTime": "2025-01-01T09:30:00", "timeZone": "UTC"},
                   "attendees": [{"emailAddress": {"address": "p@x"}, "type": "required"}]})
        );
    }

    #[tokio::test]
    async fn http_errors_and_network_failures_are_recoverable_results() {
        let router: Router = Arc::new(|_, _| {
            (
                401,
                json!({"error": {"code": "InvalidAuthenticationToken", "message": "Access token has expired."}})
                    .to_string(),
            )
        });
        let (base, _) = mock_graph(router).await;
        let tmp = tempfile::TempDir::new().unwrap();
        let (body, is_error) = run(
            &tool(OutlookVerb::ListFolders, &backend(&base, tmp.path())),
            json!({}),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["ok"], json!(false));
        assert_eq!(body["recoverable"], json!(true));
        let error = body["error"].as_str().unwrap();
        assert!(
            error.starts_with("HTTPStatusError: Client error '401 Unauthorized'"),
            "{error}"
        );
        assert!(error.contains("Access token has expired."), "{error}");
        assert!(!error.contains("tok-123"), "the token is never echoed");

        // Connection refused: a port nothing listens on.
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };
        let (body, _) = run(
            &tool(
                OutlookVerb::ListFolders,
                &backend(&format!("http://{closed}"), tmp.path()),
            ),
            json!({}),
        )
        .await;
        assert_eq!(body["recoverable"], json!(true));
        assert!(
            body["error"].as_str().unwrap().starts_with("ConnectError:"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn a_silent_server_times_out_recoverably() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock); // accept, never answer
            }
        });
        let tmp = tempfile::TempDir::new().unwrap();
        let b = Arc::new(
            GraphOutlookBackend::new(tmp.path())
                .with_base_url(format!("http://{addr}"))
                .with_token(TokenSource::Fixed("t".into()))
                .with_read_timeout_s(0.3),
        );
        let (body, _) = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            run(&tool(OutlookVerb::ListFolders, &b), json!({})),
        )
        .await
        .expect("the read timeout must fire, not hang");
        assert_eq!(body["recoverable"], json!(true));
        assert!(
            body["error"].as_str().unwrap().starts_with("Timeout:"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn an_unset_token_never_reaches_the_network() {
        let tmp = tempfile::TempDir::new().unwrap();
        let b = Arc::new(
            GraphOutlookBackend::new(tmp.path())
                .with_base_url("http://127.0.0.1:9")
                .with_token(TokenSource::Env("XAFT_TEST_MSGRAPH_TOKEN_NEVER_SET".into())),
        );
        let (body, is_error) = run(&tool(OutlookVerb::ListEmails, &b), json!({})).await;
        assert!(is_error);
        assert_eq!(body["recoverable"], json!(true));
        let error = body["error"].as_str().unwrap();
        assert!(
            error.contains("XAFT_TEST_MSGRAPH_TOKEN_NEVER_SET is not set"),
            "{error}"
        );
        assert!(error.contains("Mail.Send"), "{error}");
    }

    #[tokio::test]
    async fn malformed_calls_are_refused_before_any_request() {
        let tmp = tempfile::TempDir::new().unwrap();
        let b = backend("http://127.0.0.1:9", tmp.path());
        for (verb, input) in [
            (OutlookVerb::ReadEmail, json!({})),
            (
                OutlookVerb::SendEmail,
                json!({"to": [], "subject": "s", "body": "b"}),
            ),
            (
                OutlookVerb::SendEmail,
                json!({"to": ["a@x"], "subject": "s"}),
            ),
            (OutlookVerb::ListEmails, json!({"n": 0})),
            (OutlookVerb::ListEmails, json!({"unread_only": "maybe"})),
            (
                OutlookVerb::CalendarEvents,
                json!({"start_date": "2025-01-01"}),
            ),
        ] {
            let r = tool(verb, &b)
                .call(input.clone(), &ToolContext::new("x"))
                .await;
            assert!(r.is_err(), "{} {input}", verb.name());
        }
        // An attachment outside the workspace is the call's fault, reported
        // without contacting Graph.
        let (body, is_error) = run(
            &tool(OutlookVerb::SendEmail, &b),
            json!({"to": "a@x", "subject": "s", "body": "b", "attachments": ["../secret"]}),
        )
        .await;
        assert!(is_error);
        assert!(body.get("recoverable").is_none());
        assert!(body["error"].as_str().unwrap().contains(".."));
    }

    #[test]
    fn names_order_and_approval_match_agenthicc() {
        let b = Arc::new(GraphOutlookBackend::new("/w"));
        let tools = outlook_tools(&b);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, OUTLOOK_TOOL_NAMES);
        let gated: Vec<&str> = tools
            .iter()
            .filter(|t| t.requires_confirmation())
            .map(|t| t.name())
            .collect();
        assert_eq!(
            gated,
            ["send_email", "reply_email", "move_email", "create_event"]
        );
        let read_tools = read_only_outlook_tools(&b);
        let reads: Vec<&str> = read_tools.iter().map(|t| t.name()).collect();
        assert_eq!(
            reads,
            [
                "list_emails",
                "read_email",
                "search_emails",
                "list_folders",
                "calendar_events"
            ]
        );
        for t in &tools {
            assert_eq!(t.schema()["type"], json!("object"), "{}", t.name());
        }
    }
}
