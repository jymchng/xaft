use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::{MemoryKind, MemoryScope};

// ---------------------------------------------------------------------------
// MemoryFilter
// ---------------------------------------------------------------------------

/// Pre-filter applied before ranking/scoring in list and count operations.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MemoryFilter {
    /// Restrict to one or more scopes. `None` means all scopes.
    pub scopes: Option<Vec<MemoryScope>>,

    /// Restrict to one or more kinds. `None` means all kinds.
    pub kinds: Option<Vec<MemoryKind>>,

    /// Match entries that have ANY of these tags. `None` means no tag filter.
    pub tags: Option<Vec<String>>,

    /// Restrict to entries produced by this agent.
    pub source_agent: Option<String>,

    /// Only return entries created at or after this timestamp.
    pub created_after: Option<DateTime<Utc>>,

    /// Only return entries created before this timestamp.
    pub created_before: Option<DateTime<Utc>>,

    /// Only return entries whose expiry is at or after this timestamp.
    pub expires_after: Option<DateTime<Utc>>,

    /// When `false` (the default) entries whose `expires_at` is in the past
    /// are excluded. Set to `true` to include them.
    pub include_expired: bool,
}

impl MemoryFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Restrict results to the given scope (cumulative — call multiple times to
    /// add more scopes).
    pub fn with_scope(mut self, scope: MemoryScope) -> Self {
        self.scopes.get_or_insert_with(Vec::new).push(scope);
        self
    }

    /// Restrict results to the given kind (cumulative).
    pub fn with_kind(mut self, kind: MemoryKind) -> Self {
        self.kinds.get_or_insert_with(Vec::new).push(kind);
        self
    }

    /// Require that matching entries carry at least this tag (cumulative — any
    /// of the added tags will match).
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.get_or_insert_with(Vec::new).push(tag.into());
        self
    }

    /// Restrict results to entries emitted by the given agent.
    pub fn with_source_agent(mut self, agent: impl Into<String>) -> Self {
        self.source_agent = Some(agent.into());
        self
    }

    /// Only return entries created at or after `ts`.
    pub fn with_created_after(mut self, ts: DateTime<Utc>) -> Self {
        self.created_after = Some(ts);
        self
    }

    /// Only return entries created before `ts`.
    pub fn with_created_before(mut self, ts: DateTime<Utc>) -> Self {
        self.created_before = Some(ts);
        self
    }

    /// Only return entries whose expiry is at or after `ts`.
    pub fn with_expires_after(mut self, ts: DateTime<Utc>) -> Self {
        self.expires_after = Some(ts);
        self
    }

    /// Include entries that have already expired.
    pub fn include_expired(mut self) -> Self {
        self.include_expired = true;
        self
    }
}

// ---------------------------------------------------------------------------
// RankingStrategy
// ---------------------------------------------------------------------------

/// Determines how a set of candidate entries is ordered before the limit is
/// applied.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "strategy", rename_all = "snake_case")]
pub enum RankingStrategy {
    /// Order by `relevance_score` descending.
    Relevance,

    /// Order by `last_accessed_at` descending (most-recently-accessed first).
    Recency,

    /// Order by `access_count` descending (most-accessed first).
    Frequency,

    /// Weighted linear combination of relevance, recency, and frequency scores.
    /// Weights need not sum to 1.0; they are used as relative multipliers.
    Composite {
        relevance_weight: f64,
        recency_weight: f64,
        frequency_weight: f64,
    },
}

impl Default for RankingStrategy {
    fn default() -> Self {
        Self::Relevance
    }
}

// ---------------------------------------------------------------------------
// SearchMode
// ---------------------------------------------------------------------------

