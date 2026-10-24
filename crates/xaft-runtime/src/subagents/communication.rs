//! Typed communication between a parent agent and its subagent workers
//! (agenthicc's `subagents/communication.py`).
//!
//! One [`AgentMessageBroker`] belongs to one pool. A worker reaches the
//! parent and its peers only through bounded, routed envelopes — never
//! through the parent's transcript — and nothing it sends can change its
//! tools or its policy. The broker runs no model; it routes, queues and
//! answers.
//!
//! The nine tools:
//!
//! | side | tool | does |
//! | --- | --- | --- |
//! | parent | `answer_subagent` | answer a worker's `ask_parent` question |
//! | parent | `send_subagent_message` | send one worker an instruction or information |
//! | parent | `collect_subagent_results` | wait for a pool left running for an answer |
//! | worker | `send_parent_message` | report a status or finding to the parent |
//! | worker | `ask_parent` | ask the parent; the pool returns `awaiting_clarification` |
//! | worker | `poll_agent_messages` | read queued parent and peer messages |
//! | worker | `send_peer_message` | message another worker in the pool |
//! | worker | `ask_peer` | ask another worker |
//! | worker | `answer_peer` | answer a peer's question |
//!
//! Every state change is journaled as an `agent_message_event`
//! ([`super::SubagentJournal`]). Limits are agenthicc's: 16 KiB per payload,
//! 256 queued messages per mailbox, 16 open questions, 300 s before a message
//! or question expires.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agtrs_runtime::error::AgtrsError;
use agtrs_runtime::tool::{ErasedTool, Tool, ToolContext, ToolResult};
use async_trait::async_trait;
use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};
use serde_json::{Map, Value, json};
use tokio::sync::{oneshot, watch};

use super::{AggregatedResult, SubagentJournal};

/// The message kinds a worker or the parent may send.
pub const MESSAGE_KINDS: &[&str] = &[
    "status",
    "information",
    "instruction",
    "clarification_request",
    "clarification_response",
    "handoff",
    "error",
    "cancel",
];
/// Largest payload, in bytes of JSON.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;
/// Most messages one mailbox may hold.
pub const MAX_MAILBOX_MESSAGES: usize = 256;
/// Most questions open at once in one pool.
pub const MAX_PENDING_QUESTIONS: usize = 16;
/// How long a message or question lives, in seconds.
pub const DEFAULT_MESSAGE_TIMEOUT_S: f64 = 300.0;

/// The longest a single wait for an answer may last, in seconds.
const MAX_WAIT_S: f64 = 86_400.0;

/// The parent's member id.
pub const MAIN: &str = "main";
/// The broadcast recipient.
pub const POOL: &str = "pool";

/// An invalid or unauthorised communication request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommunicationError(pub String);

impl std::fmt::Display for CommunicationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CommunicationError {}

fn err<T>(message: impl Into<String>) -> Result<T, CommunicationError> {
    Err(CommunicationError(message.into()))
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// One routed message (agenthicc's `AgentMessageEnvelope`).
#[derive(Debug, Clone, PartialEq)]
pub struct AgentMessageEnvelope {
    /// Unique id.
    pub message_id: String,
    /// The session.
    pub conversation_id: String,
    /// The parent run.
    pub parent_run_id: String,
    /// The pool.
    pub pool_id: String,
    /// `main` or a worker id.
    pub sender: String,
    /// `main`, a worker id, or `pool` for every other member.
    pub recipient: String,
    /// One of [`MESSAGE_KINDS`].
    pub kind: String,
    /// The bounded payload.
    pub payload: Map<String, Value>,
    /// The message this answers, if any.
    pub reply_to: String,
    /// Groups a request with its answer; defaults to the message id.
    pub correlation_id: String,
    /// Per-sender sequence number.
    pub sequence: u64,
    /// Epoch seconds.
    pub created_at: f64,
    /// Epoch seconds after which the message is dropped.
    pub expires_at: Option<f64>,
    /// Whether the sender waits for an answer.
    pub requires_response: bool,
    /// The parent policy the pool runs under.
    pub policy_revision: String,
}

impl AgentMessageEnvelope {
    /// agenthicc's `to_dict()`.
    pub fn to_json(&self) -> Value {
        json!({
            "message_id": self.message_id,
            "conversation_id": self.conversation_id,
            "parent_run_id": self.parent_run_id,
            "pool_id": self.pool_id,
            "sender": self.sender,
            "recipient": self.recipient,
            "kind": self.kind,
            "payload": self.payload,
            "reply_to": self.reply_to,
            "correlation_id": self.correlation_id,
            "sequence": self.sequence,
            "created_at": self.created_at,
            "expires_at": self.expires_at,
            "requires_response": self.requires_response,
            "policy_revision": self.policy_revision,
        })
    }
}

struct MessageRecord {
    envelope: AgentMessageEnvelope,
    state: &'static str,
}

struct QuestionRecord {
    question_id: String,
    envelope: AgentMessageEnvelope,
    status: &'static str,
    answer: String,
    waiters: Vec<oneshot::Sender<Result<String, String>>>,
}

#[derive(Default)]
struct BrokerState {
    members: BTreeSet<String>,
    messages: HashMap<String, MessageRecord>,
    mailboxes: HashMap<String, VecDeque<String>>,
    questions: Vec<QuestionRecord>,
    sequence: HashMap<String, u64>,
    closed: bool,
}

/// Routes bounded messages for one pool (agenthicc's `AgentMessageBroker`).
pub struct AgentMessageBroker {
    conversation_id: String,
    parent_run_id: String,
    pool_id: String,
    policy_revision: String,
    journal: Option<Arc<SubagentJournal>>,
    message_timeout_s: f64,
    state: Mutex<BrokerState>,
    /// Bumped whenever the set of open parent questions may have changed.
    pending: watch::Sender<u64>,
}

impl std::fmt::Debug for AgentMessageBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentMessageBroker")
            .field("pool_id", &self.pool_id)
            .finish_non_exhaustive()
    }
}

fn validate_identifier(value: &str, field: &str) -> Result<(), CommunicationError> {
    if value.is_empty() || value.chars().count() > 256 {
        return err(format!("{field} must be a non-empty bounded string"));
    }
    Ok(())
}

