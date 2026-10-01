//! `/replay` handler — push a previous session's conversation into the transcript.
//!
//! agenthicc's `_cmd_replay` (`commands/builtins.py`) does three things, and
//! this handler does the same three, in the same order:
//!
//! 1. With no argument, resolve the most recent session **for the current
//!    directory, excluding the current session**. If there is none, print the
//!    usage line. (`find_latest_session_for_cwd()` + an explicit
//!    `session_id == ctx.session_id` guard.)
//! 2. Check the session exists. If not, print the not-found message with the
//!    id truncated to 16 characters, plus the "where to look" hint.
//!    (agenthicc checks `get_session_log_path(id).exists()`; xaft's equivalent
//!    is a `load` against the session store.)
//! 3. Otherwise push the replay. (agenthicc sets `pending_replay`; xaft
//!    returns the rendered lines, which `AppState::apply_command_result`
//!    commits to the transcript.)
//!
//! **Source of the history.** This reads the session store's own conversation
//! — the key `xaft_session::conversation_key_for(id)` derives, i.e. the bare
//! session id — which is the same store and the same key `xaft session
//! inspect` reads. It deliberately does *not* reuse the `--resume` path, which
//! projects the agent's in-memory `{session}::workflow::planner` keys: that is
//! Gap 4 in `knowledge/tui-parity-gap-analysis.md` and stays open.
//!
//! **Current session is never auto-selected.** `SessionStore::list` returns
//! most-recently-updated first, so the naive `list(..).first()` would pick the
//! session the user is looking at and replay it back at them. agenthicc
//! guards against exactly that (`session_id == ctx.session_id`), and so does
//! the no-argument path below.

use agtrs_runtime::transport::Message;
use xaft_runtime::session::SessionId;

use crate::slash::registry::SlashHandler;
use crate::slash::{CommandContext, CommandResult};

/// Shown when `/replay` is given no argument and this directory has no
/// earlier session to replay. Mirrors agenthicc's line verbatim (its rich
/// markup renders as plain text here).
const USAGE: &str = "Usage: /replay <session-id> — no previous session found for this directory.";

/// How many characters of a session id the not-found message shows.
const NOT_FOUND_ID_LEN: usize = 16;

/// Truncate a session id for display, the way agenthicc truncates to 16.
fn short_id(id: &str) -> String {
    id.chars().take(NOT_FOUND_ID_LEN).collect()
}

pub struct ReplayHandler;