/// Controls how the query text is matched against stored entries.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum SearchMode {
    /// Pure embedding-based cosine similarity. Requires that entries and the
    /// query both have embeddings; entries without embeddings are skipped.
    Semantic,

    /// Substring / keyword matching against entry content and metadata.
    /// No embedding is required.
    Keyword,

    /// Weighted combination of semantic and keyword scores.
    /// `semantic_weight` is applied to the semantic score; the keyword score
    /// receives weight `1.0 - semantic_weight`.
    Hybrid {
        /// Weight given to the semantic score in `[0.0, 1.0]`.
        semantic_weight: f64,
    },
}

impl Default for SearchMode {
    fn default() -> Self {
        Self::Hybrid {
            semantic_weight: 0.7,
        }
    }
}

// ---------------------------------------------------------------------------
// MemoryQuery
// ---------------------------------------------------------------------------

/// Full specification for a memory search/retrieval request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryQuery {
    /// Natural-language query text used for both semantic and keyword search.
    pub text: String,

    /// Scope/kind/tag pre-filter applied before scoring.
    pub filter: MemoryFilter,

    /// How to match `text` against stored entries.
    pub mode: SearchMode,

    /// How to rank entries that pass the filter.
    pub ranking: RankingStrategy,

    /// Maximum number of entries to return.
    pub limit: usize,

    /// Minimum similarity score an entry must achieve to be included. `None`
    /// means no threshold — all entries passing the filter are eligible.
    pub min_score: Option<f64>,

    /// When `true`, the raw embedding vectors are included in returned results.
    /// Defaults to `false` to keep payloads small.
    pub include_embeddings: bool,
}

impl MemoryQuery {
    /// Create a query with sensible defaults.
    ///
    /// * mode: `Hybrid { semantic_weight: 0.7 }`
    /// * ranking: `Relevance`
    /// * limit: `10`
    /// * min_score: `None`
    /// * include_embeddings: `false`
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            filter: MemoryFilter::default(),
            mode: SearchMode::default(),
            ranking: RankingStrategy::default(),
            limit: 10,
            min_score: None,
            include_embeddings: false,
        }
    }

    /// Replace the pre-filter.
    pub fn with_filter(mut self, filter: MemoryFilter) -> Self {
        self.filter = filter;
        self
    }

    /// Replace the search mode.
    pub fn with_mode(mut self, mode: SearchMode) -> Self {
        self.mode = mode;
        self
    }

    /// Replace the ranking strategy.
    pub fn with_ranking(mut self, ranking: RankingStrategy) -> Self {
        self.ranking = ranking;
        self
    }

    /// Set the maximum number of results to return.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Set a minimum similarity score threshold.
    pub fn with_min_score(mut self, min_score: f64) -> Self {
        self.min_score = Some(min_score);
        self
    }

    /// Include raw embedding vectors in results.
    pub fn with_embeddings(mut self) -> Self {
        self.include_embeddings = true;
        self
    }
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

/// Cursor-free offset/limit pagination for list operations.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pagination {
    /// Number of entries to skip.
    pub offset: usize,

    /// Maximum number of entries to return per page.
    pub limit: usize,
}

impl Default for Pagination {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: 50,
        }
    }
}

impl Pagination {
    pub fn new(offset: usize, limit: usize) -> Self {
        Self { offset, limit }
    }

    pub fn with_offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Advance to the next page, returning `None` if `count` (total available)
    /// has been exhausted.
    pub fn next_page(&self, count: usize) -> Option<Self> {
        let next_offset = self.offset + self.limit;
        if next_offset < count {
            Some(Self {
                offset: next_offset,
                limit: self.limit,
            })
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// ListOptions
// ---------------------------------------------------------------------------

/// Options for paginated listing of memory entries.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ListOptions {
    /// Pre-filter to apply before ranking.
    pub filter: MemoryFilter,

    /// How to order the filtered entries.
    pub ranking: RankingStrategy,

    /// Pagination cursor.
    pub pagination: Pagination,
}

impl ListOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the filter.
    pub fn with_filter(mut self, filter: MemoryFilter) -> Self {
        self.filter = filter;
        self
    }

    /// Replace the ranking strategy.
    pub fn with_ranking(mut self, ranking: RankingStrategy) -> Self {
        self.ranking = ranking;
        self
    }

    /// Replace the pagination settings.
    pub fn with_pagination(mut self, pagination: Pagination) -> Self {
        self.pagination = pagination;
        self
    }

    /// Convenience: set page size without changing the offset.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.pagination.limit = limit;
        self
    }