fn truncate(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

impl AgentMessageBroker {
    /// A broker for one pool, with agenthicc's limits.
    pub fn new(
        conversation_id: &str,
        parent_run_id: &str,
        pool_id: &str,
        policy_revision: &str,
        journal: Option<Arc<SubagentJournal>>,
    ) -> Result<Self, CommunicationError> {
        Self::with_timeout(
            conversation_id,
            parent_run_id,
            pool_id,
            policy_revision,
            journal,
            DEFAULT_MESSAGE_TIMEOUT_S,
        )
    }

    /// [`AgentMessageBroker::new`] with a different message lifetime.
    pub fn with_timeout(
        conversation_id: &str,
        parent_run_id: &str,
        pool_id: &str,
        policy_revision: &str,
        journal: Option<Arc<SubagentJournal>>,
        message_timeout_s: f64,
    ) -> Result<Self, CommunicationError> {
        if conversation_id.is_empty() {
            return err("conversation_id is required");
        }
        if pool_id.is_empty() {
            return err("pool_id is required");
        }
        if !(message_timeout_s > 0.0 && message_timeout_s.is_finite()) {
            return err("communication limits must be positive");
        }
        let state = BrokerState {
            members: BTreeSet::from([MAIN.to_string()]),
            ..Default::default()
        };
        Ok(Self {
            conversation_id: conversation_id.into(),
            parent_run_id: parent_run_id.into(),
            pool_id: pool_id.into(),
            policy_revision: policy_revision.into(),
            journal,
            message_timeout_s,
            state: Mutex::new(state),
            pending: watch::channel(0).0,
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, BrokerState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn bump(&self) {
        self.pending.send_modify(|v| *v = v.wrapping_add(1));
    }

    fn record(&self, kind: &str, payload: Value) {
        if let Some(journal) = &self.journal {
            journal.message_event(
                &self.pool_id,
                &self.conversation_id,
                &self.parent_run_id,
                &self.policy_revision,
                kind,
                payload,
            );
        }
    }

    /// The pool this broker routes.
    pub fn pool_id(&self) -> &str {
        &self.pool_id
    }

    /// A snapshot of the members (`main` and every registered worker).
    pub fn members(&self) -> BTreeSet<String> {
        self.state().members.clone()
    }

    /// Register a worker before its tools are exposed.
    pub fn register_worker(&self, worker_id: &str) -> Result<(), CommunicationError> {
        validate_identifier(worker_id, "worker_id")?;
        if worker_id == MAIN || worker_id == POOL {
            return err("worker_id is reserved");
        }
        {
            let mut state = self.state();
            if state.closed {
                return err("pool communication is closed");
            }
            state.members.insert(worker_id.to_string());
        }
        self.record(
            "agent_pool_member",
            json!({"worker_id": worker_id, "state": "registered"}),
        );
        Ok(())
    }

    /// Send one validated message and return its envelope.
    #[allow(clippy::too_many_arguments)]
    pub fn send(
        &self,
        sender: &str,
        recipient: &str,
        kind: &str,
        payload: Map<String, Value>,
        reply_to: &str,
        correlation_id: &str,
        requires_response: bool,
        ttl_s: Option<f64>,
    ) -> Result<AgentMessageEnvelope, CommunicationError> {
        let mut state = self.state();
        let envelope = self.send_locked(
            &mut state,
            sender,
            recipient,
            kind,
            payload,
            reply_to,
            correlation_id,
            requires_response,
            ttl_s,
        )?;
        drop(state);
        self.record("agent_message_sent", envelope.to_json());
        Ok(envelope)
    }

    #[allow(clippy::too_many_arguments)]
    fn send_locked(
        &self,
        state: &mut BrokerState,
        sender: &str,
        recipient: &str,
        kind: &str,
        payload: Map<String, Value>,
        reply_to: &str,
        correlation_id: &str,
        requires_response: bool,
        ttl_s: Option<f64>,
    ) -> Result<AgentMessageEnvelope, CommunicationError> {
        validate_identifier(sender, "sender")?;
        if !state.members.contains(sender) {
            return err("sender is not a member of this pool");
        }
        validate_identifier(recipient, "recipient")?;
        if recipient != POOL && !state.members.contains(recipient) {
            return err("recipient is not a member of this pool");
        }
        if !MESSAGE_KINDS.contains(&kind) {
            return err(format!("unknown message kind: '{kind}'"));
        }
        let encoded = serde_json::to_string(&payload).unwrap_or_default();
        if encoded.len() > MAX_MESSAGE_BYTES {
            return err("message payload exceeds the configured byte limit");
        }
        let targets: Vec<String> = if recipient == POOL {
            state
                .members
                .iter()
                .filter(|m| m.as_str() != sender)
                .cloned()
                .collect()
        } else {
            vec![recipient.to_string()]
        };
        if recipient == POOL && targets.is_empty() {
            return err("pool has no recipient workers");
        }
        if targets.iter().any(|t| {
            state
                .mailboxes
                .get(t)
                .is_some_and(|m| m.len() >= MAX_MAILBOX_MESSAGES)
        }) {
            return err("recipient mailbox is full");
        }
        if !correlation_id.is_empty() {
            let duplicate = state.messages.values().find(|r| {
                let e = &r.envelope;
                e.sender == sender
                    && e.recipient == recipient
                    && e.correlation_id == correlation_id
                    && e.kind == kind
                    && e.payload == payload
            });
            if let Some(existing) = duplicate {
                return Ok(existing.envelope.clone());
            }
        }
        let sequence = {
            let counter = state.sequence.entry(sender.to_string()).or_insert(0);
            *counter += 1;
            *counter
        };
        let created_at = now();
        let message_id = uuid::Uuid::new_v4().simple().to_string();
        let envelope = AgentMessageEnvelope {
            message_id: message_id.clone(),
            conversation_id: self.conversation_id.clone(),
            parent_run_id: self.parent_run_id.clone(),
            pool_id: self.pool_id.clone(),
            sender: sender.into(),
            recipient: recipient.into(),
            kind: kind.into(),
            payload,
            reply_to: reply_to.into(),
            correlation_id: if correlation_id.is_empty() {
                message_id.clone()
            } else {
                correlation_id.into()
            },
            sequence,
            created_at,
            expires_at: Some(created_at + ttl_s.unwrap_or(self.message_timeout_s)),
            requires_response,
            policy_revision: self.policy_revision.clone(),
        };
        state.messages.insert(
            message_id.clone(),
            MessageRecord {
                envelope: envelope.clone(),
                state: "queued",
            },
        );
        for target in targets {
            state
                .mailboxes
                .entry(target)
                .or_default()
                .push_back(message_id.clone());
        }
        Ok(envelope)
    }

    fn projection(&self, record: &QuestionRecord) -> Value {
        let payload = &record.envelope.payload;
        json!({
            "question_id": record.question_id,
            "pool_id": self.pool_id,
            "worker_id": record.envelope.sender,
            "recipient": record.envelope.recipient,
            "status": record.status,
            "question": payload.get("question").cloned().unwrap_or(json!("")),
            "context": payload.get("context").cloned().unwrap_or(json!("")),
            "expected_answer": payload.get("expected_answer").cloned().unwrap_or(json!("text")),
            "answer": record.answer,
            "correlation_id": record.envelope.correlation_id,
            "expires_at": record.envelope.expires_at,
        })
    }

    /// Open a question to the parent (`main`) or a peer.
    pub fn open_question(
        &self,
        sender: &str,
        recipient: &str,
        question: &str,
        context: &str,
        expected_answer: &str,
        timeout_s: f64,
    ) -> Result<Value, CommunicationError> {
        if question.trim().is_empty() {
            return err("question must not be empty");
        }
        if !(timeout_s > 0.0 && timeout_s.is_finite()) {
            return err("question timeout must be positive");
        }
        let mut state = self.state();
        let open = state
            .questions
            .iter()
            .filter(|q| q.status == "pending")
            .count();
        if open >= MAX_PENDING_QUESTIONS {
            return err("communication question quota exceeded");
        }
        let mut payload = Map::new();
        payload.insert(
            "question".into(),
            json!(truncate(question, MAX_MESSAGE_BYTES / 2)),
        );
        payload.insert(
            "context".into(),
            json!(truncate(context, MAX_MESSAGE_BYTES / 2)),
        );
        payload.insert(
            "expected_answer".into(),
            json!(truncate(expected_answer, 128)),
        );
        let envelope = self.send_locked(
            &mut state,
            sender,
            recipient,
            "clarification_request",
            payload,
            "",
            "",
            true,
            Some(timeout_s),
        )?;
        let record = QuestionRecord {
            question_id: envelope.message_id.clone(),
            envelope: envelope.clone(),
            status: "pending",
            answer: String::new(),
            waiters: Vec::new(),
        };
        let projection = self.projection(&record);
        state.questions.push(record);
        drop(state);
        self.record("agent_message_sent", envelope.to_json());
        self.record("question_wait_started", projection.clone());
        self.bump();
        Ok(projection)
    }

    fn finish_question(record: &mut QuestionRecord, answer: String, status: &'static str) {
        record.answer = answer.clone();
        record.status = status;
        for waiter in record.waiters.drain(..) {
            let _ = waiter.send(if status == "answered" {
                Ok(answer.clone())
            } else {
                Err(format!("question is {status}"))
            });
        }
    }

    /// Answer one open question; only its recipient (or, for a `pool`
    /// question, any member) may.
    pub fn answer_question(
        &self,
        answerer: &str,
        question_id: &str,
        answer: &str,
    ) -> Result<Value, CommunicationError> {
        if answer.trim().is_empty() {
            return err("answer must not be empty");
        }
        let mut state = self.state();
        let members = state.members.clone();
        let Some(record) = state
            .questions
            .iter_mut()
            .find(|q| q.question_id == question_id)
        else {
            return err("unknown question_id");
        };
        if record.status != "pending" {
            return Ok(self.projection(record));
        }
        let expected = record.envelope.recipient.clone();
        if answerer != expected && !(expected == POOL && members.contains(answerer)) {
            return err("answerer is not the question recipient");
        }
        Self::finish_question(record, truncate(answer, MAX_MESSAGE_BYTES), "answered");
        let projection = self.projection(record);
        drop(state);
        let mut journaled = projection.clone();
        journaled["answer"] = json!(truncate(answer, MAX_MESSAGE_BYTES));
        self.record("question_answered", journaled);
        self.bump();
        Ok(projection)
    }

    /// Wait for the answer to `question_id`, at most `timeout_s` (the
    /// broker's message lifetime by default); an expiry closes the question.
    pub async fn wait_for_answer(
        &self,
        question_id: &str,
        timeout_s: Option<f64>,
    ) -> Result<String, CommunicationError> {
        let receiver = {
            let mut state = self.state();
            let Some(record) = state
                .questions
                .iter_mut()
                .find(|q| q.question_id == question_id)
            else {
                return err("unknown question_id");
            };
            match record.status {
                "pending" => {
                    let (tx, rx) = oneshot::channel();
                    record.waiters.push(tx);
                    rx
                }
                "answered" => return Ok(record.answer.clone()),
                other => return err(format!("question is {other}")),
            }
        };
        let seconds = timeout_s.unwrap_or(self.message_timeout_s);
        let timeout = Duration::from_secs_f64(if seconds.is_finite() {
            seconds.clamp(0.0, MAX_WAIT_S)
        } else {
            MAX_WAIT_S
        });
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(Ok(answer))) => Ok(answer),
            Ok(Ok(Err(reason))) => err(reason),
            Ok(Err(_)) => err("question is cancelled"),
            Err(_) => {
                let projection = {
                    let mut state = self.state();
                    state
                        .questions
                        .iter_mut()
                        .find(|q| q.question_id == question_id && q.status == "pending")
                        .map(|record| {
                            Self::finish_question(record, String::new(), "expired");
                            self.projection(record)
                        })
                };
                if let Some(projection) = projection {
                    self.record("question_timed_out", projection);
                    self.bump();
                }
                err("question timed out")
            }
        }
    }

    /// Read (and mark delivered) up to `limit` queued messages for
    /// `recipient`, oldest first; at most 64 per call.
    pub fn poll(&self, recipient: &str, limit: usize) -> Result<Vec<Value>, CommunicationError> {
        if limit < 1 {
            return err("limit must be positive");
        }
        validate_identifier(recipient, "recipient")?;
        let mut delivered = Vec::new();
        let mut out = Vec::new();
        {
            let mut state = self.state();
            if recipient != POOL && !state.members.contains(recipient) {
                return err("recipient is not a member of this pool");
            }
            let clock = now();
            while out.len() < limit.min(64) {
                let next = state
                    .mailboxes
                    .get_mut(recipient)
                    .and_then(VecDeque::pop_front)
                    .or_else(|| {
                        (recipient != POOL)
                            .then(|| state.mailboxes.get_mut(POOL).and_then(VecDeque::pop_front))
                            .flatten()
                    });
                let Some(message_id) = next else {
                    break;
                };
                let Some(record) = state.messages.get_mut(&message_id) else {
                    continue;
                };
                if record.envelope.expires_at.is_some_and(|e| e <= clock) {
                    record.state = "expired";
                    continue;
                }
                record.state = "delivered";
                out.push(record.envelope.to_json());
                delivered.push(message_id);
            }
        }
        for message_id in delivered {
            self.record(
                "agent_message_delivered",
                json!({"message_id": message_id, "recipient": recipient}),
            );
        }
        Ok(out)
    }

    /// Open questions, optionally filtered by recipient and sender, oldest
    /// first; expired ones are left out.
    pub fn pending_questions(&self, recipient: Option<&str>, sender: Option<&str>) -> Vec<Value> {
        let clock = now();
        let state = self.state();
        state
            .questions
            .iter()
            .filter(|q| q.status == "pending")
            .filter(|q| recipient.is_none_or(|r| q.envelope.recipient == r))
            .filter(|q| sender.is_none_or(|s| q.envelope.sender == s))
            .filter(|q| q.envelope.expires_at.is_none_or(|e| e > clock))
            .map(|q| self.projection(q))
            .collect()
    }

    /// Every question `sender` asked, oldest first: `(question_id, status)`.
    pub fn questions_from(&self, sender: &str) -> Vec<(String, &'static str)> {
        self.state()
            .questions
            .iter()
            .filter(|q| q.envelope.sender == sender)
            .map(|q| (q.question_id.clone(), q.status))
            .collect()
    }

    /// Wait until the parent has at least one open question.
    pub async fn wait_for_pending(&self) -> Result<Vec<Value>, CommunicationError> {
        loop {
            let mut changes = self.pending.subscribe();
            let pending = self.pending_questions(Some(MAIN), None);
            if !pending.is_empty() {
                return Ok(pending);
            }
            if self.state().closed {
                return err("pool communication is closed");
            }
            if changes.changed().await.is_err() {
                return err("pool communication is closed");
            }
        }
    }

    /// Close the broker; every open question is cancelled.
    pub fn close(&self, reason: &str) {
        let cancelled: Vec<Value> = {
            let mut state = self.state();
            state.closed = true;
            let mut cancelled = Vec::new();
            for index in 0..state.questions.len() {
                if state.questions[index].status == "pending" {
                    Self::finish_question(&mut state.questions[index], String::new(), "cancelled");
                    cancelled.push(self.projection(&state.questions[index]));
                }
            }
            cancelled
        };
        for projection in cancelled {
            self.record("question_cancelled", projection);
        }
        tracing::debug!(pool = %self.pool_id, reason, "subagent pool communication closed");
        self.bump();
    }
}

// ── Continuations ────────────────────────────────────────────────────────────

/// A pool's eventual aggregate, awaitable more than once.
pub type PoolFuture = Shared<BoxFuture<'static, Result<AggregatedResult, String>>>;

/// A pool left running for a clarification answer (agenthicc's `PoolHandle`).
pub struct PoolHandle {
    /// The pool.
    pub pool_id: String,
    /// The session that owns it.
    pub conversation_id: String,
    /// Its broker.
    pub broker: Arc<AgentMessageBroker>,
    /// Its aggregate.
    pub result: PoolFuture,
    /// Stops it if the session ends first.
    pub abort: tokio::task::AbortHandle,
}

/// The session's pools awaiting answers (agenthicc's
/// `PoolContinuationRegistry`, one per session). Owned by the runtime, so a
/// pool outlives the turn that spawned it; dropped pools are aborted.
#[derive(Default)]
pub struct PoolContinuationRegistry {
    handles: Mutex<HashMap<String, PoolHandle>>,
}

impl std::fmt::Debug for PoolContinuationRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PoolContinuationRegistry")
            .field("pools", &self.lock().len())
            .finish()
    }
}

