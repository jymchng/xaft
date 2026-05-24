# PRD: Conversation Memory

> xaft — Autonomous Coding CLI built on agtrs
> Document: `memory/01_conversation_memory.md`
> Version: 0.1.0-draft

---

## 1. Overview

An autonomous coding agent without memory is condemned to re-learn the same
facts on every invocation: project structure, coding conventions, user
preferences, past decisions, and their rationales. xaft's memory system is
built on the `agtrs` `MemoryStore` trait hierarchy and provides four
complementary memory layers:

1. **ConversationStore** — persistent conversation history across sessions
2. **MemoryFact** — extracted, searchable knowledge atoms
3. **ShortTermMemory** — sliding-window context for the current task
4. **`#[remember]` macro** — declarative fact injection/extraction

Together these layers enable xaft to carry context across tasks, sessions, and
projects, making it progressively more useful over time.

---

## 2. Memory Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                     xaft MEMORY SYSTEM                              │
│                                                                     │
│  ┌─────────────────────────────────────────────────────────────┐    │
│  │                    MemoryStore Trait                         │    │
│  │  (unified interface for all memory backends)                │    │
│  └──────────────────────────┬──────────────────────────────────┘    │
│                             │                                       │
│         ┌───────────────────┼───────────────────┐                   │
│         │                   │                   │                   │
│  ┌──────▼──────┐  ┌────────▼────────┐  ┌───────▼───────┐          │
│  │Conversation │  │  ShortTerm      │  │  FactStore    │          │
│  │Store        │  │  Memory         │  │  (MemoryFact) │          │
│  │             │  │                 │  │               │          │
│  │ Full history│  │ Sliding window  │  │ Extracted     │          │
│  │ Cross-      │  │ Current task    │  │ facts         │          │
│  │ session     │  │ Auto-evicts     │  │ Searchable    │          │
│  │ Persistent  │  │ Ephemeral       │  │ Cross-session │          │
│  └──────┬──────┘  └────────┬────────┘  └───────┬───────┘          │
│         │                  │                    │                   │
│         └──────────────────┼────────────────────┘                   │
│                            │                                        │
│                    ┌───────▼────────┐                               │
│                    │  RAG Pipeline  │                               │
│                    │  (retrieval-   │                               │
│                    │   augmented    │                               │
│                    │   generation)  │                               │
│                    └────────────────┘                               │
│                                                                     │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │  #[remember] Macro — declarative fact injection/extraction   │   │
│  └──────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 3. MemoryStore Trait

### 3.1 Core Trait

The `MemoryStore` trait is the unified interface for all memory backends.
It provides CRUD operations, search, and lifecycle management.

```rust
/// Core memory store trait — all memory backends implement this.
#[async_trait]
pub trait MemoryStore: Send + Sync {
    /// Store a value in memory.
    async fn store(&self, entry: MemoryEntry) -> Result<EntryId>;

    /// Retrieve a value by ID.
    async fn retrieve(&self, id: EntryId) -> Result<Option<MemoryEntry>>;

    /// Search memory with a query.
    async fn search(&self, query: &MemoryQuery) -> Result<Vec<MemorySearchResult>>;

    /// Update an existing entry.
    async fn update(&self, id: EntryId, entry: MemoryEntry) -> Result<()>;

    /// Delete an entry.
    async fn delete(&self, id: EntryId) -> Result<()>;

    /// List entries matching a filter.
    async fn list(&self, filter: &MemoryFilter) -> Result<Vec<MemoryEntry>>;

    /// Garbage collect expired or low-relevance entries.
    async fn gc(&self, policy: &GcPolicy) -> Result<GcReport>;

    /// Return the store type identifier.
    fn store_type(&self) -> &str;
}

/// Unique identifier for a memory entry.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct EntryId(Uuid);

/// A single memory entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: EntryId,
    pub key: String,
    pub value: serde_json::Value,
    pub metadata: MemoryMetadata,
    pub embedding: Option<Vec<f32>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub access_count: u64,
    pub last_accessed: Option<DateTime<Utc>>,
    pub relevance_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMetadata {
    pub source: MemorySource,
    pub scope: MemoryScope,
    pub tags: Vec<String>,
    pub confidence: f64,
    pub user_id: Option<String>,
    pub project_id: Option<String>,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MemorySource {
    /// Extracted from conversation by the agent
    AgentExtracted,
    /// Explicitly stored by the user
    UserProvided,
    /// Extracted via #[remember] macro
    MacroExtracted,
    /// Imported from external source
    Imported { source: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MemoryScope {
    /// Visible only within the current task
    Task,
    /// Visible within the current session
    Session,
    /// Visible across sessions for this user/project
    Project,
    /// Visible across all projects for this user
    User,
}
```

