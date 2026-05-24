# Context Window Management

> How xauft fits large repositories into limited context windows:
> conversation summarization, memory windows, RAG injection, file content
> truncation, scratchpads, token counting, and progressive context loading.

---

## 1. The Problem

Large codebases contain far more text than any LLM's context window can hold.
A typical repository might have 500K+ lines of code across thousands of files,
while the largest context windows are 128K–2M tokens. xauft must be selective
about what enters the context.

```
  Repository: 500K lines ≈ 1.5M tokens
                     │
                     │  must fit into
                     ▼
  Context Window: 128K tokens (gpt-4o)
  ┌─────────────────────────────────────────┐
  │ System Prompt          ~2K tokens       │
  │ Task Description       ~1K tokens       │
  │ Conversation History   ~20K tokens      │
  │ RAG Context            ~50K tokens      │
  │ File Contents          ~40K tokens      │
  │ Scratchpad             ~5K tokens       │
  │ Reserved for Output    ~10K tokens      │
  └─────────────────────────────────────────┘
```

---

## 2. Context Budget Allocator

### 2.1 Architecture

The `ContextBudgetAllocator` divides the context window into budgeted slots:

```rust
pub struct ContextBudgetAllocator {
    /// Total context window size.
    total_tokens: usize,
    /// Output reservation.
    output_reserve: usize,
    /// Budget allocations.
    allocations: ContextAllocations,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextAllocations {
    pub system_prompt: TokenBudget,
    pub task_description: TokenBudget,
    pub conversation: TokenBudget,
    pub rag_context: TokenBudget,
    pub file_contents: TokenBudget,
    pub scratchpad: TokenBudget,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudget {
    /// Maximum tokens allocated.
    pub max_tokens: usize,
    /// Current tokens used.
    pub used_tokens: usize,
    /// Priority (higher = less likely to be trimmed).
    pub priority: u8,
    /// Truncation strategy when budget exceeded.
    pub truncation: TruncationStrategy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TruncationStrategy {
    /// Truncate from the beginning (keep recent).
    TruncateHead,
    /// Truncate from the end (keep context).
    TruncateTail,
    /// Summarize the content to fit.
    Summarize { target_ratio: f64 },
    /// Remove oldest entries (for conversation history).
    SlidingWindow { window_size: usize },
}
```

### 2.2 Default Allocations

| Slot              | Allocation | Priority | Truncation       |
|-------------------|:----------:|:--------:|:-----------------:|
| System Prompt     | 2K         | 10       | None (fixed)      |
| Task Description  | 1K         | 9        | TruncateTail      |
| Conversation      | 20K        | 5        | SlidingWindow     |
| RAG Context       | 50K        | 7        | TruncateHead      |
| File Contents     | 40K        | 8        | Summarize         |
| Scratchpad        | 5K         | 6        | TruncateHead      |
| Output Reserve    | 10K        | 10       | None (reserved)   |

### 2.3 Allocation Algorithm

