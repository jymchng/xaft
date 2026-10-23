//! A minimal Chrome DevTools Protocol connection.
//!
//! xaft speaks CDP directly instead of through a generated binding: the
//! browser engine needs about twenty methods, and a hand-written client over
//! two message pumps keeps the dependency surface to one small WebSocket
//! crate. Two transports feed the same connection:
//!
//! * **pipe** — `--remote-debugging-pipe`: NUL-delimited JSON over file
//!   descriptors 3 and 4 of a browser xaft launched. No TCP port is opened, so
//!   no other local user can attach to the session, and the browser exits on
//!   its own when xaft goes away and the pipe closes.
//! * **WebSocket** — an operator-run browser's loopback DevTools endpoint
//!   (`cloakbrowser` `transport = "cdp"`), or a launched browser on platforms
//!   without the pipe transport.
//!
//! The connection routes responses to their callers by `id` and forwards
//! every event to a single channel, which the engine's event loop drains in
//! order — so no event is ever dropped for want of a subscriber.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// The largest single CDP message accepted (a full-page screenshot is the
/// largest legitimate one). Anything bigger ends the connection rather than
/// growing a buffer without bound.
pub(crate) const MAX_MESSAGE_BYTES: usize = 512 * 1024 * 1024;

/// A CDP event: `method` + `params`, and the flat-mode session it came from.
#[derive(Debug, Clone)]
pub(crate) struct CdpEvent {
    /// `None` for the browser-level session.
    pub session_id: Option<String>,
    /// The event name, e.g. `Page.lifecycleEvent`.
    pub method: String,
    /// The event payload.
    pub params: Value,
}

/// Why a CDP call failed. Kept internal: the engine maps each variant to a
/// [`super::errors::BrowserFailure`] bucket and never shows the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CdpError {
    /// The connection is gone.
    Closed,
    /// No response within the deadline.
    Timeout,
    /// The browser answered with a protocol error.
    Protocol {
        /// The JSON-RPC error code.
        code: i64,
        /// The browser's message (logged only, never returned to an agent).
        message: String,
    },
}

impl std::fmt::Display for CdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => f.write_str("cdp connection closed"),
            Self::Timeout => f.write_str("cdp call timed out"),
            Self::Protocol { code, message } => write!(f, "cdp error {code}: {message}"),
        }
    }
}

impl std::error::Error for CdpError {}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, CdpError>>>>>;

/// Raw message pumps for one transport: inbound frames, outbound frames, and
/// the tasks moving them.
pub(crate) struct TransportPumps {
    pub inbound: mpsc::UnboundedReceiver<String>,
    pub outbound: mpsc::UnboundedSender<String>,
    pub tasks: Vec<JoinHandle<()>>,
}

/// Read one NUL-terminated message, refusing anything over `max` bytes.
/// `Ok(false)` is a clean end of stream.
async fn read_message<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    out: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<bool> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(false);
        }
        if let Some(i) = available.iter().position(|b| *b == 0) {
            out.extend_from_slice(&available[..i]);
            reader.consume(i + 1);
            return Ok(true);
        }
        let n = available.len();
        out.extend_from_slice(available);
        reader.consume(n);
        if out.len() > max {
            return Err(std::io::Error::other("cdp message exceeds the size limit"));
        }
    }
}

/// Pumps for `--remote-debugging-pipe`: NUL-delimited JSON frames.
pub(crate) fn pipe_pumps<R, W>(reader: R, writer: W) -> TransportPumps
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (in_tx, inbound) = mpsc::unbounded_channel::<String>();
    let (outbound, mut out_rx) = mpsc::unbounded_channel::<String>();
    let read_task = tokio::spawn(async move {
        let mut reader = BufReader::with_capacity(1 << 16, reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match read_message(&mut reader, &mut buf, MAX_MESSAGE_BYTES).await {
                Ok(true) => match String::from_utf8(std::mem::take(&mut buf)) {
                    Ok(text) => {
                        if in_tx.send(text).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                Ok(false) | Err(_) => break,
            }
        }
    });
    let write_task = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(message) = out_rx.recv().await {
            if writer.write_all(message.as_bytes()).await.is_err()
                || writer.write_all(&[0]).await.is_err()
                || writer.flush().await.is_err()
            {
                break;
            }
        }
        let _ = writer.shutdown().await;
    });
    TransportPumps {
        inbound,
        outbound,
        tasks: vec![read_task, write_task],
    }
}