impl SlashHandler for ReplayHandler {
    fn description(&self) -> &'static str {
        "Replay a previous session's conversation in the transcript"
    }

    fn args_hint(&self) -> Option<&'static str> {
        Some("[session-id]")
    }

    fn is_async(&self) -> bool {
        true
    }

    fn execute(&self, _ctx: CommandContext) -> CommandResult {
        CommandResult::Error("use async execute".into())
    }

    fn execute_boxed_async(
        &self,
        ctx: CommandContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = CommandResult> + Send + 'static>> {
        Box::pin(async move {
            let Some(store) = ctx.session_store.clone() else {
                return CommandResult::Error("No session store available to replay from.".into());
            };

            // ── 1. Resolve the target session ─────────────────────────────────
            let requested = ctx.args.trim().to_string();
            let session_id = if requested.is_empty() {
                let sessions = match store.list(Some(&ctx.working_dir)).await {
                    Ok(s) => s,
                    Err(e) => {
                        return CommandResult::Error(format!("Could not list sessions: {e}"));
                    }
                };
                let current = ctx.session_id.as_deref();
                sessions
                    .into_iter()
                    .find(|s| Some(s.id.as_str()) != current)
                    .map(|s| s.id.to_string())
            } else {
                Some(requested)
            };

            let Some(session_id) = session_id else {
                return CommandResult::Lines(vec![USAGE.to_string()]);
            };

            // ── 2. Existence check ───────────────────────────────────────────
            let id = SessionId::from_string(session_id.clone());
            match store.load(&id).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return CommandResult::Lines(vec![
                        format!("Session {} not found.", short_id(&session_id)),
                        format!(
                            "Check {} for available session IDs.",
                            ctx.config.core.data_dir.display()
                        ),
                    ]);
                }
                Err(e) => {
                    return CommandResult::Error(format!("Could not read session store: {e}"));
                }
            }

            // ── 3. Push the replay from the session store's real history ─────
            let messages: Vec<Message> = match ctx.conversation_store.as_ref() {
                Some(conv) => conv.load(&session_id).await.unwrap_or_default(),
                None => Vec::new(),
            };
            if messages.is_empty() {
                return CommandResult::Lines(vec![format!(
                    "Session {} has no recorded conversation to replay.",
                    short_id(&session_id)
                )]);
            }

            CommandResult::StyledLines(crate::replay::render_replay_lines(
                &messages,
                &session_id,
                "replayed",
                false,
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slash::AgentStatsMap;
    use crate::slash::SlashCommand;
    use agtrs_runtime::signals::SignalBus;
    use agtrs_runtime::transport::Message;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, RwLock};
    use xaft_config::XaftConfig;
    use xaft_runtime::session::AgentSession;
    use xaft_runtime::session_store::{InMemorySessionStore, SessionStore};

    // ── Minimal ConversationStore, so these tests exercise the real load key.

    #[derive(Default)]
    struct MemConv {
        inner: tokio::sync::RwLock<std::collections::HashMap<String, Vec<Message>>>,
    }

    #[async_trait::async_trait]
    impl ConversationStoreTrait for MemConv {
        async fn load(&self, id: &str) -> Result<Vec<Message>, agtrs_runtime::error::AgtrsError> {
            Ok(self.inner.read().await.get(id).cloned().unwrap_or_default())
        }
        async fn save(
            &self,
            id: &str,
            messages: &[Message],
        ) -> Result<(), agtrs_runtime::error::AgtrsError> {
            self.inner
                .write()
                .await
                .insert(id.to_string(), messages.to_vec());
            Ok(())
        }
        async fn delete(&self, id: &str) -> Result<(), agtrs_runtime::error::AgtrsError> {
            self.inner.write().await.remove(id);
            Ok(())
        }
    }

    use agtrs_runtime::memory::ConversationStore as ConversationStoreTrait;

    struct Fixture {
        sessions: Arc<InMemorySessionStore>,
        conversations: Arc<MemConv>,
        working_dir: PathBuf,
    }

    /// Seed `n` sessions for `working_dir`, oldest first, each with one
    /// user/assistant exchange. Returns their ids in creation order.
    async fn seed(fx: &Fixture, n: usize, tag: &str) -> Vec<String> {
        let mut ids = Vec::new();
        for i in 0..n {
            let mut s = AgentSession::new(
                format!("{tag} task {i}"),
                fx.working_dir.clone(),
                "default".into(),
                "model".into(),
            );
            // Strictly increasing so "most recent" is unambiguous.
            s.updated_at = chrono::Utc::now() + chrono::Duration::seconds(i as i64);
            let id = s.id.to_string();
            fx.sessions.save(&s).await.unwrap();
            fx.conversations
                .save(
                    &id,
                    &[
                        Message::user(&format!("{tag} question {i}")),
                        Message::assistant(&format!("{tag} answer {i}")),
                    ],
                )
                .await
                .unwrap();
            ids.push(id);
        }
        ids
    }

    async fn fixture() -> Fixture {
        Fixture {
            sessions: Arc::new(InMemorySessionStore::new()),
            conversations: Arc::new(MemConv::default()),
            working_dir: PathBuf::from("/proj/replay-fixture"),
        }
    }

    fn ctx<'a>(fx: &'a Fixture, args: &str, session_id: Option<String>) -> CommandContext {
        CommandContext {
            args: args.to_string(),
            command: SlashCommand::Replay { id: None },
            signals: Arc::new(SignalBus::new()),
            config: Arc::new(XaftConfig::default()),
            session_id,
            working_dir: fx.working_dir.clone(),
            terminal_cols: 80,
            llm_stats: Arc::new(RwLock::new(AgentStatsMap::new())),
            conversation_store: Some(Arc::clone(&fx.conversations) as Arc<_>),
            session_store: Some(Arc::clone(&fx.sessions) as Arc<_>),
            intent_active: Arc::new(AtomicBool::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            input_history: None,
        }
    }

    async fn run(fx: &Fixture, args: &str, session_id: Option<String>) -> CommandResult {
        ReplayHandler
            .execute_boxed_async(ctx(fx, args, session_id))
            .await
    }

    fn texts(result: CommandResult) -> String {
        match result {
            CommandResult::Lines(v) => v.join("\n"),
            CommandResult::StyledLines(v) => v
                .iter()
                .map(|l| l.text.clone())
                .collect::<Vec<_>>()
                .join("\n"),
            CommandResult::Error(e) => e,
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[tokio::test]
    async fn no_argument_with_no_prior_session_prints_the_usage_line() {
        let fx = fixture().await;
        let out = texts(run(&fx, "", None).await);
        assert_eq!(out, USAGE);
    }

    #[tokio::test]
    async fn no_argument_never_selects_the_current_session() {
        let fx = fixture().await;
        let ids = seed(&fx, 2, "two").await;
        // Make the SECOND (newer) session the active one — the naive
        // "most recent wins" answer would replay it.
        let current = ids[1].clone();
        let out = texts(run(&fx, "", Some(current.clone())).await);
        assert!(
            out.contains(&ids[0][..8]),
            "the banner must name the earlier session, not the current one: {out}"
        );
        assert!(
            out.contains("❯ two question 0"),
            "the earlier session's turns must be shown: {out}"
        );
        assert!(
            !out.contains("two question 1") && !out.contains(&current[..8]),
            "must not replay the current session: {out}"
        );
    }

    #[tokio::test]
    async fn a_lone_current_session_reports_the_usage_line() {
        let fx = fixture().await;
        let ids = seed(&fx, 1, "solo").await;
        let out = texts(run(&fx, "", Some(ids[0].clone())).await);
        assert_eq!(
            out, USAGE,
            "the only session is the current one, so there is nothing to replay"
        );
    }

    #[tokio::test]
    async fn unknown_id_reports_not_found_with_the_id_truncated_to_16() {
        let fx = fixture().await;
        let long = "0123456789abcdef-more-than-sixteen-characters";
        let out = texts(run(&fx, long, None).await);
        assert!(
            out.starts_with("Session 0123456789abcdef not found."),
            "id must be truncated to 16 chars: {out}"
        );
        assert!(!out.contains("more-than-sixteen"), "full id leaked: {out}");
        assert!(out.contains("Check "), "expected the location hint: {out}");
    }

    #[tokio::test]
    async fn an_existing_session_replays_its_real_turns() {
        let fx = fixture().await;
        let ids = seed(&fx, 1, "good").await;
        let out = texts(run(&fx, &ids[0], None).await);
        assert!(
            out.contains("replayed: "),
            "expected a replay banner: {out}"
        );
        assert!(out.contains("❯ good question 0"), "{out}");
        assert!(out.contains("good answer 0"), "{out}");
    }

    #[tokio::test]
    async fn an_existing_session_with_no_conversation_says_so_instead_of_a_blank_replay() {
        let fx = fixture().await;
        let s = AgentSession::new(
            "no history",
            fx.working_dir.clone(),
            "default".into(),
            "model".into(),
        );
        fx.sessions.save(&s).await.unwrap();
        let out = texts(run(&fx, s.id.as_str(), None).await);
        assert!(
            out.contains("no recorded conversation to replay"),
            "an empty replay must not look like a working feature: {out}"
        );
    }

    #[tokio::test]
    async fn a_missing_session_store_is_reported_not_panicked() {
        let mut fx = fixture().await;
        fx.sessions = Arc::new(InMemorySessionStore::new());
        let mut c = ctx(&fx, "", None);
        c.session_store = None;
        let out = texts(ReplayHandler.execute_boxed_async(c).await);
        assert!(out.contains("No session store"), "{out}");
    }

    #[tokio::test]
    async fn sessions_from_another_directory_are_not_candidates() {
        let fx = fixture().await;
        seed(&fx, 1, "scoped").await;
        let other = Fixture {
            sessions: Arc::clone(&fx.sessions),
            conversations: Arc::clone(&fx.conversations),
            working_dir: PathBuf::from("/somewhere/else"),
        };
        let out = texts(run(&other, "", None).await);
        assert_eq!(
            out, USAGE,
            "scoping is by working directory, not by 'any session'"
        );
    }
}