### 3.2 Search

```rust
#[derive(Debug, Clone)]
pub struct MemoryQuery {
    /// Text query for semantic search
    pub text: Option<String>,
    /// Vector query for embedding-based search
    pub embedding: Option<Vec<f32>>,
    /// Filter by tags
    pub tags: Vec<String>,
    /// Filter by scope
    pub scope: Option<MemoryScope>,
    /// Filter by source
    pub source: Option<MemorySource>,
    /// Maximum results
    pub limit: usize,
    /// Minimum relevance score
    pub min_relevance: f64,
    /// Sort order
    pub sort: MemorySort,
}

#[derive(Debug, Clone)]
pub enum MemorySort {
    Relevance,
    Recency,
    AccessCount,
    Confidence,
}

#[derive(Debug, Clone)]
pub struct MemorySearchResult {
    pub entry: MemoryEntry,
    pub score: f64,
    pub highlights: Vec<TextHighlight>,
}

#[derive(Debug, Clone)]
pub struct TextHighlight {
    pub field: String,
    pub snippet: String,
    pub start: usize,
    pub end: usize,
}
```

---

## 4. ConversationStore

### 4.1 Purpose

The `ConversationStore` persists the full conversation history between the user
and xaft, enabling:
- Resuming conversations across sessions
- Context retrieval for follow-up questions
- Audit trail of all agent interactions
- Learning from past interactions

### 4.2 Implementation

```rust
/// Persistent store for conversation history.
pub struct ConversationStore {
    /// Underlying MemoryStore backend
    backend: Arc<dyn MemoryStore>,
    /// Embedding service for semantic search
    embedder: Arc<dyn Embedder>,
    /// Conversation summarizer for long contexts
    summarizer: Arc<dyn Summarizer>,
    /// Configuration
    config: ConversationConfig,
}

#[derive(Debug, Clone)]
pub struct ConversationConfig {
    /// Maximum conversation length before summarization
    pub max_messages_before_summary: usize,
    /// Whether to auto-summarize old messages
    pub auto_summarize: bool,
    /// Whether to generate embeddings for messages
    pub generate_embeddings: bool,
    /// Retention policy for old conversations
    pub retention: RetentionPolicy,
}

#[derive(Debug, Clone)]
pub enum RetentionPolicy {
    /// Keep all conversations forever
    KeepAll,
    /// Keep conversations for a specified duration
    Duration { max_age: Duration },
    /// Keep only the N most recent conversations
    Count { max_count: usize },
    /// Keep conversations that have been accessed within a duration
    Lru { max_age_since_access: Duration },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: ConversationId,
    pub user_id: String,
    pub project_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<Message>,
    pub summary: Option<ConversationSummary>,
    pub metadata: ConversationMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub role: MessageRole,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub tool_calls: Vec<ToolCallRecord>,
    pub metadata: MessageMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub summary: String,
    pub key_decisions: Vec<String>,
    pub files_modified: Vec<String>,
    pub topics: Vec<String>,
    pub generated_at: DateTime<Utc>,
    pub message_range: (usize, usize), // indices into messages vec
}
```

### 4.3 Conversation Operations