/// Pumps for a DevTools WebSocket.
pub(crate) fn websocket_pumps<S>(socket: tokio_tungstenite::WebSocketStream<S>) -> TransportPumps
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use tokio_tungstenite::tungstenite::Message;
    let (mut sink, mut stream) = socket.split();
    let (in_tx, inbound) = mpsc::unbounded_channel::<String>();
    let (outbound, mut out_rx) = mpsc::unbounded_channel::<String>();
    let read_task = tokio::spawn(async move {
        while let Some(frame) = stream.next().await {
            let text = match frame {
                Ok(Message::Text(text)) => text,
                Ok(Message::Binary(bytes)) => match String::from_utf8(bytes) {
                    Ok(text) => text,
                    Err(_) => break,
                },
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => continue,
            };
            if in_tx.send(text).is_err() {
                break;
            }
        }
    });
    let write_task = tokio::spawn(async move {
        while let Some(message) = out_rx.recv().await {
            if sink.send(Message::Text(message)).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    TransportPumps {
        inbound,
        outbound,
        tasks: vec![read_task, write_task],
    }
}

/// The WebSocket configuration for DevTools: frames as large as the largest
/// screenshot, rather than tungstenite's 16 MiB default.
pub(crate) fn websocket_config() -> tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
    tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
        max_message_size: Some(MAX_MESSAGE_BYTES),
        max_frame_size: Some(MAX_MESSAGE_BYTES),
        ..Default::default()
    }
}

/// One CDP connection.
pub(crate) struct CdpConnection {
    outbound: mpsc::UnboundedSender<String>,
    pending: Pending,
    next_id: AtomicU64,
    closed: Arc<AtomicBool>,
    tasks: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for CdpConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CdpConnection")
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

impl CdpConnection {
    /// Start routing messages for `pumps`. Returns the connection and the
    /// single channel every event is delivered to.
    pub(crate) fn start(pumps: TransportPumps) -> (Arc<Self>, mpsc::UnboundedReceiver<CdpEvent>) {
        let TransportPumps {
            mut inbound,
            outbound,
            mut tasks,
        } = pumps;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let (event_tx, event_rx) = mpsc::unbounded_channel::<CdpEvent>();

        let dispatch_pending = Arc::clone(&pending);
        let dispatch_closed = Arc::clone(&closed);
        let dispatcher = tokio::spawn(async move {
            while let Some(text) = inbound.recv().await {
                let Ok(message) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if let Some(id) = message.get("id").and_then(Value::as_u64) {
                    let sender = dispatch_pending
                        .lock()
                        .expect("cdp pending map poisoned")
                        .remove(&id);
                    if let Some(sender) = sender {
                        let outcome = match message.get("error") {
                            Some(error) => Err(CdpError::Protocol {
                                code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                                message: error
                                    .get("message")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                            }),
                            None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        let _ = sender.send(outcome);
                    }
                    continue;
                }
                if let Some(method) = message.get("method").and_then(Value::as_str) {
                    let event = CdpEvent {
                        session_id: message
                            .get("sessionId")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        method: method.to_string(),
                        params: message.get("params").cloned().unwrap_or(Value::Null),
                    };
                    let _ = event_tx.send(event);
                }
            }
            // The transport ended: fail every waiting call and close the event
            // channel so the engine's event loop sees the end too.
            dispatch_closed.store(true, Ordering::SeqCst);
            let waiting: Vec<_> = dispatch_pending
                .lock()
                .expect("cdp pending map poisoned")
                .drain()
                .collect();
            for (_, sender) in waiting {
                let _ = sender.send(Err(CdpError::Closed));
            }
            drop(event_tx);
        });
        tasks.push(dispatcher);
        (
            Arc::new(Self {
                outbound,
                pending,
                next_id: AtomicU64::new(1),
                closed,
                tasks,
            }),
            event_rx,
        )
    }

    /// Whether the transport has ended.
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst) || self.outbound.is_closed()
    }