```rust
impl ContextBudgetAllocator {
    pub fn new(total_tokens: usize, output_reserve: usize) -> Self {
        let available = total_tokens.saturating_sub(output_reserve);
        Self {
            total_tokens,
            output_reserve,
            allocations: ContextAllocations {
                system_prompt: TokenBudget {
                    max_tokens: 2048,
                    used_tokens: 0,
                    priority: 10,
                    truncation: TruncationStrategy::Summarize { target_ratio: 0.5 },
                },
                task_description: TokenBudget {
                    max_tokens: 1024,
                    used_tokens: 0,
                    priority: 9,
                    truncation: TruncationStrategy::TruncateTail,
                },
                conversation: TokenBudget {
                    max_tokens: (available as f64 * 0.16) as usize,  // ~20K of 128K
                    used_tokens: 0,
                    priority: 5,
                    truncation: TruncationStrategy::SlidingWindow { window_size: 20 },
                },
                rag_context: TokenBudget {
                    max_tokens: (available as f64 * 0.40) as usize,  // ~50K of 128K
                    used_tokens: 0,
                    priority: 7,
                    truncation: TruncationStrategy::TruncateHead,
                },
                file_contents: TokenBudget {
                    max_tokens: (available as f64 * 0.32) as usize,  // ~40K of 128K
                    used_tokens: 0,
                    priority: 8,
                    truncation: TruncationStrategy::Summarize { target_ratio: 0.3 },
                },
                scratchpad: TokenBudget {
                    max_tokens: (available as f64 * 0.04) as usize,  // ~5K of 128K
                    used_tokens: 0,
                    priority: 6,
                    truncation: TruncationStrategy::TruncateHead,
                },
            },
        }
    }

    /// Allocate the context window, trimming as needed.
    pub fn allocate(&mut self, context: &mut AgentContextSnapshot) -> AllocationResult {
        let mut total_used = 0;
        let mut trimmed = Vec::new();

        // Process by priority (highest first)
        let mut slots: Vec<(&str, &mut TokenBudget, &mut String)> = vec![
            ("system_prompt", &mut self.allocations.system_prompt, &mut context.system_prompt),
            ("task_description", &mut self.allocations.task_description, &mut context.task_description),
            ("file_contents", &mut self.allocations.file_contents, &mut context.file_contents),
            ("rag_context", &mut self.allocations.rag_context, &mut context.rag_context),
            ("scratchpad", &mut self.allocations.scratchpad, &mut context.scratchpad),
            ("conversation", &mut self.allocations.conversation, &mut context.conversation),
        ];

        // Sort by priority (descending)
        slots.sort_by(|a, b| b.1.priority.cmp(&a.1.priority));

        for (name, budget, content) in &mut slots {
            let token_count = TokenEstimator::estimate(content);
            budget.used_tokens = token_count;

            if token_count > budget.max_tokens {
                let trimmed_content = self.apply_truncation(content, budget);
                let new_count = TokenEstimator::estimate(&trimmed_content);
                let saved = token_count - new_count;
                *content = trimmed_content;
                budget.used_tokens = new_count;
                trimmed.push(TrimRecord {
                    slot: name.to_string(),
                    original_tokens: token_count,
                    trimmed_tokens: new_count,
                    tokens_saved: saved,
                    strategy: budget.truncation.clone(),
                });
            }

            total_used += budget.used_tokens;
        }

        AllocationResult {
            total_used,
            total_available: self.total_tokens - self.output_reserve,
            output_reserve: self.output_reserve,
            trimmed,
        }
    }
}
```

---

## 3. Conversation Summarization

### 3.1 Mechanism

When conversation history exceeds the `summarize_at` threshold, xauft
summarizes the older messages to compress them:

```
  Before Summarization:
  ┌──────────────────────────────────────────────┐
  │ Msg 1 (user): "Fix the auth bug"              │  ~50 tokens
  │ Msg 2 (assistant): "I'll analyze the code..." │  ~200 tokens
  │ Msg 3 (tool): read_file("auth.rs")            │  ~500 tokens
  │ Msg 4 (assistant): "Found the issue..."       │  ~150 tokens
  │ Msg 5 (tool): edit_file(...)                  │  ~300 tokens
  │ Msg 6 (assistant): "Fixed! Now testing..."    │  ~100 tokens
  │ Msg 7 (tool): shell("cargo test")             │  ~800 tokens
  │ Msg 8 (assistant): "Tests pass"               │  ~50 tokens
  │ Msg 9 (user): "Also fix the logout bug"       │  ~50 tokens
  │ Msg 10 (assistant): "Looking at logout..."    │  ~200 tokens
  └──────────────────────────────────────────────┘
  Total: ~2400 tokens

  After Summarization (threshold = 1000 tokens):
  ┌──────────────────────────────────────────────┐
  │ [Summary]: User asked to fix auth bug.        │  ~80 tokens
  │ Agent found issue in auth.rs, applied fix,    │
  │ tests passed.                                 │
  │ Msg 9 (user): "Also fix the logout bug"       │  ~50 tokens
  │ Msg 10 (assistant): "Looking at logout..."    │  ~200 tokens
  └──────────────────────────────────────────────┘
  Total: ~330 tokens
```

### 3.2 Implementation