```rust
impl ConversationStore {
    /// Start a new conversation.
    pub async fn start_conversation(
        &self,
        user_id: &str,
        project_id: Option<&str>,
    ) -> Result<Conversation> {
        let conversation = Conversation {
            id: ConversationId::new(),
            user_id: user_id.to_string(),
            project_id: project_id.map(String::from),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            messages: Vec::new(),
            summary: None,
            metadata: ConversationMetadata::default(),
        };
        self.backend.store(conversation.to_entry()?).await?;
        Ok(conversation)
    }

    /// Add a message to a conversation.
    pub async fn add_message(
        &self,
        conversation_id: ConversationId,
        message: Message,
    ) -> Result<()> {
        let mut conversation = self.load_conversation(conversation_id).await?;
        conversation.messages.push(message);
        conversation.updated_at = Utc::now();

        // Auto-summarize if conversation is getting long
        if self.config.auto_summarize
            && conversation.messages.len() > self.config.max_messages_before_summary
        {
            self.summarize_old_messages(&mut conversation).await?;
        }

        self.backend.store(conversation.to_entry()?).await?;
        Ok(())
    }

    /// Search across all conversations for relevant context.
    pub async fn search_conversations(
        &self,
        query: &str,
        scope: &ConversationSearchScope,
        limit: usize,
    ) -> Result<Vec<ConversationSearchResult>> {
        let embedding = self.embedder.embed(query).await?;
        let memory_query = MemoryQuery {
            text: Some(query.to_string()),
            embedding: Some(embedding),
            tags: vec![],
            scope: None,
            source: None,
            limit,
            min_relevance: 0.5,
            sort: MemorySort::Relevance,
        };
        let results = self.backend.search(&memory_query).await?;
        // Convert memory results back to conversation results
        Ok(results.into_iter().map(|r| r.into()).collect())
    }

    /// Summarize old messages to reduce context length.
    async fn summarize_old_messages(
        &self,
        conversation: &mut Conversation,
    ) -> Result<()> {
        let summary_threshold = self.config.max_messages_before_summary / 2;
        if conversation.messages.len() <= summary_threshold * 2 {
            return Ok(()); // nothing to summarize
        }

        // Summarize the first half of messages
        let old_messages = &conversation.messages[..summary_threshold];
        let summary = self.summarizer.summarize(old_messages).await?;

        conversation.summary = Some(summary);
        // Keep only recent messages + summary
        conversation.messages = conversation.messages[summary_threshold..].to_vec();
        Ok(())
    }
}
```

### 4.4 Conversation Lifecycle

```
┌──────────────┐    ┌───────────────────┐    ┌──────────────┐
│ User starts  │───▶│ ConversationStore │───▶│ Messages     │
│ xaft session │    │ .start_convo()    │    │ exchanged    │
└──────────────┘    └───────────────────┘    └──────┬───────┘
                                                     │
                                              ┌──────┴───────┐
                                              │  Too many    │
                                              │  messages?   │
                                              └──┬───────┬───┘
                                                Yes     No
                                                 │       │
                                                 ▼       │
                                        ┌────────────┐  │
                                        │ Summarize  │  │
                                        │ old msgs   │  │
                                        │ via LLM    │  │
                                        └──────┬─────┘  │
                                               │        │
                                               ▼        ▼
                                        ┌──────────────────┐
                                        │ Session ends:    │
                                        │ • Persist convo  │
                                        │ • Extract facts  │
                                        │ • Generate       │
                                        │   embeddings     │
                                        └────────┬─────────┘
                                                 │
                                                 ▼
                                        ┌──────────────────┐
                                        │ Next session:    │
                                        │ • Resume convo   │
                                        │ • Inject summary │
                                        │ • Search history │
                                        └──────────────────┘
```

---

## 5. MemoryFact

### 5.1 Fact Model

A `MemoryFact` is an atomic unit of knowledge extracted from conversations
or explicitly provided by the user. Facts are the primary currency of
cross-session memory.