    /// Send `method` to `session` (browser-level when `None`) and wait for the
    /// result.
    pub(crate) async fn call(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, CdpError> {
        if self.is_closed() {
            return Err(CdpError::Closed);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = Value::String(session.to_string());
        }
        let (tx, rx) = oneshot::channel();
        {
            // Check `closed` under the same lock the dispatcher drains with:
            // it sets `closed` before draining, so a call registered after the
            // drain sees the flag here instead of waiting out its timeout.
            let mut pending = self.pending.lock().expect("cdp pending map poisoned");
            if self.closed.load(Ordering::SeqCst) {
                return Err(CdpError::Closed);
            }
            pending.insert(id, tx);
        }
        if self.outbound.send(message.to_string()).is_err() {
            self.pending
                .lock()
                .expect("cdp pending map poisoned")
                .remove(&id);
            return Err(CdpError::Closed);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err(CdpError::Closed),
            Err(_) => {
                self.pending
                    .lock()
                    .expect("cdp pending map poisoned")
                    .remove(&id);
                Err(CdpError::Timeout)
            }
        }
    }
}

impl Drop for CdpConnection {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    /// A fake browser on the other end of a pipe: answers `Echo.call` with its
    /// params, `Fail.call` with a protocol error, and emits one event first.
    async fn fake_browser(stream: tokio::io::DuplexStream) {
        let (read, mut write) = tokio::io::split(stream);
        let mut reader = BufReader::new(read);
        let event = json!({"method": "Test.ready", "params": {"n": 1}, "sessionId": "S1"});
        write.write_all(event.to_string().as_bytes()).await.unwrap();
        write.write_all(&[0]).await.unwrap();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match read_message(&mut reader, &mut buf, MAX_MESSAGE_BYTES).await {
                Ok(true) => {}
                _ => break,
            }
            let request: Value = serde_json::from_slice(&buf).unwrap();
            let id = request["id"].clone();
            let reply = match request["method"].as_str().unwrap() {
                "Echo.call" => json!({"id": id, "result": request["params"]}),
                "Fail.call" => json!({"id": id, "error": {"code": -32000, "message": "nope"}}),
                "Hang.call" => continue,
                "Bye.call" => break,
                _ => json!({"id": id, "result": {}}),
            };
            write.write_all(reply.to_string().as_bytes()).await.unwrap();
            write.write_all(&[0]).await.unwrap();
        }
    }

    fn connect() -> (Arc<CdpConnection>, mpsc::UnboundedReceiver<CdpEvent>) {
        let (ours, theirs) = duplex(1 << 20);
        tokio::spawn(fake_browser(theirs));
        let (read, write) = tokio::io::split(ours);
        CdpConnection::start(pipe_pumps(read, write))
    }

    #[tokio::test]
    async fn responses_are_routed_by_id_and_events_by_channel() {
        let (conn, mut events) = connect();
        let event = events.recv().await.unwrap();
        assert_eq!(event.method, "Test.ready");
        assert_eq!(event.session_id.as_deref(), Some("S1"));
        let a = conn.call(None, "Echo.call", json!({"a": 1}), Duration::from_secs(5));
        let b = conn.call(
            Some("S1"),
            "Echo.call",
            json!({"b": 2}),
            Duration::from_secs(5),
        );
        let (a, b) = tokio::join!(a, b);
        assert_eq!(a.unwrap(), json!({"a": 1}));
        assert_eq!(b.unwrap(), json!({"b": 2}));
    }

    #[tokio::test]
    async fn protocol_errors_and_timeouts_are_typed() {
        let (conn, _events) = connect();
        let err = conn
            .call(None, "Fail.call", json!({}), Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(err, CdpError::Protocol { code: -32000, .. }));
        let err = conn
            .call(None, "Hang.call", json!({}), Duration::from_millis(50))
            .await
            .unwrap_err();
        assert_eq!(err, CdpError::Timeout);
    }

    #[tokio::test]
    async fn a_closed_transport_fails_waiting_and_later_calls() {
        let (conn, mut events) = connect();
        let _ = events.recv().await;
        let hang = conn.call(None, "Hang.call", json!({}), Duration::from_secs(30));
        let bye = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = conn
                .call(None, "Bye.call", json!({}), Duration::from_millis(200))
                .await;
        };
        let (hang, _) = tokio::join!(hang, bye);
        assert_eq!(hang.unwrap_err(), CdpError::Closed);
        assert!(
            events.recv().await.is_none(),
            "the event channel closes with the transport"
        );
        assert!(conn.is_closed());
        assert_eq!(
            conn.call(None, "Echo.call", json!({}), Duration::from_secs(1))
                .await
                .unwrap_err(),
            CdpError::Closed
        );
    }

    #[tokio::test]
    async fn oversized_messages_end_the_stream() {
        let data = [b'x'; 64];
        let mut reader = BufReader::new(&data[..]);
        let mut out = Vec::new();
        assert!(read_message(&mut reader, &mut out, 16).await.is_err());
    }
}