```rust
pub struct ConversationSummarizer<P: LlmProvider> {
    provider: P,
    model: String,
    /// Token threshold to trigger summarization.
    summarize_at: usize,
    /// Target ratio for summarized content.
    compression_ratio: f64,
    /// Maximum summary tokens.
    max_summary_tokens: usize,
}

impl<P: LlmProvider> ConversationSummarizer<P> {
    /// Check if summarization is needed and perform it.
    pub async fn maybe_summarize(
        &self,
        messages: &mut Vec<Message>,
        max_budget: usize,
    ) -> Result<SummarizationResult, SummarizationError> {
        let total_tokens = self.estimate_message_tokens(messages);

        if total_tokens <= self.summarize_at {
            return Ok(SummarizationResult::NoChange { total_tokens });
        }

        // Find the split point: keep the most recent messages intact
        let mut split_point = messages.len();
        let mut recent_tokens = 0;
        for (i, msg) in messages.iter().enumerate().rev() {
            let msg_tokens = TokenEstimator::estimate(&msg.content());
            recent_tokens += msg_tokens;
            if recent_tokens > max_budget / 2 {
                split_point = i + 1;
                break;
            }
        }

        // Split messages into old (to summarize) and recent (to keep)
        let old_messages: Vec<Message> = messages.drain(..split_point).collect();
        let recent_messages: Vec<Message> = messages.drain(..).collect();

        // Summarize old messages
        let summary = self.summarize_messages(&old_messages).await?;

        // Reconstruct: summary + recent messages
        messages.clear();
        messages.push(Message::system(format!(
            "[Conversation Summary]\n{}", summary
        )));
        messages.extend(recent_messages);

        let new_total = self.estimate_message_tokens(messages);

        Ok(SummarizationResult::Summarized {
            original_tokens: total_tokens,
            new_tokens: new_total,
            messages_summarized: old_messages.len(),
            compression_ratio: new_total as f64 / total_tokens as f64,
        })
    }

    async fn summarize_messages(
        &self,
        messages: &[Message],
    ) -> Result<String, SummarizationError> {
        let conversation_text = messages.iter()
            .map(|m| format!("[{}]: {}", m.role(), m.content()))
            .collect::<Vec<_>>()
            .join("\n");

        let summary_request = LlmRequest {
            model: self.model.clone(),
            messages: vec![Message::user(format!(
                "Summarize the following conversation between a coding assistant and a user. \
                 Preserve key decisions, code changes, and unresolved issues. \
                 Be concise but comprehensive.\n\n{}", conversation_text
            ))],
            system_prompt: Some("You are a conversation summarizer. Produce concise summaries \
                                 that preserve all important context for continuing the task.".into()),
            temperature: Some(0.0),
            max_tokens: Some(self.max_summary_tokens),
            ..Default::default()
        };

        let response = self.provider.complete(summary_request).await?;
        Ok(response.content)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SummarizationResult {
    NoChange { total_tokens: usize },
    Summarized {
        original_tokens: usize,
        new_tokens: usize,
        messages_summarized: usize,
        compression_ratio: f64,
    },
}
```

---

## 4. Memory Window

### 4.1 Concept

The `memory_window_tokens` setting defines a hard limit on conversation
history. Unlike summarization (which compresses), the memory window simply
drops the oldest messages:

```rust
pub struct MemoryWindow {
    /// Maximum tokens for conversation history.
    max_tokens: usize,
    /// Minimum messages to always keep.
    min_messages: usize,
    /// Whether to keep system messages regardless of token count.
    keep_system_messages: bool,
}

impl MemoryWindow {
    /// Apply the memory window, removing oldest messages that exceed the limit.
    pub fn apply(&self, messages: &mut Vec<Message>) -> WindowResult {
        let total_tokens = TokenEstimator::estimate_messages(messages);
        if total_tokens <= self.max_tokens {
            return WindowResult::NoChange { total_tokens };
        }

        let mut removed = 0;
        let mut removed_tokens = 0;

        // Keep removing from the front until within budget
        while TokenEstimator::estimate_messages(messages) > self.max_tokens
            && messages.len() > self.min_messages
        {
            let idx = self.find_removable_message(messages);
            let msg = messages.remove(idx);
            removed_tokens += TokenEstimator::estimate(&msg.content());
            removed += 1;
        }

        WindowResult::Trimmed {
            original_tokens: total_tokens,
            new_tokens: TokenEstimator::estimate_messages(messages),
            messages_removed: removed,
            tokens_removed: removed_tokens,
        }
    }

    fn find_removable_message(&self, messages: &[Message]) -> usize {
        for (i, msg) in messages.iter().enumerate() {
            if self.keep_system_messages && msg.role() == Role::System {
                continue;
            }
            return i;
        }
        0 // fallback
    }
}
```

---

## 5. RAG Injection

### 5.1 Retrieval-Augmented Generation for Code

xauft uses RAG to inject relevant code snippets into the context without
loading entire files:

```
  User: "Fix the authentication middleware"
       │
       ▼
  ┌───────────┐     ┌──────────────┐     ┌──────────────┐
  │ Query     │────▶│ Embedding    │────▶│ Vector Store │
  │ Encoder   │     │ Search       │     │ (Code Chunks)│
  └───────────┘     └──────┬───────┘     └──────────────┘
                           │
                           │  Top-K relevant chunks
                           ▼
                    ┌──────────────┐
                    │ RAG Context  │
                    │ Injection    │
                    │              │
                    │ "Based on    │
                    │  relevant    │
                    │  code: ..."  │
                    └──────┬───────┘
                           │
                           ▼
                    ┌──────────────┐
                    │  LLM Request │
                    │  with RAG    │
                    └──────────────┘
```

### 5.2 Implementation

```rust
pub struct RagInjector<E: EmbeddingProvider, S: VectorStore> {
    /// Embedding provider for query encoding.
    embedder: E,
    /// Vector store for code chunk retrieval.
    store: S,
    /// Configuration.
    config: RagConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagConfig {
    /// Number of chunks to retrieve (K).
    pub top_k: usize,
    /// Maximum total tokens for RAG context.
    pub max_context_tokens: usize,
    /// Minimum relevance score for inclusion.
    pub min_relevance: f64,
    /// Chunk size in tokens.
    pub chunk_size: usize,
    /// Overlap between chunks in tokens.
    pub chunk_overlap: usize,
    /// Whether to include surrounding context (±2 lines).
    pub include_surrounding: bool,
    /// File types to index.
    pub indexed_extensions: Vec<String>,
    /// Directories to exclude.
    pub exclude_dirs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CodeChunk {
    /// File path.
    pub file_path: PathBuf,
    /// Line range (start, end).
    pub line_range: (u32, u32),
    /// Chunk content.
    pub content: String,
    /// Embedding vector.
    pub embedding: Vec<f32>,
    /// Relevance score from search.
    pub score: f64,
    /// Chunk type (function, class, import, etc.).
    pub chunk_type: ChunkType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChunkType {
    Function,
    Method,
    Class,
    Struct,
    Interface,
    Module,
    Import,
    Config,
    Test,
    Unknown,
}

impl<E: EmbeddingProvider, S: VectorStore> RagInjector<E, S> {
    /// Retrieve and format RAG context for a query.
    pub async fn retrieve(
        &self,
        query: &str,
        budget: usize,
    ) -> Result<RagContext, RagError> {
        // 1. Embed the query
        let query_embedding = self.embedder.embed(query).await?;

        // 2. Search for relevant chunks
        let candidates = self.store.search(
            &query_embedding,
            self.config.top_k * 2,  // Retrieve extra, then filter
        ).await?;

        // 3. Filter by relevance
        let relevant: Vec<CodeChunk> = candidates.into_iter()
            .filter(|c| c.score >= self.config.min_relevance)
            .collect();

        // 4. Fit within token budget
        let mut selected = Vec::new();
        let mut tokens_used = 0;

        for chunk in relevant {
            let chunk_tokens = TokenEstimator::estimate(&chunk.content);
            if tokens_used + chunk_tokens > budget.min(self.config.max_context_tokens) {
                // Try to truncate the chunk
                if tokens_used < budget {
                    let remaining = budget - tokens_used;
                    let truncated = self.truncate_chunk(&chunk, remaining);
                    selected.push(truncated);
                }
                break;
            }
            tokens_used += chunk_tokens;
            selected.push(chunk);
        }

        // 5. Format as context
        let context_text = self.format_context(&selected);

        Ok(RagContext {
            chunks: selected,
            total_tokens: tokens_used,
            context_text,
        })
    }

    fn format_context(&self, chunks: &[CodeChunk]) -> String {
        let mut parts = Vec::new();
        for chunk in chunks {
            parts.push(format!(
                "```{}:{}-{}\n{}\n```",
                chunk.file_path.display(),
                chunk.line_range.0,
                chunk.line_range.1,
                chunk.content
            ));
        }
        format!(
            "Relevant code context:\n\n{}",
            parts.join("\n\n")
        )
    }

    fn truncate_chunk(&self, chunk: &CodeChunk, max_tokens: usize) -> CodeChunk {
        let lines: Vec<&str> = chunk.content.lines().collect();
        let mut selected_lines = Vec::new();
        let mut tokens = 0;

        for line in &lines {
            let line_tokens = TokenEstimator::estimate(line);
            if tokens + line_tokens > max_tokens {
                break;
            }
            tokens += line_tokens;
            selected_lines.push(*line);
        }

        CodeChunk {
            content: selected_lines.join("\n"),
            line_range: (
                chunk.line_range.0,
                chunk.line_range.0 + selected_lines.len() as u32 - 1,
            ),
            ..chunk.clone()
        }
    }
}
```

### 5.3 Codebase Indexing