```rust
/// An atomic unit of knowledge stored in memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryFact {
    /// Unique identifier
    pub id: FactId,
    /// The factual statement
    pub content: String,
    /// Category of the fact
    pub category: FactCategory,
    /// Confidence in this fact (0.0 - 1.0)
    pub confidence: f64,
    /// Source of this fact
    pub source: FactSource,
    /// When this fact was first observed
    pub first_seen: DateTime<Utc>,
    /// When this fact was last validated
    pub last_validated: DateTime<Utc>,
    /// How many times this fact has been accessed
    pub access_count: u64,
    /// Whether this fact is still believed to be true
    pub valid: bool,
    /// Tags for categorization
    pub tags: Vec<String>,
    /// Scope of visibility
    pub scope: MemoryScope,
    /// Optional embedding for semantic search
    pub embedding: Option<Vec<f32>>,
    /// Related facts
    pub related: Vec<FactId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FactCategory {
    /// User preference (e.g., "prefers tabs over spaces")
    UserPreference,
    /// Project convention (e.g., "uses conventional commits")
    ProjectConvention,
    /// Technical fact (e.g., "database is PostgreSQL 15")
    TechnicalFact,
    /// Decision record (e.g., "chose Redis over Memcached because...")
    Decision,
    /// Error pattern (e.g., "cargo build fails if... do this...")
    ErrorPattern,
    /// Architecture note (e.g., "API routes are in src/api/")
    Architecture,
    /// Dependency info (e.g., "project uses React 18 with TypeScript")
    Dependency,
    /// Custom category
    Custom { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactSource {
    pub origin: FactOrigin,
    pub conversation_id: Option<ConversationId>,
    pub message_id: Option<MessageId>,
    pub extracted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FactOrigin {
    /// Agent extracted this from conversation
    AgentExtraction,
    /// User explicitly told xaft to remember this
    UserExplicit,
    /// Extracted via #[remember] macro
    MacroExtraction,
    /// Inferred from code analysis
    CodeAnalysis,
    /// Imported from external source
    Imported { source: String },
}
```

### 5.2 Fact Extraction Pipeline

```
Conversation message
        │
        ▼
┌──────────────────┐
│ FactExtractor    │
│ (LLM-based)     │
│                  │
│ Prompt: "Extract │
│ facts from this  │
│ conversation"    │
└────────┬─────────┘
         │
         ▼
┌──────────────────────────────┐
│ Raw extracted facts          │
│ (may include noise)          │
└──────────────┬───────────────┘
               │
               ▼
┌──────────────────────────────┐
│ FactDeduplicator             │
│                              │
│ • Semantic dedup against     │
│   existing facts             │
│ • Merge confidence scores    │
│ • Update existing facts if   │
│   contradicted               │
└──────────────┬───────────────┘
               │
               ▼
┌──────────────────────────────┐
│ FactValidator                │
│                              │
│ • Check against codebase     │
│ • Verify technical claims    │
│ • Assign confidence score    │
└──────────────┬───────────────┘
               │
               ▼
┌──────────────────────────────┐
│ FactStore                    │
│                              │
│ • Persist with embeddings    │
│ • Index for search           │
│ • Set expiration if needed   │
└──────────────────────────────┘
```

### 5.3 Fact Extraction Implementation

```rust
pub struct FactExtractor {
    llm: Arc<dyn LlmClient>,
    embedder: Arc<dyn Embedder>,
    deduplicator: Arc<FactDeduplicator>,
    validator: Arc<FactValidator>,
}

impl FactExtractor {
    /// Extract facts from a conversation message.
    pub async fn extract(
        &self,
        message: &Message,
        context: &ExtractionContext,
    ) -> Result<Vec<MemoryFact>> {
        let prompt = format!(
            r#"Analyze the following conversation message and extract any factual knowledge
            that should be remembered for future interactions.

            Categories: user_preference, project_convention, technical_fact,
            decision, error_pattern, architecture, dependency

            Rules:
            - Only extract concrete, verifiable facts
            - Do not extract opinions or temporary states
            - Include confidence level (0.0-1.0)
            - Tag facts with relevant categories

            Message:
            Role: {:?}
            Content: {}

            Extracted facts (JSON array):
            "#,
            message.role, message.content
        );

        let response = self.llm.complete(&prompt).await?;
        let raw_facts: Vec<RawFact> = serde_json::from_str(&response.text)
            .unwrap_or_default();

        let mut facts = Vec::new();
        for raw in raw_facts {
            let embedding = self.embedder.embed(&raw.content).await.ok();

            let fact = MemoryFact {
                id: FactId::new(),
                content: raw.content,
                category: raw.category,
                confidence: raw.confidence,
                source: FactSource {
                    origin: FactOrigin::AgentExtraction,
                    conversation_id: context.conversation_id,
                    message_id: Some(message.id),
                    extracted_at: Utc::now(),
                },
                first_seen: Utc::now(),
                last_validated: Utc::now(),
                access_count: 0,
                valid: true,
                tags: raw.tags,
                scope: raw.scope,
                embedding,
                related: vec![],
            };

            // Deduplicate against existing facts
            if let Some(merged) = self.deduplicator.deduplicate(&fact, context.existing_facts).await? {
                facts.push(merged);
            }
        }

        Ok(facts)
    }
}
```