impl PoolContinuationRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, PoolHandle>> {
        self.handles.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Keep `handle` until it is collected.
    pub fn register(&self, handle: PoolHandle) {
        self.lock().insert(handle.pool_id.clone(), handle);
    }

    /// The broker of `pool_id`, if that pool belongs to `conversation_id`.
    pub fn broker(&self, pool_id: &str, conversation_id: &str) -> Option<Arc<AgentMessageBroker>> {
        self.lock()
            .get(pool_id)
            .filter(|h| h.conversation_id == conversation_id)
            .map(|h| Arc::clone(&h.broker))
    }

    /// Wait for `pool_id` to finish and forget it.
    pub async fn collect(
        &self,
        pool_id: &str,
        conversation_id: &str,
    ) -> Result<AggregatedResult, CommunicationError> {
        let result = self
            .lock()
            .get(pool_id)
            .filter(|h| h.conversation_id == conversation_id)
            .map(|h| h.result.clone());
        let Some(result) = result else {
            return err("unknown subagent pool for this session");
        };
        let outcome = result.await;
        self.lock().remove(pool_id);
        outcome.map_err(CommunicationError)
    }

    /// Pools still registered.
    pub fn pool_ids(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }
}

impl Drop for PoolContinuationRegistry {
    fn drop(&mut self) {
        for handle in self.lock().values() {
            handle.abort.abort();
            handle.broker.close("session_closed");
        }
    }
}

