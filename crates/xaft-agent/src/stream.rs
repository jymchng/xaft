//! Streaming sink abstraction for forwarding `StreamEvent`s out of agents.
//!
//! # Design
//!
//! `StreamSink` is a fire-and-forget trait — the agent calls `send()` and
//! does not wait for the consumer to process the event. Backpressure is
//! handled at the transport level (unbounded or bounded channel, etc.).
//!
//! # Provided implementations
//!
//! - [`NopSink`] — discards all events (default, zero overhead)
//! - [`ChannelSink`] — forwards events to an unbounded mpsc channel

use agtrs_runtime::streaming::StreamEvent;

// ── Trait ─────────────────────────────────────────────────────────────────────

/// A fire-and-forget sink for [`StreamEvent`]s.
///
/// Implementations must be `Send + Sync + 'static` so they can be shared
/// across async tasks.
pub trait StreamSink: Send + Sync + 'static {
    /// Forward an event to the sink.
    ///
    /// This call must not block. Drop the event silently if the downstream
    /// consumer has disconnected.
    fn send(&self, event: StreamEvent);
}

// ── NopSink ───────────────────────────────────────────────────────────────────

/// A [`StreamSink`] that silently discards all events.
///
/// Zero overhead — all calls are no-ops.
#[derive(Debug, Default, Clone)]
pub struct NopSink;

impl StreamSink for NopSink {
    #[inline]
    fn send(&self, _event: StreamEvent) {}
}

// ── ChannelSink ───────────────────────────────────────────────────────────────

/// A [`StreamSink`] backed by an unbounded `futures::channel::mpsc` channel.
///
/// Create with [`channel()`] to get a `(ChannelSink, Receiver)` pair.
#[derive(Clone)]
pub struct ChannelSink {
    tx: futures::channel::mpsc::UnboundedSender<StreamEvent>,
}

impl std::fmt::Debug for ChannelSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelSink").finish()
    }
}

impl StreamSink for ChannelSink {
    fn send(&self, event: StreamEvent) {
        // `unbounded_send` never blocks; only fails when receiver is dropped
        let _ = self.tx.unbounded_send(event);
    }
}

// ── Vec-collecting sink (for tests) ──────────────────────────────────────────

/// A [`StreamSink`] that collects events into a shared `Vec` (for testing).
#[derive(Debug, Clone, Default)]
pub struct CollectSink {
    events: std::sync::Arc<std::sync::Mutex<Vec<StreamEvent>>>,
}

impl CollectSink {
    /// Create a new collecting sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drain all collected events.
    pub fn drain(&self) -> Vec<StreamEvent> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }

    /// Count collected events.
    pub fn len(&self) -> usize {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// True if no events have been collected.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl StreamSink for CollectSink {
    fn send(&self, event: StreamEvent) {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(event);
    }
}

// ── Factory ───────────────────────────────────────────────────────────────────

/// Create a `(ChannelSink, Receiver<StreamEvent>)` pair.
///
/// The sink can be given to an agent; the receiver is used by the caller to
/// consume emitted events.
pub fn channel() -> (
    ChannelSink,
    futures::channel::mpsc::UnboundedReceiver<StreamEvent>,
) {
    let (tx, rx) = futures::channel::mpsc::unbounded();
    (ChannelSink { tx }, rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agtrs_runtime::transport::{StopReason, TokenUsage};

    fn done_event() -> StreamEvent {
        StreamEvent::Done {
            content: "done".into(),
            stop_reason: StopReason::EndTurn,
            usage: TokenUsage::new(10, 20),
            turns: 1,
            agent_name: "test".into(),
            messages: vec![],
        }
    }

    #[test]
    fn nop_sink_accepts_all() {
        let s = NopSink;
        s.send(done_event());
        s.send(StreamEvent::Error { message: "err".into() });
    }

    #[test]
    fn channel_sink_forwards_events() {
        use futures::executor::block_on;
        use futures::StreamExt;

        let (sink, mut rx) = channel();
        sink.send(done_event());
        sink.send(StreamEvent::TextDelta { delta: "hi".into() });
        drop(sink);

        let events: Vec<_> = block_on(async { rx.collect::<Vec<_>>().await });
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], StreamEvent::Done { .. }));
        assert!(matches!(events[1], StreamEvent::TextDelta { .. }));
    }

    #[test]
    fn collect_sink_collects() {
        let s = CollectSink::new();
        assert!(s.is_empty());
        s.send(done_event());
        s.send(StreamEvent::TextDelta { delta: "x".into() });
        assert_eq!(s.len(), 2);
        let drained = s.drain();
        assert_eq!(drained.len(), 2);
        assert!(s.is_empty()); // drained
    }

    #[test]
    fn channel_sink_dropped_receiver_does_not_panic() {
        let (sink, rx) = channel();
        drop(rx); // consumer gone
        // should silently discard, not panic
        sink.send(done_event());
    }
}