### 5.4 Fact Deduplication

```rust
pub struct FactDeduplicator {
    embedder: Arc<dyn Embedder>,
    /// Similarity threshold for considering facts duplicates
    similarity_threshold: f64,
}

impl FactDeduplicator {
    /// Check if a new fact duplicates or contradicts existing facts.
    pub async fn deduplicate(
        &self,
        new_fact: &MemoryFact,
        existing: &[MemoryFact],
    ) -> Result<Option<MemoryFact>> {
        let new_embedding = match &new_fact.embedding {
            Some(e) => e,
            None => self.embedder.embed(&new_fact.content).await?.as_slice(),
        };

        for existing_fact in existing {
            if let Some(existing_embedding) = &existing_fact.embedding {
                let similarity = cosine_similarity(new_embedding, existing_embedding);
                if similarity > self.similarity_threshold {
                    // Facts are semantically similar
                    if new_fact.contradicts(existing_fact) {
                        // Contradiction: update the existing fact
                        return Ok(Some(MemoryFact {
                            content: format!(
                                "[UPDATED] {} → {}",
                                existing_fact.content, new_fact.content
                            ),
                            confidence: new_fact.confidence,
                            last_validated: Utc::now(),
                            valid: true,
                            ..existing_fact.clone()
                        }));
                    } else {
                        // Reinforcement: boost confidence
                        return Ok(Some(MemoryFact {
                            confidence: (existing_fact.confidence + new_fact.confidence) / 2.0,
                            access_count: existing_fact.access_count + 1,
                            last_validated: Utc::now(),
                            ..existing_fact.clone()
                        }));
                    }
                }
            }
        }

        // No duplicate found — this is a new fact
        Ok(Some(new_fact.clone()))
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    (dot / (norm_a * norm_b)) as f64
}
```

---

## 6. ShortTermMemory

### 6.1 Purpose

`ShortTermMemory` provides a bounded, sliding-window context for the current
task. Unlike `ConversationStore` (persistent) or `MemoryFact` (extracted
knowledge), `ShortTermMemory` holds transient working context that is
automatically evicted when the task completes or the window overflows.

### 6.2 Implementation

```rust
/// Bounded short-term memory for the current task.
pub struct ShortTermMemory {
    /// Ring buffer of recent entries
    buffer: VecDeque<StmEntry>,
    /// Maximum number of entries
    capacity: usize,
    /// Token budget for all entries
    token_budget: usize,
    /// Current token usage
    token_usage: usize,
}

#[derive(Debug, Clone)]
pub struct StmEntry {
    pub id: EntryId,
    pub content: String,
    pub token_count: usize,
    pub priority: StmPriority,
    pub category: StmCategory,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StmPriority {
    /// Can be evicted first
    Low = 0,
    /// Default priority
    Normal = 1,
    /// Evicted last
    High = 2,
    /// Never evicted (pinned)
    Pinned = 3,
}

#[derive(Debug, Clone)]
pub enum StmCategory {
    /// Current task description
    TaskDescription,
    /// Files currently being edited
    ActiveFiles,
    /// Recent tool results
    ToolResults,
    /// Agent's internal reasoning
    Reasoning,
    /// User's last instruction
    UserInstruction,
    /// Context about the current code location
    CodeContext,
}

impl ShortTermMemory {
    pub fn new(capacity: usize, token_budget: usize) -> Self {
        Self {
            buffer: VecDeque::with_capacity(capacity),
            capacity,
            token_budget,
            token_usage: 0,
        }
    }

    /// Add an entry to short-term memory, evicting if necessary.
    pub fn push(&mut self, entry: StmEntry) -> Result<Option<StmEntry>> {
        let mut evicted = None;

        // Check token budget — evict lowest-priority entries
        while self.token_usage + entry.token_count > self.token_budget
            && !self.buffer.is_empty()
        {
            evicted = self.evict_lowest_priority();
        }

        // Check capacity
        if self.buffer.len() >= self.capacity {
            evicted = self.evict_lowest_priority();
        }

        self.token_usage += entry.token_count;
        self.buffer.push_back(entry);
        Ok(evicted)
    }

    /// Evict the lowest-priority, oldest entry.
    fn evict_lowest_priority(&mut self) -> Option<StmEntry> {
        let min_priority = self.buffer.iter()
            .filter(|e| e.priority != StmPriority::Pinned)
            .map(|e| e.priority)
            .min()?;

        // Find the oldest entry with minimum priority
        let idx = self.buffer.iter()
            .enumerate()
            .filter(|(_, e)| e.priority == min_priority)
            .map(|(i, _)| i)
            .next()?;

        let evicted = self.buffer.remove(idx)?;
        self.token_usage -= evicted.token_count;
        Some(evicted)
    }

    /// Get all entries formatted for LLM context injection.
    pub fn to_context_string(&self) -> String {
        let mut parts = Vec::new();
        for entry in &self.buffer {
            parts.push(format!("[{}] {}", entry.category_str(), entry.content));
        }
        parts.join("\n")
    }
}
```