// ── Tools ────────────────────────────────────────────────────────────────────

fn string_arg(input: &Value, key: &str) -> Option<String> {
    match input.get(key) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

fn required(input: &Value, key: &str) -> Result<String, CommunicationError> {
    string_arg(input, key).ok_or_else(|| CommunicationError(format!("'{key}' is required")))
}

fn number_arg(input: &Value, key: &str, default: f64) -> Result<f64, CommunicationError> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Number(n)) => Ok(n.as_f64().unwrap_or(default)),
        Some(_) => err(format!("'{key}' must be a number")),
    }
}

fn text_payload(message: &str) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert("text".into(), json!(message));
    payload
}

fn reply(outcome: Result<Value, CommunicationError>, ctx: &ToolContext) -> ToolResult {
    match outcome {
        Ok(value) => ToolResult::ok(value.to_string(), &ctx.tool_use_id),
        Err(error) => ToolResult::error(error.0, &ctx.tool_use_id),
    }
}

fn with_ok(projection: Value) -> Value {
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    if let Value::Object(fields) = projection {
        out.extend(fields);
    }
    Value::Object(out)
}

/// The communication tool kinds, one struct with a discriminant: each tool is
/// a few lines of routing over the broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    SendParentMessage,
    AskParent,
    PollAgentMessages,
    SendPeerMessage,
    AskPeer,
    AnswerPeer,
    AnswerSubagent,
    SendSubagentMessage,
    CollectSubagentResults,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::SendParentMessage => "send_parent_message",
            Self::AskParent => "ask_parent",
            Self::PollAgentMessages => "poll_agent_messages",
            Self::SendPeerMessage => "send_peer_message",
            Self::AskPeer => "ask_peer",
            Self::AnswerPeer => "answer_peer",
            Self::AnswerSubagent => "answer_subagent",
            Self::SendSubagentMessage => "send_subagent_message",
            Self::CollectSubagentResults => "collect_subagent_results",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::SendParentMessage => "Send a bounded status or finding to the main agent.",
            Self::AskParent => "Ask the main agent for missing information and await continuation.",
            Self::PollAgentMessages => "Read ordered parent and peer messages for this worker.",
            Self::SendPeerMessage => "Send a bounded message to another worker in this pool.",
            Self::AskPeer => "Ask a same-pool peer for information.",
            Self::AnswerPeer => "Answer a question addressed to this worker.",
            Self::AnswerSubagent => "Answer a clarification raised by one of your subagents.",
            Self::SendSubagentMessage => {
                "Send a bounded instruction or clarification to one worker."
            }
            Self::CollectSubagentResults => {
                "Collect a pool that was left running for a clarification answer."
            }
        }
    }

    fn schema(self) -> Value {
        let kind = json!({"type": "string", "enum": MESSAGE_KINDS});
        let (properties, required) = match self {
            Self::SendParentMessage => (
                json!({"message": {"type": "string"}, "kind": {"default": "information", "type": "string", "enum": MESSAGE_KINDS}}),
                vec!["message"],
            ),
            Self::AskParent => (
                json!({
                    "question": {"type": "string"},
                    "context": {"type": "string", "default": ""},
                    "expected_answer": {"type": "string", "default": "text"},
                    "timeout_s": {"type": "number", "default": DEFAULT_MESSAGE_TIMEOUT_S},
                }),
                vec!["question"],
            ),
            Self::PollAgentMessages => {
                (json!({"limit": {"type": "integer", "default": 16}}), vec![])
            }
            Self::SendPeerMessage => (
                json!({"recipient": {"type": "string", "description": "A peer's worker id (task-N) or 'pool' for every peer."}, "message": {"type": "string"}, "kind": kind}),
                vec!["recipient", "message"],
            ),
            Self::AskPeer => (
                json!({
                    "recipient": {"type": "string", "description": "A peer's worker id (task-N)."},
                    "question": {"type": "string"},
                    "context": {"type": "string", "default": ""},
                    "timeout_s": {"type": "number", "default": DEFAULT_MESSAGE_TIMEOUT_S},
                }),
                vec!["recipient", "question"],
            ),
            Self::AnswerPeer => (
                json!({"question_id": {"type": "string"}, "answer": {"type": "string"}}),
                vec!["question_id", "answer"],
            ),
            Self::AnswerSubagent => (
                json!({"pool_id": {"type": "string"}, "question_id": {"type": "string"}, "answer": {"type": "string"}}),
                vec!["pool_id", "question_id", "answer"],
            ),
            Self::SendSubagentMessage => (
                json!({
                    "pool_id": {"type": "string"},
                    "worker_id": {"type": "string", "description": "The worker's id (task-N)."},
                    "message": {"type": "string"},
                    "kind": {"type": "string", "enum": MESSAGE_KINDS, "default": "instruction"},
                }),
                vec!["pool_id", "worker_id", "message"],
            ),
            Self::CollectSubagentResults => {
                (json!({"pool_id": {"type": "string"}}), vec!["pool_id"])
            }
        };
        json!({"type": "object", "properties": properties, "required": required})
    }
}