```rust
pub struct CodebaseIndexer<E: EmbeddingProvider, S: VectorStore> {
    embedder: E,
    store: S,
    config: RagConfig,
}

impl<E: EmbeddingProvider, S: VectorStore> CodebaseIndexer<E, S> {
    /// Index the entire codebase.
    pub async fn index_directory(&self, root: &Path) -> Result<IndexStats, IndexError> {
        let mut stats = IndexStats::default();
        let mut chunks = Vec::new();

        // Walk the directory tree
        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();

            // Skip excluded directories
            if self.should_skip(path) {
                continue;
            }

            // Skip non-code files
            if !self.is_indexable(path) {
                continue;
            }

            // Read and chunk the file
            let content = tokio::fs::read_to_string(path).await?;
            let file_chunks = self.chunk_file(path, &content);
            stats.files_indexed += 1;
            stats.chunks_created += file_chunks.len();

            chunks.extend(file_chunks);
        }

        // Generate embeddings in batches
        let batch_size = 100;
        for batch in chunks.chunks(batch_size) {
            let texts: Vec<&str> = batch.iter().map(|c| c.content.as_str()).collect();
            let embeddings = self.embedder.embed_batch(&texts).await?;

            for (chunk, embedding) in batch.iter().zip(embeddings) {
                self.store.insert(chunk, embedding).await?;
            }
            stats.chunks_embedded += batch.len();
        }

        Ok(stats)
    }

    fn chunk_file(&self, path: &Path, content: &str) -> Vec<CodeChunk> {
        let lines: Vec<&str> = content.lines().collect();
        let mut chunks = Vec::new();
        let mut current_start = 0;
        let mut current_tokens = 0;
        let mut current_lines = Vec::new();

        for (i, line) in lines.iter().enumerate() {
            let line_tokens = TokenEstimator::estimate(line);

            if current_tokens + line_tokens > self.config.chunk_size && !current_lines.is_empty() {
                // Flush current chunk
                chunks.push(CodeChunk {
                    file_path: path.to_path_buf(),
                    line_range: (current_start as u32 + 1, i as u32),
                    content: current_lines.join("\n"),
                    embedding: vec![], // filled later
                    score: 0.0,
                    chunk_type: ChunkType::Unknown,
                });

                // Start new chunk with overlap
                let overlap_start = i.saturating_sub(self.config.chunk_overlap / 20);
                current_start = overlap_start;
                current_lines = lines[overlap_start..=i].iter().map(|l| l.to_string()).collect();
                current_tokens = TokenEstimator::estimate(&current_lines.join("\n"));
            } else {
                current_lines.push(line.to_string());
                current_tokens += line_tokens;
            }
        }

        // Flush remaining
        if !current_lines.is_empty() {
            chunks.push(CodeChunk {
                file_path: path.to_path_buf(),
                line_range: (current_start as u32 + 1, lines.len() as u32),
                content: current_lines.join("\n"),
                embedding: vec![],
                score: 0.0,
                chunk_type: ChunkType::Unknown,
            });
        }

        chunks
    }
}
```

---

## 6. File Content Truncation

### 6.1 Smart Truncation with Line Ranges

When xauft needs to include file content, it uses **line-range-aware
truncation** to include the most relevant portions:

```rust
pub struct FileContentTruncator {
    /// Maximum tokens per file inclusion.
    max_tokens_per_file: usize,
    /// Context lines around target lines.
    surrounding_lines: usize,
}

impl FileContentTruncator {
    /// Truncate a file to include the most relevant sections.
    pub fn truncate(
        &self,
        file: &FileContent,
        target_lines: &[u32],
        budget: usize,
    ) -> TruncatedFile {
        if TokenEstimator::estimate(&file.content) <= budget.min(self.max_tokens_per_file) {
            return TruncatedFile {
                path: file.path.clone(),
                content: file.content.clone(),
                included_ranges: vec![(1, file.line_count as u32)],
                truncated: false,
            };
        }

        // Build ranges around target lines
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for &line in target_lines {
            let start = line.saturating_sub(self.surrounding_lines as u32);
            let end = (line + self.surrounding_lines as u32).min(file.line_count as u32);
            ranges.push((start, end));
        }

        // Merge overlapping ranges
        ranges.sort_by_key(|r| r.0);
        let merged = self.merge_ranges(ranges);

        // Extract content for each range
        let lines: Vec<&str> = file.content.lines().collect();
        let mut parts = Vec::new();
        let mut total_tokens = 0;

        for (start, end) in &merged {
            let range_content = lines[(start.saturating_sub(1)) as usize..end as usize]
                .join("\n");
            let range_tokens = TokenEstimator::estimate(&range_content);

            if total_tokens + range_tokens > budget {
                break;
            }

            parts.push(format!(
                "// Lines {}-{}\n{}",
                start, end, range_content
            ));
            total_tokens += range_tokens;
        }

        let full_content = parts.join("\n\n// ... truncated ...\n\n");

        TruncatedFile {
            path: file.path.clone(),
            content: full_content,
            included_ranges: merged,
            truncated: true,
        }
    }

    fn merge_ranges(&self, ranges: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
        let mut merged = Vec::new();
        for range in ranges {
            if let Some(last) = merged.last_mut() {
                if range.0 <= last.1 + 1 {
                    last.1 = last.1.max(range.1);
                    continue;
                }
            }
            merged.push(range);
        }
        merged
    }
}
```