### 6.3 STM Context Injection

```
LLM Prompt Construction
        │
        ▼
┌─────────────────────────────────────────────────────┐
│ System Prompt                                        │
│ + ShortTermMemory::to_context_string()              │
│ + MemoryFact search results (top-K relevant)        │
│ + Conversation summary (if resuming)                │
│ + User message                                       │
└─────────────────────────────────────────────────────┘
```

---

## 7. `#[remember]` Macro

### 7.1 Purpose

The `#[remember]` macro provides a declarative way to mark tool results or
function outputs as facts that should be automatically extracted and stored
in the fact store.

### 7.2 Macro Syntax

```rust
/// Search for a pattern in the codebase.
/// The result is automatically remembered as an architecture fact.
#[remember(category = "architecture", scope = "project")]
#[tool(name = "search", description = "Search codebase for a pattern")]
async fn search_code(
    ctx: &ToolContext,
    pattern: String,
) -> Result<ToolOutput, ToolError> {
    let results = ctx.workspace.search(&pattern).await?;
    Ok(ToolOutput::text(format!("Found {} matches:\n{}", results.len(), results)))
}

/// Get the project's package manifest.
/// Remembered as a dependency fact.
#[remember(category = "dependency", scope = "project", confidence = 0.95)]
#[tool(name = "read_manifest", description = "Read package.json / Cargo.toml")]
async fn read_manifest(ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
    // ...
}
```

### 7.3 Macro Expansion

```rust
// Pseudo-expansion of #[remember(category = "architecture", scope = "project")]
async fn search_code(
    ctx: &ToolContext,
    pattern: String,
) -> Result<ToolOutput, ToolError> {
    let result = {
        // ── original function body ──
        let results = ctx.workspace.search(&pattern).await?;
        ToolOutput::text(format!("Found {} matches:\n{}", results.len(), results))
    };

    // ── #[remember] expansion ──
    if let Ok(output) = &result {
        let fact = MemoryFact {
            id: FactId::new(),
            content: output.text(),
            category: FactCategory::Architecture,
            confidence: 0.8, // default
            source: FactSource {
                origin: FactOrigin::MacroExtraction,
                conversation_id: None,
                message_id: None,
                extracted_at: Utc::now(),
            },
            first_seen: Utc::now(),
            last_validated: Utc::now(),
            access_count: 0,
            valid: true,
            tags: vec!["architecture".into()],
            scope: MemoryScope::Project,
            embedding: None,
            related: vec![],
        };
        // Fire-and-forget fact storage
        let store = ctx.fact_store.clone();
        tokio::spawn(async move {
            let _ = store.store(fact.into()).await;
        });
    }

    result
}
```

### 7.4 `#[remember]` Attribute Options