/// Who a communication tool acts as.
enum Binding {
    /// A worker of one pool.
    Worker {
        broker: Arc<AgentMessageBroker>,
        worker_id: String,
    },
    /// The parent of a session's pools.
    Parent {
        registry: Arc<PoolContinuationRegistry>,
        conversation_id: String,
    },
}

/// One communication tool (control capability: it advances only the pool's
/// own message state, never the project).
pub struct CommunicationTool {
    kind: Kind,
    binding: Binding,
}

impl std::fmt::Debug for CommunicationTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommunicationTool")
            .field("name", &self.kind.name())
            .finish()
    }
}

/// The six tools of one worker (agenthicc's `make_child_communication_tools`).
pub fn child_tools(broker: &Arc<AgentMessageBroker>, worker_id: &str) -> Vec<Arc<ErasedTool>> {
    [
        Kind::SendParentMessage,
        Kind::AskParent,
        Kind::PollAgentMessages,
        Kind::SendPeerMessage,
        Kind::AskPeer,
        Kind::AnswerPeer,
    ]
    .into_iter()
    .map(|kind| {
        Arc::new(CommunicationTool {
            kind,
            binding: Binding::Worker {
                broker: Arc::clone(broker),
                worker_id: worker_id.to_string(),
            },
        }) as Arc<ErasedTool>
    })
    .collect()
}