---

## 7. Scratchpad for Cross-Turn Notes

### 7.1 Concept

The scratchpad is a persistent in-session text buffer that the agent can
read and write. It survives across conversation turns and is included in
every LLM request. This gives the agent a "memory" beyond the sliding window.

```
  Turn 1: Agent discovers important info → writes to scratchpad
  Turn 2: Conversation window moves → old messages dropped
  Turn 3: Agent reads scratchpad → still has the important info
```

### 7.2 Implementation

```rust
pub struct Scratchpad {
    /// Scratchpad content.
    content: String,
    /// Maximum tokens for the scratchpad.
    max_tokens: usize,
    /// Current estimated token count.
    current_tokens: usize,
}

impl Scratchpad {
    pub fn new(max_tokens: usize) -> Self {
        Self {
            content: String::new(),
            max_tokens,
            current_tokens: 0,
        }
    }

    /// Append a note to the scratchpad.
    pub fn append(&mut self, note: &str) -> Result<(), ScratchpadError> {
        let note_tokens = TokenEstimator::estimate(note);
        if self.current_tokens + note_tokens > self.max_tokens {
            // Need to make room — trim oldest notes
            self.trim_to_fit(note_tokens)?;
        }

        self.content.push_str(note);
        self.content.push('\n');
        self.current_tokens += note_tokens;
        Ok(())
    }

    /// Write the entire scratchpad content (replaces existing).
    pub fn write(&mut self, content: &str) -> Result<(), ScratchpadError> {
        let tokens = TokenEstimator::estimate(content);
        if tokens > self.max_tokens {
            return Err(ScratchpadError::ExceedsLimit {
                tokens,
                max: self.max_tokens,
            });
        }
        self.content = content.to_string();
        self.current_tokens = tokens;
        Ok(())
    }

    /// Read the scratchpad content.
    pub fn read(&self) -> &str {
        &self.content
    }

    /// Format the scratchpad for inclusion in an LLM request.
    pub fn format_for_context(&self) -> String {
        if self.content.is_empty() {
            String::new()
        } else {
            format!(
                "<scratchpad>\n{}\n</scratchpad>",
                self.content
            )
        }
    }

    fn trim_to_fit(&mut self, needed: usize) -> Result<(), ScratchpadError> {
        let target = self.max_tokens.saturating_sub(needed);
        // Remove oldest notes until within target
        while self.current_tokens > target && self.content.contains('\n') {
            let first_newline = self.content.find('\n').unwrap();
            let removed = &self.content[..first_newline];
            self.current_tokens -= TokenEstimator::estimate(removed);
            self.content = self.content[first_newline + 1..].to_string();
        }
        Ok(())
    }
}
```

### 7.3 ScratchpadTool

The agent can explicitly write to the scratchpad via a tool:

```rust
pub struct ScratchpadWriteTool {
    scratchpad: Arc<Mutex<Scratchpad>>,
}

#[derive(Debug, JsonSchema, Deserialize)]
struct ScratchpadWriteInput {
    /// The note to write.
    note: String,
    /// Whether to append or replace.
    mode: ScratchpadMode,
}

#[derive(Debug, JsonSchema, Deserialize)]
enum ScratchpadMode {
    Append,
    Replace,
}

#[async_trait]
impl Tool for ScratchpadWriteTool {
    fn name(&self) -> &str { "scratchpad_write" }
    fn description(&self) -> &str {
        "Write a note to your scratchpad for cross-turn memory. Use this to \
         remember important findings, decisions, or context that you'll need \
         in future turns."
    }

    async fn call(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let input: ScratchpadWriteInput = serde_json::from_value(input)?;
        let mut pad = self.scratchpad.lock().await;
        match input.mode {
            ScratchpadMode::Append => pad.append(&input.note),
            ScratchpadMode::Replace => pad.write(&input.note),
        }.map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        Ok(ToolOutput::Text(format!("Scratchpad updated ({} tokens)", pad.current_tokens)))
    }
}
```