```
┌────────────────────────────────────────────────────────────────┐
│            #[remember] ATTRIBUTE OPTIONS                       │
├──────────────┬─────────────────────────────────────────────────┤
│ Option       │ Description                                   │
├──────────────┼─────────────────────────────────────────────────┤
│ category     │ FactCategory for the extracted fact            │
│              │ Values: user_preference, project_convention,   │
│              │ technical_fact, decision, error_pattern,       │
│              │ architecture, dependency, custom(name)         │
├──────────────┼─────────────────────────────────────────────────┤
│ scope        │ MemoryScope for the fact                       │
│              │ Values: task, session, project, user           │
├──────────────┼─────────────────────────────────────────────────┤
│ confidence   │ Initial confidence score (0.0-1.0)             │
│              │ Default: 0.8                                   │
├──────────────┼─────────────────────────────────────────────────┤
│ tags         │ Additional tags for the fact                   │
│              │ Values: comma-separated strings                │
├──────────────┼─────────────────────────────────────────────────┤
│ extract      │ How to extract the fact from the result        │
│              │ Values: "full" (entire output), "summary"      │
│              │ (LLM-summarized), "json_path($.key)"           │
│              │ Default: "full"                                │
├──────────────┼─────────────────────────────────────────────────┤
│ dedup        │ Whether to deduplicate against existing facts  │
│              │ Values: true, false                            │
│              │ Default: true                                  │
├──────────────┼─────────────────────────────────────────────────┤
│ async_store  │ Whether to store asynchronously (fire-and-     │
│              │ forget) or synchronously (blocking)            │
│              │ Default: true                                  │
└──────────────┴─────────────────────────────────────────────────┘
```

---

## 8. User-Specific Memory

### 8.1 Multi-User Isolation

Each user has their own memory namespace. Facts and conversations are
isolated per user, with optional sharing at the project level.

```rust
pub struct UserMemoryNamespace {
    pub user_id: String,
    pub personal_facts: Arc<dyn FactStore>,
    pub conversations: Arc<ConversationStore>,
    pub preferences: Arc<UserPreferences>,
}

impl UserMemoryNamespace {
    /// Search across all memory layers for this user.
    pub async fn search_all(
        &self,
        query: &str,
        project_id: Option<&str>,
    ) -> Result<UnifiedSearchResult> {
        let mut results = UnifiedSearchResult::default();

        // 1. Search personal facts
        let fact_results = self.personal_facts
            .search(query, project_id, 10).await?;
        results.facts = fact_results;

        // 2. Search conversation history
        let convo_results = self.conversations
            .search_conversations(query, &ConversationSearchScope::All, 5).await?;
        results.conversations = convo_results;

        // 3. Search project-level facts (if project_id given)
        if let Some(pid) = project_id {
            let project_facts = self.personal_facts
                .search_by_project(query, pid, 5).await?;
            results.project_facts = project_facts;
        }

        Ok(results)
    }
}
```

---

## 9. RAG Integration

### 9.1 Retrieval-Augmented Generation Pipeline

Memory facts are injected into LLM prompts via a RAG pipeline that retrieves
the most relevant facts for the current context.

```rust
pub struct MemoryRagPipeline {
    fact_store: Arc<dyn FactStore>,
    conversation_store: Arc<ConversationStore>,
    embedder: Arc<dyn Embedder>,
    config: RagConfig,
}

#[derive(Debug, Clone)]
pub struct RagConfig {
    /// Maximum number of facts to retrieve
    pub max_facts: usize,
    /// Minimum relevance score for fact inclusion
    pub min_relevance: f64,
    /// Maximum tokens for injected context
    pub max_context_tokens: usize,
    /// Whether to include conversation summaries
    pub include_conversation_summaries: bool,
    /// Whether to include recent conversation context
    pub include_recent_messages: usize,
}

impl MemoryRagPipeline {
    /// Build a RAG-augmented prompt for the LLM.
    pub async fn augment_prompt(
        &self,
        user_message: &str,
        task_context: &TaskContext,
    ) -> Result<RagAugmentedPrompt> {
        // 1. Embed the user's message
        let query_embedding = self.embedder.embed(user_message).await?;

        // 2. Search for relevant facts
        let fact_query = MemoryQuery {
            text: Some(user_message.to_string()),
            embedding: Some(query_embedding.clone()),
            tags: vec![],
            scope: Some(task_context.scope.clone()),
            source: None,
            limit: self.config.max_facts,
            min_relevance: self.config.min_relevance,
            sort: MemorySort::Relevance,
        };
        let fact_results = self.fact_store.search(&fact_query).await?;

        // 3. Search for relevant conversation history
        let convo_results = if self.config.include_conversation_summaries {
            self.conversation_store
                .search_conversations(user_message, &ConversationSearchScope::All, 3)
                .await?
        } else {
            vec![]
        };

        // 4. Build the augmented prompt
        let mut context_parts = Vec::new();
        let mut token_count = 0;

        // Inject relevant facts
        context_parts.push("## Relevant Memory Facts".to_string());
        for result in &fact_results {
            if token_count + result.token_estimate() > self.config.max_context_tokens {
                break;
            }
            context_parts.push(format!(
                "- [{}] {} (confidence: {:.0%})",
                result.entry.metadata.tags.join(", "),
                result.entry.key,
                result.score,
            ));
            token_count += result.token_estimate();
        }

        // Inject conversation summaries
        if !convo_results.is_empty() {
            context_parts.push("\n## Past Conversation Context".to_string());
            for result in &convo_results {
                if token_count + result.token_estimate() > self.config.max_context_tokens {
                    break;
                }
                if let Some(summary) = &result.summary {
                    context_parts.push(format!("- {}", summary.summary));
                    token_count += result.token_estimate();
                }
            }
        }

        Ok(RagAugmentedPrompt {
            system_context: context_parts.join("\n"),
            facts_used: fact_results.len(),
            conversations_used: convo_results.len(),
            total_tokens: token_count,
        })
    }
}
```

