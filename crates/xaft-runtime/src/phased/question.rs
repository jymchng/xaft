//! Free-text questions to the user — the channel behind `ask_user`.
//!
//! agtrs's `ApprovalGate` answers yes/no only. agenthicc's workflows also ask
//! open questions ("which database should this use?") and must behave well
//! when nobody answers: the tool reports `timed_out = true` and the agent
//! records a best-effort assumption. A composition root that has a user
//! attached (the TUI) supplies an implementation; headless runs supply none.

use async_trait::async_trait;

/// Something that can put questions to the user and collect answers.
#[async_trait]
pub trait UserQuestionGate: Send + Sync {
    /// Ask `questions` and return one answer per question, or `None` when the
    /// user did not answer (dismissed, timed out, or not present).
    async fn ask(&self, questions: &[String]) -> Option<Vec<String>>;
}

/// A gate that always answers with fixed text — for tests and scripted runs.
#[derive(Debug, Clone)]
pub struct FixedAnswers(pub Vec<String>);

#[async_trait]
impl UserQuestionGate for FixedAnswers {
    async fn ask(&self, questions: &[String]) -> Option<Vec<String>> {
        Some(
            questions
                .iter()
                .enumerate()
                .map(|(i, _)| self.0.get(i).cloned().unwrap_or_default())
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
        let answers = gate
            .ask(&["db?".to_string(), "cache?".to_string()])
            .await
            .unwrap();
        assert_eq!(answers, vec!["postgres".to_string(), String::new()]);
    }
}
