//! Questions to the user — the channel behind `ask_user`.
//!
//! agtrs's `ApprovalGate` answers yes/no only. agenthicc's agents also ask
//! structured questions ("which database should this use?" with a few
//! choices) and must behave well when nobody answers: a dismissed round is
//! `cancelled`, an unanswered one `timed_out`, and in both cases the agent
//! makes a best-effort decision instead of pretending the user replied. A
//! composition root that has a user attached (the TUI) supplies an
//! implementation; headless runs supply none.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One question — agenthicc's `{id, text, options}`.
///
/// The UI always offers a free-text "Other" answer besides `options`, so a
/// question with no options is a plain free-text question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserQuestion {
    /// Key of this question's answer in the tool result.
    pub id: String,
    /// The question shown to the user.
    pub text: String,
    /// Selectable choices, in order.
    pub options: Vec<String>,
}

impl UserQuestion {
    /// A question with preset choices.
    pub fn new(id: impl Into<String>, text: impl Into<String>, options: Vec<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            options,
        }
    }

    /// A question answered in free text only.
    pub fn free_text(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(id, text, Vec::new())
    }
}

/// How a round of questions ended.
#[derive(Debug, Clone, PartialEq)]
pub enum QuestionOutcome {
    /// One answer per question, in the order asked.
    Answered(Vec<String>),
    /// The user dismissed the questions, or nobody can be asked.
    Cancelled,
    /// Nobody answered before the deadline.
    TimedOut {
        /// The deadline that passed, in seconds.
        timeout_s: f64,
    },
}

impl QuestionOutcome {
    /// The answers, when the user gave them.
    pub fn answers(&self) -> Option<&[String]> {
        match self {
            Self::Answered(answers) => Some(answers),
            _ => None,
        }
    }
}

/// Something that can put questions to the user and collect answers.
#[async_trait]
pub trait UserQuestionGate: Send + Sync {
    /// Ask `questions` and report how the round ended.
    async fn ask(&self, questions: &[UserQuestion]) -> QuestionOutcome;
}

/// A gate that always answers with fixed text — for tests and scripted runs.
#[derive(Debug, Clone)]
pub struct FixedAnswers(pub Vec<String>);

#[async_trait]
impl UserQuestionGate for FixedAnswers {
    async fn ask(&self, questions: &[UserQuestion]) -> QuestionOutcome {
        QuestionOutcome::Answered(
            (0..questions.len())
                .map(|i| self.0.get(i).cloned().unwrap_or_default())
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fixed_answers_pad_missing_entries() {
        let gate = FixedAnswers(vec!["postgres".into()]);
        let outcome = gate
            .ask(&[
                UserQuestion::new("db", "db?", vec!["postgres".into(), "sqlite".into()]),
                UserQuestion::free_text("cache", "cache?"),
            ])
            .await;
        assert_eq!(
            outcome.answers(),
            Some(&["postgres".to_string(), String::new()][..])
        );
    }

    #[test]
    fn only_an_answered_round_has_answers() {
        assert!(QuestionOutcome::Cancelled.answers().is_none());
        assert!(
            QuestionOutcome::TimedOut { timeout_s: 1.0 }
                .answers()
                .is_none()
        );
    }
}