### 9.2 RAG Injection Flow

```
User message arrives
        │
        ▼
┌──────────────────────┐
│ MemoryRagPipeline    │
│ .augment_prompt()    │
└─────────┬────────────┘
          │
          ▼
┌──────────────────────┐    ┌──────────────────┐
│ Embed user message   │───▶│ Search FactStore │
└──────────────────────┘    │ (semantic + tag) │
                            └────────┬─────────┘
                                     │
                            ┌────────▼─────────┐
                            │ Search ConvStore │
                            │ (semantic)       │
                            └────────┬─────────┘
                                     │
                                     ▼
                            ┌──────────────────┐
                            │ Rank & filter    │
                            │ by relevance     │
                            │ + token budget   │
                            └────────┬─────────┘
                                     │
                                     ▼
                            ┌──────────────────┐
                            │ Inject into      │
                            │ system prompt    │
                            │                  │
                            │ ## Relevant      │
                            │ Memory Facts     │
                            │ - fact1          │
                            │ - fact2          │
                            │                  │
                            │ ## Past Context  │
                            │ - summary1       │
                            └────────┬─────────┘
                                     │
                                     ▼
                              LLM receives
                              augmented prompt
```

---

## 10. Configuration

```toml
[memory]
# Storage backend: "sqlite" (default), "postgres", "json-file"
backend = "sqlite"
# Path to memory database
db_path = ".xaft/memory.db"

[memory.conversation]
max_messages_before_summary = 50
auto_summarize = true
generate_embeddings = true
retention = "keep_all"

[memory.facts]
# Minimum confidence to store a fact
min_confidence = 0.6
# Similarity threshold for deduplication
dedup_similarity_threshold = 0.85
# Auto-extract facts from conversations
auto_extract = true
# Maximum facts per project
max_facts_per_project = 1000

[memory.short_term]
# Maximum entries in short-term memory
capacity = 50
# Token budget for short-term memory
token_budget = 8000

[memory.rag]
max_facts = 10
min_relevance = 0.5
max_context_tokens = 4000
include_conversation_summaries = true
include_recent_messages = 5

[memory.embedding]
# Provider: "local" (ONNX), "openai", "cohere"
provider = "openai"
model = "text-embedding-3-small"
dimensions = 1536
```

---

## 11. Open Questions

| # | Question | Status |
|---|----------|--------|
| 1 | Should facts have a TTL (auto-expire after N days)? | Open |
| 2 | How to handle conflicting facts from different sources? | Open |
| 3 | Should users be able to manually edit/delete facts? | Planned |
| 4 | Embedding model: local vs. remote trade-offs? | Open |
| 5 | Fact versioning — track changes to facts over time? | Open |
| 6 | Cross-project fact sharing with user consent? | Open |
| 7 | Memory export/import for workspace portability? | Planned |