    /// Convenience: set the page offset without changing the limit.
    pub fn with_offset(mut self, offset: usize) -> Self {
        self.pagination.offset = offset;
        self
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_filter_builder_accumulates() {
        let f = MemoryFilter::new()
            .with_scope(MemoryScope::Session)
            .with_scope(MemoryScope::Global)
            .with_kind(MemoryKind::Fact)
            .with_tag("rust")
            .with_tag("async")
            .with_source_agent("planner");

        assert_eq!(f.scopes.as_ref().unwrap().len(), 2);
        assert_eq!(f.kinds.as_ref().unwrap().len(), 1);
        assert_eq!(f.tags.as_ref().unwrap(), &["rust", "async"]);
        assert_eq!(f.source_agent.as_deref(), Some("planner"));
        assert!(!f.include_expired);
    }

    #[test]
    fn memory_filter_include_expired() {
        let f = MemoryFilter::new().include_expired();
        assert!(f.include_expired);
    }

    #[test]
    fn memory_query_defaults() {
        let q = MemoryQuery::new("what is the project structure?");
        assert_eq!(q.limit, 10);
        assert!(q.min_score.is_none());
        assert!(!q.include_embeddings);
        assert!(matches!(q.ranking, RankingStrategy::Relevance));
        assert!(matches!(q.mode, SearchMode::Hybrid { semantic_weight } if (semantic_weight - 0.7).abs() < f64::EPSILON));
    }

    #[test]
    fn memory_query_builder() {
        let q = MemoryQuery::new("test")
            .with_limit(25)
            .with_min_score(0.5)
            .with_mode(SearchMode::Semantic)
            .with_ranking(RankingStrategy::Recency)
            .with_embeddings();

        assert_eq!(q.limit, 25);
        assert_eq!(q.min_score, Some(0.5));
        assert!(q.include_embeddings);
        assert!(matches!(q.mode, SearchMode::Semantic));
        assert!(matches!(q.ranking, RankingStrategy::Recency));
    }

    #[test]
    fn pagination_defaults() {
        let p = Pagination::default();
        assert_eq!(p.offset, 0);
        assert_eq!(p.limit, 50);
    }

    #[test]
    fn pagination_next_page() {
        let p = Pagination::new(0, 10);
        let next = p.next_page(25).unwrap();
        assert_eq!(next.offset, 10);
        let after = next.next_page(25).unwrap();
        assert_eq!(after.offset, 20);
        assert!(after.next_page(25).is_none());
    }

    #[test]
    fn list_options_builder() {
        let opts = ListOptions::new()
            .with_limit(20)
            .with_offset(40)
            .with_ranking(RankingStrategy::Frequency);

        assert_eq!(opts.pagination.limit, 20);
        assert_eq!(opts.pagination.offset, 40);
        assert!(matches!(opts.ranking, RankingStrategy::Frequency));
    }

    #[test]
    fn ranking_strategy_composite_serde_round_trip() {
        let r = RankingStrategy::Composite {
            relevance_weight: 0.5,
            recency_weight: 0.3,
            frequency_weight: 0.2,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: RankingStrategy = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            RankingStrategy::Composite {
                relevance_weight,
                recency_weight,
                frequency_weight,
            } if (relevance_weight - 0.5).abs() < f64::EPSILON
              && (recency_weight - 0.3).abs() < f64::EPSILON
              && (frequency_weight - 0.2).abs() < f64::EPSILON
        ));
    }
}