---

## 8. Token Counting

### 8.1 TokenEstimator

```rust
pub struct TokenEstimator;

impl TokenEstimator {
    /// Estimate the number of tokens in a text string.
    /// Uses heuristic: ~3.5 characters per token for code, ~4 for English.
    pub fn estimate(text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        // Count code-like vs prose-like content
        let code_chars = text.chars().filter(|c| {
            matches!(c, '{' | '}' | '(' | ')' | '[' | ']' | ';' | '=' | '<' | '>' | '|' | '&')
        }).count();
        let code_ratio = code_chars as f64 / text.len() as f64;

        let chars_per_token = if code_ratio > 0.1 {
            3.0  // Code is denser
        } else {
            4.0  // English prose
        };

        (text.len() as f64 / chars_per_token).ceil() as usize
    }

    /// Estimate tokens for a list of messages.
    pub fn estimate_messages(messages: &[Message]) -> usize {
        messages.iter()
            .map(|m| Self::estimate(&m.content()) + 4)  // +4 for role overhead
            .sum()
    }
}
```

### 8.2 Exact Token Counting

When available, xauft uses provider-specific tokenizers for exact counting:

```rust
pub enum TokenCounter {
    /// Heuristic estimator (fast, approximate).
    Heuristic,
    /// OpenAI tiktoken (exact for OpenAI models).
    Tiktoken { encoding: tiktoken_rs::CoreBPE },
    /// Anthropic tokenizer.
    Anthropic { client: AnthropicProvider },
    /// Provider API-based counting.
    ProviderApi { provider: Arc<dyn LlmProvider> },
}

impl TokenCounter {
    pub async fn count(&self, text: &str, model: &str) -> usize {
        match self {
            Self::Heuristic => TokenEstimator::estimate(text),
            Self::Tiktoken { encoding } => {
                encoding.encode_with_special_tokens(text).len()
            }
            Self::Anthropic { client } => {
                client.count_tokens(text, model).await
                    .unwrap_or_else(|| TokenEstimator::estimate(text))
            }
            Self::ProviderApi { provider } => {
                provider.count_tokens(text, model).await
                    .unwrap_or_else(|| TokenEstimator::estimate(text))
            }
        }
    }
}
```

---

## 9. Progressive Context Loading

### 9.1 Concept

Rather than loading all context upfront, xauft progressively loads context
as the agent needs it. This reduces initial token usage and allows the agent
to request more context on demand.

```
  Phase 1: Minimal Context
  ┌──────────────────────────────────┐
  │ System Prompt   (2K)            │
  │ Task Desc       (1K)            │
  │ File List       (2K)            │  ← just file names, not contents
  │ Scratchpad      (1K)            │
  └──────────────────────────────────┘
  Total: ~6K tokens

  Phase 2: Focused Context (after agent reads files)
  ┌──────────────────────────────────┐
  │ System Prompt   (2K)            │
  │ Task Desc       (1K)            │
  │ File List       (2K)            │
  │ File Contents   (20K)           │  ← only files the agent requested
  │ Conversation    (5K)            │
  │ Scratchpad      (2K)            │
  └──────────────────────────────────┘
  Total: ~32K tokens

  Phase 3: Deep Context (after RAG search)
  ┌──────────────────────────────────┐
  │ System Prompt   (2K)            │
  │ Task Desc       (1K)            │
  │ Conversation    (15K)           │
  │ RAG Context     (40K)           │  ← relevant code chunks
  │ File Contents   (30K)           │
  │ Scratchpad      (5K)            │
  └──────────────────────────────────┘
  Total: ~93K tokens
```

### 9.2 Implementation