/// The parent's three tools (agenthicc's `make_parent_communication_tools`).
pub fn parent_tools(
    registry: &Arc<PoolContinuationRegistry>,
    conversation_id: &str,
) -> Vec<Arc<ErasedTool>> {
    [
        Kind::AnswerSubagent,
        Kind::SendSubagentMessage,
        Kind::CollectSubagentResults,
    ]
    .into_iter()
    .map(|kind| {
        Arc::new(CommunicationTool {
            kind,
            binding: Binding::Parent {
                registry: Arc::clone(registry),
                conversation_id: conversation_id.to_string(),
            },
        }) as Arc<ErasedTool>
    })
    .collect()
}

/// The tool names [`child_tools`] builds.
pub const CHILD_TOOL_NAMES: &[&str] = &[
    "send_parent_message",
    "ask_parent",
    "poll_agent_messages",
    "send_peer_message",
    "ask_peer",
    "answer_peer",
];

/// The tool names [`parent_tools`] builds.
pub const PARENT_TOOL_NAMES: &[&str] = &[
    "answer_subagent",
    "send_subagent_message",
    "collect_subagent_results",
];

impl CommunicationTool {
    async fn run(&self, input: &Value) -> Result<Value, CommunicationError> {
        match (&self.binding, self.kind) {
            (Binding::Worker { broker, worker_id }, kind) => {
                let me = worker_id.as_str();
                match kind {
                    Kind::SendParentMessage | Kind::SendPeerMessage => {
                        let recipient = if kind == Kind::SendParentMessage {
                            MAIN.to_string()
                        } else {
                            required(input, "recipient")?
                        };
                        let message = required(input, "message")?;
                        let message_kind =
                            string_arg(input, "kind").unwrap_or_else(|| "information".into());
                        let envelope = broker.send(
                            me,
                            &recipient,
                            &message_kind,
                            text_payload(&message),
                            "",
                            "",
                            false,
                            None,
                        )?;
                        Ok(json!({"ok": true, "message_id": envelope.message_id, "status": "sent"}))
                    }
                    Kind::AskParent | Kind::AskPeer => {
                        let recipient = if kind == Kind::AskParent {
                            MAIN.to_string()
                        } else {
                            required(input, "recipient")?
                        };
                        let question = required(input, "question")?;
                        let context = string_arg(input, "context").unwrap_or_default();
                        let expected =
                            string_arg(input, "expected_answer").unwrap_or_else(|| "text".into());
                        let timeout_s = number_arg(input, "timeout_s", DEFAULT_MESSAGE_TIMEOUT_S)?;
                        broker.open_question(
                            me, &recipient, &question, &context, &expected, timeout_s,
                        )
                    }
                    Kind::PollAgentMessages => {
                        let limit = match input.get("limit") {
                            None | Some(Value::Null) => 16,
                            Some(Value::Number(n)) if n.as_u64().is_some() => {
                                n.as_u64().unwrap_or(16) as usize
                            }
                            Some(_) => return err("limit must be positive"),
                        };
                        let messages = broker.poll(me, limit)?;
                        Ok(json!({"ok": true, "messages": messages}))
                    }
                    Kind::AnswerPeer => {
                        let question_id = required(input, "question_id")?;
                        let answer = required(input, "answer")?;
                        Ok(with_ok(broker.answer_question(
                            me,
                            &question_id,
                            &answer,
                        )?))
                    }
                    _ => err("not a worker tool"),
                }
            }
            (
                Binding::Parent {
                    registry,
                    conversation_id,
                },
                kind,
            ) => {
                let pool_id = required(input, "pool_id")?;
                if kind == Kind::CollectSubagentResults {
                    let result = registry.collect(&pool_id, conversation_id).await?;
                    return Ok(json!({
                        "ok": result.failed == 0,
                        "pool_id": pool_id,
                        "total": result.total,
                        "succeeded": result.succeeded,
                        "failed": result.failed,
                        "results": result.text,
                    }));
                }
                let broker = registry.broker(&pool_id, conversation_id).ok_or_else(|| {
                    CommunicationError("unknown subagent pool for this session".into())
                })?;
                match kind {
                    Kind::AnswerSubagent => {
                        let question_id = required(input, "question_id")?;
                        let answer = required(input, "answer")?;
                        Ok(with_ok(broker.answer_question(
                            MAIN,
                            &question_id,
                            &answer,
                        )?))
                    }
                    Kind::SendSubagentMessage => {
                        let worker_id = required(input, "worker_id")?;
                        let message = required(input, "message")?;
                        let message_kind =
                            string_arg(input, "kind").unwrap_or_else(|| "instruction".into());
                        let envelope = broker.send(
                            MAIN,
                            &worker_id,
                            &message_kind,
                            text_payload(&message),
                            "",
                            "",
                            false,
                            None,
                        )?;
                        Ok(json!({"ok": true, "message_id": envelope.message_id, "status": "sent"}))
                    }
                    _ => err("not a parent tool"),
                }
            }
        }
    }
}

#[async_trait]
impl Tool for CommunicationTool {
    type Inputs = Value;
    type Output = ToolResult;

    fn name(&self) -> &str {
        self.kind.name()
    }

    fn description(&self) -> &str {
        self.kind.description()
    }