```rust
pub struct ProgressiveContextLoader {
    /// Context budget allocator.
    allocator: ContextBudgetAllocator,
    /// RAG injector.
    rag: RagInjector,
    /// File content truncator.
    truncator: FileContentTruncator,
    /// Conversation summarizer.
    summarizer: ConversationSummarizer<dyn LlmProvider>,
    /// Current loading phase.
    phase: ContextPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextPhase {
    /// Minimal: system prompt + task + file list.
    Minimal,
    /// Focused: + requested file contents + conversation.
    Focused,
    /// Deep: + RAG context + full conversation.
    Deep,
}

impl ProgressiveContextLoader {
    /// Load context for the given phase.
    pub async fn load(
        &mut self,
        task: &Task,
        conversation: &mut Vec<Message>,
        requested_files: &[PathBuf],
        rag_query: Option<&str>,
    ) -> Result<LoadedContext, ContextError> {
        match self.phase {
            ContextPhase::Minimal => {
                self.load_minimal(task).await
            }
            ContextPhase::Focused => {
                self.load_focused(task, conversation, requested_files).await
            }
            ContextPhase::Deep => {
                self.load_deep(task, conversation, requested_files, rag_query).await
            }
        }
    }

    async fn load_minimal(&self, task: &Task) -> Result<LoadedContext, ContextError> {
        let file_list = self.get_file_list().await?;
        Ok(LoadedContext {
            system_prompt: self.build_system_prompt(),
            task_description: task.description.clone(),
            file_list,
            file_contents: String::new(),
            rag_context: String::new(),
            scratchpad: String::new(),
            total_tokens: 0,
            phase: ContextPhase::Minimal,
        })
    }

    async fn load_focused(
        &self,
        task: &Task,
        conversation: &mut Vec<Message>,
        requested_files: &[PathBuf],
    ) -> Result<LoadedContext, ContextError> {
        let budget = self.allocator.allocations.file_contents.max_tokens;
        let mut file_contents = String::new();
        let mut used_tokens = 0;

        for path in requested_files {
            let content = tokio::fs::read_to_string(path).await
                .map_err(|_| ContextError::FileNotFound(path.clone()))?;
            let truncated = self.truncator.truncate(
                &FileContent { path: path.clone(), content, line_count: 0 },
                &[],
                budget.saturating_sub(used_tokens),
            );
            used_tokens += TokenEstimator::estimate(&truncated.content);
            file_contents.push_str(&truncated.content);
            file_contents.push('\n');
        }

        // Apply conversation summarization if needed
        let conv_budget = self.allocator.allocations.conversation.max_tokens;
        self.summarizer.maybe_summarize(conversation, conv_budget).await.ok();

        Ok(LoadedContext {
            file_contents,
            phase: ContextPhase::Focused,
            ..self.load_minimal(task).await?
        })
    }

    async fn load_deep(
        &self,
        task: &Task,
        conversation: &mut Vec<Message>,
        requested_files: &[PathBuf],
        rag_query: Option<&str>,
    ) -> Result<LoadedContext, ContextError> {
        let mut ctx = self.load_focused(task, conversation, requested_files).await?;

        // Add RAG context
        if let Some(query) = rag_query {
            let rag_budget = self.allocator.allocations.rag_context.max_tokens;
            let rag_context = self.rag.retrieve(query, rag_budget).await?;
            ctx.rag_context = rag_context.context_text;
        }

        ctx.phase = ContextPhase::Deep;
        Ok(ctx)
    }
}
```

---

## 10. Configuration Reference

```toml
[xaft.context]

[xaft.context.budget]
# Default allocation percentages (of available context after output reserve)
system_prompt_percent = 2
task_description_percent = 1
conversation_percent = 16
rag_context_percent = 40
file_contents_percent = 32
scratchpad_percent = 4
output_reserve_tokens = 10240

[xaft.context.summarization]
enabled = true
summarize_at_tokens = 15000     # trigger summarization
compression_ratio = 0.3         # target 30% of original
max_summary_tokens = 2048
summarization_model = "gpt-4o-mini"

[xaft.context.memory_window]
enabled = true
max_tokens = 20000              # hard limit on conversation history
min_messages = 4                # always keep last 4 messages
keep_system_messages = true

[xaft.context.rag]
enabled = true
top_k = 10
max_context_tokens = 50000
min_relevance = 0.5
chunk_size_tokens = 512
chunk_overlap_tokens = 64
include_surrounding = true
indexed_extensions = [".rs", ".ts", ".js", ".py", ".go", ".java", ".tsx", ".jsx"]
exclude_dirs = ["node_modules", ".git", "target", "dist", "build"]

[xaft.context.file_truncation]
max_tokens_per_file = 10000
surrounding_lines = 10

[xaft.context.scratchpad]
enabled = true
max_tokens = 5000

[xaft.context.progressive_loading]
enabled = true
initial_phase = "minimal"       # "minimal" | "focused" | "deep"
auto_escalate = true            # auto-escalate phase based on task complexity
```