    fn schema(&self) -> Value {
        self.kind.schema()
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, AgtrsError> {
        if ctx.is_cancelled() {
            return Err(AgtrsError::Cancelled {
                reason: format!("{} cancelled", self.kind.name()),
            });
        }
        let outcome = tokio::select! {
            biased;
            _ = ctx.cancel_token.cancelled() => {
                return Err(AgtrsError::Cancelled {
                    reason: format!("{} cancelled", self.kind.name()),
                });
            }
            outcome = self.run(&input) => outcome,
        };
        Ok(reply(outcome, ctx))
    }

    fn parallel_safe(&self) -> bool {
        false
    }
}

/// Wrap a pool task so it can be awaited by the spawning call and, later,
/// by `collect_subagent_results`.
pub fn shared_pool(task: tokio::task::JoinHandle<AggregatedResult>) -> PoolFuture {
    task.map(|joined| joined.map_err(|e| format!("subagent pool ended unexpectedly: {e}")))
        .boxed()
        .shared()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn broker() -> AgentMessageBroker {
        let broker = AgentMessageBroker::new("conv", "run", "pool-1", "rev", None).unwrap();
        broker.register_worker("task-0").unwrap();
        broker.register_worker("task-1").unwrap();
        broker
    }

    #[test]
    fn messages_are_routed_validated_and_delivered_once() {
        let b = broker();
        b.send(
            "main",
            "task-0",
            "instruction",
            text_payload("go"),
            "",
            "",
            false,
            None,
        )
        .unwrap();
        assert_eq!(
            b.send(
                "task-0",
                "nobody",
                "information",
                text_payload("x"),
                "",
                "",
                false,
                None
            )
            .unwrap_err()
            .0,
            "recipient is not a member of this pool"
        );
        assert_eq!(
            b.send(
                "ghost",
                "main",
                "information",
                text_payload("x"),
                "",
                "",
                false,
                None
            )
            .unwrap_err()
            .0,
            "sender is not a member of this pool"
        );
        assert_eq!(
            b.send(
                "task-0",
                "main",
                "gossip",
                text_payload("x"),
                "",
                "",
                false,
                None
            )
            .unwrap_err()
            .0,
            "unknown message kind: 'gossip'"
        );
        let big = "x".repeat(MAX_MESSAGE_BYTES);
        assert_eq!(
            b.send(
                "task-0",
                "main",
                "information",
                text_payload(&big),
                "",
                "",
                false,
                None
            )
            .unwrap_err()
            .0,
            "message payload exceeds the configured byte limit"
        );
        let polled = b.poll("task-0", 16).unwrap();
        assert_eq!(polled.len(), 1);
        assert_eq!(polled[0]["payload"]["text"], "go");
        assert_eq!(polled[0]["sender"], "main");
        assert!(b.poll("task-0", 16).unwrap().is_empty(), "delivered once");
        assert_eq!(b.poll("task-0", 0).unwrap_err().0, "limit must be positive");
    }

    #[test]
    fn a_pool_broadcast_reaches_every_other_member() {
        let b = broker();
        b.send(
            "task-0",
            "pool",
            "status",
            text_payload("hi all"),
            "",
            "",
            false,
            None,
        )
        .unwrap();
        assert_eq!(b.poll("task-1", 16).unwrap().len(), 1);
        assert_eq!(b.poll("main", 16).unwrap().len(), 1);
        assert!(
            b.poll("task-0", 16).unwrap().is_empty(),
            "not to the sender"
        );
    }

    #[test]
    fn a_correlation_id_deduplicates_a_retried_send() {
        let b = broker();
        let first = b
            .send(
                "main",
                "task-0",
                "instruction",
                text_payload("go"),
                "",
                "c-1",
                false,
                None,
            )
            .unwrap();
        let again = b
            .send(
                "main",
                "task-0",
                "instruction",
                text_payload("go"),
                "",
                "c-1",
                false,
                None,
            )
            .unwrap();
        assert_eq!(first.message_id, again.message_id);
        assert_eq!(b.poll("task-0", 16).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_question_is_answered_by_its_recipient_only() {
        let b = broker();
        let q = b
            .open_question("task-0", "main", "Which file?", "", "text", 60.0)
            .unwrap();
        let id = q["question_id"].as_str().unwrap().to_string();
        assert_eq!(q["status"], "pending");
        assert_eq!(b.pending_questions(Some("main"), None).len(), 1);
        assert_eq!(
            b.answer_question("task-1", &id, "x").unwrap_err().0,
            "answerer is not the question recipient"
        );
        assert_eq!(
            b.answer_question("main", &id, " ").unwrap_err().0,
            "answer must not be empty"
        );
        let answered = b.answer_question("main", &id, "src/lib.rs").unwrap();
        assert_eq!(answered["status"], "answered");
        assert_eq!(answered["answer"], "src/lib.rs");
        assert_eq!(
            b.wait_for_answer(&id, Some(1.0)).await.unwrap(),
            "src/lib.rs"
        );
        assert!(b.pending_questions(Some("main"), None).is_empty());
    }

    #[tokio::test]
    async fn waiting_resolves_on_answer_and_expires_on_timeout() {
        let b = Arc::new(broker());
        let q = b
            .open_question("task-0", "main", "Proceed?", "", "text", 60.0)
            .unwrap();
        let id = q["question_id"].as_str().unwrap().to_string();
        let waiter = {
            let b = Arc::clone(&b);
            let id = id.clone();
            tokio::spawn(async move { b.wait_for_answer(&id, Some(5.0)).await })
        };
        tokio::task::yield_now().await;
        b.answer_question("main", &id, "yes").unwrap();
        assert_eq!(waiter.await.unwrap().unwrap(), "yes");

        let q = b
            .open_question("task-1", "main", "Still there?", "", "text", 60.0)
            .unwrap();
        let id = q["question_id"].as_str().unwrap();
        assert_eq!(
            b.wait_for_answer(id, Some(0.05)).await.unwrap_err().0,
            "question timed out"
        );
        assert_eq!(
            b.wait_for_answer(id, Some(0.05)).await.unwrap_err().0,
            "question is expired"
        );
    }

    #[tokio::test]
    async fn wait_for_pending_wakes_on_a_parent_question_and_close_cancels() {
        let b = Arc::new(broker());
        let waiting = {
            let b = Arc::clone(&b);
            tokio::spawn(async move { b.wait_for_pending().await })
        };
        tokio::task::yield_now().await;
        // A peer question does not wake the parent.
        b.open_question("task-0", "task-1", "peer?", "", "text", 60.0)
            .unwrap();
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        b.open_question("task-1", "main", "parent?", "", "text", 60.0)
            .unwrap();
        let pending = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["worker_id"], "task-1");

        b.close("done");
        assert!(b.pending_questions(None, None).is_empty());
        assert_eq!(
            b.wait_for_pending().await.unwrap_err().0,
            "pool communication is closed"
        );
    }

    #[test]
    fn the_question_quota_is_sixteen() {
        let b = broker();
        for i in 0..MAX_PENDING_QUESTIONS {
            b.open_question("task-0", "main", &format!("q{i}"), "", "text", 60.0)
                .unwrap();
        }
        assert_eq!(
            b.open_question("task-0", "main", "one more", "", "text", 60.0)
                .unwrap_err()
                .0,
            "communication question quota exceeded"
        );
    }

    #[test]
    fn reserved_and_empty_worker_ids_are_refused() {
        let b = broker();
        assert_eq!(
            b.register_worker("main").unwrap_err().0,
            "worker_id is reserved"
        );
        assert_eq!(
            b.register_worker("").unwrap_err().0,
            "worker_id must be a non-empty bounded string"
        );
        assert!(AgentMessageBroker::new("", "r", "p", "", None).is_err());
    }

    #[tokio::test]
    async fn every_change_is_journaled_as_an_agent_message_event() {
        let dir = tempfile::TempDir::new().unwrap();
        let journal = Arc::new(SubagentJournal::for_session(dir.path(), "conv"));
        let b = AgentMessageBroker::new("conv", "run", "pool-1", "rev", Some(Arc::clone(&journal)))
            .unwrap();
        b.register_worker("task-0").unwrap();
        b.send(
            "task-0",
            "main",
            "status",
            text_payload("half way"),
            "",
            "",
            false,
            None,
        )
        .unwrap();
        let q = b
            .open_question("task-0", "main", "ok?", "", "text", 60.0)
            .unwrap();
        b.answer_question("main", q["question_id"].as_str().unwrap(), "fine")
            .unwrap();
        let kinds: Vec<String> = journal
            .message_events()
            .iter()
            .map(|e| e["event_kind"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            kinds,
            [
                "agent_pool_member",
                "agent_message_sent",
                "agent_message_sent",
                "question_wait_started",
                "question_answered"
            ]
        );
        assert_eq!(journal.message_events()[1]["pool_id"], "pool-1");
    }

    #[tokio::test]
    async fn the_worker_tools_route_through_the_broker() {
        let b = Arc::new(broker());
        let tools = child_tools(&b, "task-0");
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, CHILD_TOOL_NAMES);
        let ctx = ToolContext::new("t1");
        let sent = tools[0]
            .call(json!({"message": "found the bug"}), &ctx)
            .await
            .unwrap();
        assert!(!sent.is_error, "{}", sent.content);
        let sent: Value = serde_json::from_str(&sent.content).unwrap();
        assert_eq!(sent["status"], "sent");
        assert_eq!(
            b.poll("main", 16).unwrap()[0]["payload"]["text"],
            "found the bug"
        );

        let asked = tools[4]
            .call(
                json!({"recipient": "task-1", "question": "which test?"}),
                &ctx,
            )
            .await
            .unwrap();
        let asked: Value = serde_json::from_str(&asked.content).unwrap();
        assert_eq!(asked["recipient"], "task-1");
        let peer = child_tools(&b, "task-1");
        let answered = peer[5]
            .call(
                json!({"question_id": asked["question_id"], "answer": "the unit one"}),
                &ctx,
            )
            .await
            .unwrap();
        let answered: Value = serde_json::from_str(&answered.content).unwrap();
        assert_eq!(answered["ok"], true);
        assert_eq!(answered["status"], "answered");

        let bad = tools[3]
            .call(json!({"recipient": "task-9", "message": "hi"}), &ctx)
            .await
            .unwrap();
        assert!(bad.is_error);
        assert_eq!(bad.content, "recipient is not a member of this pool");
    }

    #[tokio::test]
    async fn the_parent_tools_reach_only_this_sessions_pools() {
        let registry = Arc::new(PoolContinuationRegistry::default());
        let b = Arc::new(broker());
        let question = b
            .open_question("task-0", "main", "Which branch?", "", "text", 60.0)
            .unwrap();
        let aggregate = AggregatedResult {
            pool_id: "pool-1".into(),
            total: 1,
            succeeded: 1,
            failed: 0,
            text: "=== explorer #1 (✓ 0.1s) ===\ndone".into(),
        };
        let task = tokio::spawn(async move { aggregate });
        let abort = task.abort_handle();
        registry.register(PoolHandle {
            pool_id: "pool-1".into(),
            conversation_id: "conv".into(),
            broker: Arc::clone(&b),
            result: shared_pool(task),
            abort,
        });
        let ctx = ToolContext::new("t");
        let foreign = parent_tools(&registry, "other-session");
        let refused = foreign[0]
            .call(
                json!({"pool_id": "pool-1", "question_id": question["question_id"], "answer": "main"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(refused.is_error);
        assert_eq!(refused.content, "unknown subagent pool for this session");

        let tools = parent_tools(&registry, "conv");
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, PARENT_TOOL_NAMES);
        let answered = tools[0]
            .call(
                json!({"pool_id": "pool-1", "question_id": question["question_id"], "answer": "main"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!answered.is_error, "{}", answered.content);
        let sent = tools[1]
            .call(
                json!({"pool_id": "pool-1", "worker_id": "task-0", "message": "also check docs"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!sent.is_error, "{}", sent.content);
        assert_eq!(b.poll("task-0", 16).unwrap()[0]["kind"], "instruction");
        let collected = tools[2]
            .call(json!({"pool_id": "pool-1"}), &ctx)
            .await
            .unwrap();
        let collected: Value = serde_json::from_str(&collected.content).unwrap();
        assert_eq!(collected["ok"], true);
        assert_eq!(collected["succeeded"], 1);
        assert!(
            registry.pool_ids().is_empty(),
            "collected pools are forgotten"
        );
        let again = tools[2]
            .call(json!({"pool_id": "pool-1"}), &ctx)
            .await
            .unwrap();
        assert!(again.is_error);
    }
}
