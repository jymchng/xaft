# XAFT Repository Indexing — PRD

> Document ID: XAFT-EXEC-004
> Version: 0.1.0-draft
> Status: Design Phase
> Owner: xaft-core team

---

## 1. Overview

For `xaft` to make informed planning and editing decisions, it must deeply understand the repository it operates on. The Repository Indexing system scans the file tree, detects languages, extracts symbols, generates embeddings, and builds a searchable knowledge base. This document specifies the indexing pipeline, RAG/knowledge module, embedding generation, chunk strategies, context injection, and incremental re-indexing.

---

## 2. Architecture

```
 ┌──────────────────────────────────────────────────────────────────────────┐
 │                    Repository Indexing Architecture                       │
 │                                                                          │
 │  ┌──────────────┐   ┌──────────────┐   ┌──────────────┐   ┌──────────┐ │
 │  │  File Tree   │──▶│   Language   │──▶│    Symbol    │──▶│  Chunk   │ │
 │  │  Scanner     │   │  Detection   │   │  Extraction  │   │ Strategy │ │
 │  └──────────────┘   └──────────────┘   └──────────────┘   └────┬─────┘ │
 │                                                                  │      │
 │                                                                  ▼      │
 │  ┌──────────────┐   ┌──────────────┐   ┌──────────────┐   ┌──────────┐ │
 │  │   Context    │◀──│    RAG /     │◀──│  Embedding   │◀──│  Chunk   │ │
 │  │  Injection   │   │  Knowledge   │   │  Generation  │   │  Store   │ │
 │  │  (LLM prompt)│   │  Pipeline    │   │              │   │          │ │
 │  └──────────────┘   └──────┬───────┘   └──────────────┘   └──────────┘ │
 │                            │                                            │
 │                     ┌──────┴───────┐                                   │
 │                     │   Vector     │                                   │
 │                     │   Store      │                                   │
 │                     │ (persistent) │                                   │
 │                     └──────────────┘                                   │
 │                                                                          │
 │  ┌──────────────────────────────────────────────────────────────────┐   │
 │  │                   Incremental Re-Indexer                         │   │
 │  │   (watches for file changes, updates index incrementally)        │   │
 │  └──────────────────────────────────────────────────────────────────┘   │
 └──────────────────────────────────────────────────────────────────────────┘
```

---

## 3. File Tree Scanner

### 3.1 Scanner Design

The scanner walks the repository file tree, respecting `.gitignore` rules and `xaft`-specific ignore patterns.

```rust
/// Scans the repository file tree and builds a structured representation.
pub struct FileTreeScanner {
    repo_root: PathBuf,
    config: ScannerConfig,
    ignore_matcher: GitignoreMatcher,
}

#[derive(Debug, Clone)]
pub struct ScannerConfig {
    /// Maximum file size to index (bytes).
    pub max_file_size: u64,             // default: 1_000_000 (1MB)
    /// Maximum directory depth to scan.
    pub max_depth: usize,               // default: 20
    /// Whether to follow symlinks.
    pub follow_symlinks: bool,          // default: false
    /// Additional ignore patterns (beyond .gitignore).
    pub extra_ignores: Vec<String>,     // default: ["target/", "node_modules/", ".git/"]
    /// Whether to include hidden files (dotfiles).
    pub include_hidden: bool,           // default: false
    /// Number of parallel scanning tasks.
    pub parallelism: usize,             // default: num_cpus
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: PathBuf,
    pub relative_path: PathBuf,
    pub size_bytes: u64,
    pub modified_at: Option<DateTime<Utc>>,
    pub language: Option<Language>,
    pub is_binary: bool,
    pub hash: FileHash,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileHash(pub String);

#[derive(Debug, Clone)]
pub struct ScanResult {
    pub entries: Vec<FileEntry>,
    pub total_files: usize,
    pub total_size_bytes: u64,
    pub languages: HashMap<Language, usize>,
    pub scan_duration: Duration,
}
```

### 3.2 Scanner Implementation

```rust
impl FileTreeScanner {
    pub fn new(repo_root: &Path, config: ScannerConfig) -> Result<Self, ScannerError> {
        let ignore_matcher = GitignoreMatcher::from_repo(repo_root)?;
        Ok(Self {
            repo_root: repo_root.to_path_buf(),
            config,
            ignore_matcher,
        })
    }

    /// Perform a full scan of the repository.
    pub async fn scan(&self) -> Result<ScanResult, ScannerError> {
        let start = Instant::now();
        let mut entries = Vec::new();
        let mut languages: HashMap<Language, usize> = HashMap::new();

        let mut dirs_to_visit = vec![self.repo_root.clone()];
        let mut depth = 0usize;

        while let Some(dir) = dirs_to_visit.pop() {
            if depth > self.config.max_depth {
                continue;
            }

            let mut read_dir = fs::read_dir(&dir).await?;

            while let Some(entry) = read_dir.next_entry().await? {
                let path = entry.path();
                let relative = path.strip_prefix(&self.repo_root)?.to_path_buf();

                // Check ignore rules
                if self.ignore_matcher.is_ignored(&relative) {
                    continue;
                }

                // Check extra ignores
                if self.is_extra_ignored(&relative) {
                    continue;
                }

                // Check hidden files
                if !self.config.include_hidden {
                    let name = relative.file_name().unwrap_or_default();
                    if name.to_string_lossy().starts_with('.') {
                        continue;
                    }
                }

                let metadata = entry.metadata().await?;

                if metadata.is_dir() {
                    dirs_to_visit.push(path);
                } else if metadata.is_file() {
                    // Check file size
                    if metadata.len() > self.config.max_file_size {
                        tracing::debug!("Skipping large file: {} ({} bytes)", relative.display(), metadata.len());
                        continue;
                    }

                    let language = Language::from_path(&relative);
                    let is_binary = self.is_binary_file(&relative);
                    let hash = self.compute_file_hash(&path).await?;

                    if let Some(ref lang) = language {
                        *languages.entry(lang.clone()).or_insert(0) += 1;
                    }

                    entries.push(FileEntry {
                        path,
                        relative_path: relative,
                        size_bytes: metadata.len(),
                        modified_at: metadata.modified().ok().map(|t| {
                            t.into()
                        }),
                        language,
                        is_binary,
                        hash,
                    });
                }
            }
            depth += 1;
        }

        let total_size: u64 = entries.iter().map(|e| e.size_bytes).sum();

        Ok(ScanResult {
            total_files: entries.len(),
            entries,
            total_size_bytes: total_size,
            languages,
            scan_duration: start.elapsed(),
        })
    }
}
```

---

## 4. Language Detection

### 4.1 Detection Strategy

Language detection uses a multi-signal approach:

```
 ┌───────────────────────────────────────────────────────────────────┐
 │                   Language Detection Pipeline                     │
 │                                                                   │
 │  Signal 1: File Extension                                        │
 │  ┌─────────────────────────────────────────────────────────────┐ │
 │  │ .rs → Rust, .py → Python, .ts → TypeScript, .go → Go,    │ │
 │  │ .java → Java, .cpp → C++, .c → C, .rb → Ruby, .js → JS, │ │
 │  │ .tsx → TSX, .jsx → JSX, .md → Markdown, .toml → TOML,   │ │
 │  │ .yaml/.yml → YAML, .json → JSON, .html → HTML, .css → CSS│ │
 │  └─────────────────────────────────────────────────────────────┘ │
 │                                                                   │
 │  Signal 2: Filename Convention                                   │
 │  ┌─────────────────────────────────────────────────────────────┐ │
 │  │ Makefile → Make, Dockerfile → Docker, Vagrantfile → Ruby, │ │
 │  │ Gemfile → Ruby, Pipfile → Python, Cargo.toml → TOML,     │ │
 │  │ go.mod → Go, package.json → JSON                          │ │
 │  └─────────────────────────────────────────────────────────────┘ │
 │                                                                   │
 │  Signal 3: Content Heuristics (fallback)                         │
 │  ┌─────────────────────────────────────────────────────────────┐ │
 │  │ "#!/usr/bin/env python" → Python                           │ │
 │  │ "#!/usr/bin/env node" → JavaScript                        │ │
 │  │ "fn main()" → Rust                                        │ │
 │  │ "package main" → Go                                       │ │
 │  │ "<?php" → PHP                                             │ │
 │  └─────────────────────────────────────────────────────────────┘ │
 └───────────────────────────────────────────────────────────────────┘
```

### 4.2 Language Model

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Tsx,
    Jsx,
    Go,
    Java,
    C,
    Cpp,
    Ruby,
    Php,
    Swift,
    Kotlin,
    CSharp,
    Scala,
    Haskell,
    Shell,
    Sql,
    Html,
    Css,
    Scss,
    Markdown,
    Json,
    Yaml,
    Toml,
    Xml,
    Dockerfile,
    Make,
    Proto,
    GraphQL,
    Unknown(String),
}

impl Language {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension().and_then(|e| e.to_str())?.to_lowercase().as_str() {
            "rs"    => Some(Language::Rust),
            "py"    => Some(Language::Python),
            "ts"    => Some(Language::TypeScript),
            "js"    => Some(Language::JavaScript),
            "tsx"   => Some(Language::Tsx),
            "jsx"   => Some(Language::Jsx),
            "go"    => Some(Language::Go),
            "java"  => Some(Language::Java),
            "c"     => Some(Language::C),
            "cpp" | "cc" | "cxx" => Some(Language::Cpp),
            "rb"    => Some(Language::Ruby),
            "php"   => Some(Language::Php),
            "swift" => Some(Language::Swift),
            "kt"    => Some(Language::Kotlin),
            "cs"    => Some(Language::CSharp),
            "scala" => Some(Language::Scala),
            "hs"    => Some(Language::Haskell),
            "sh" | "bash" | "zsh" => Some(Language::Shell),
            "sql"   => Some(Language::Sql),
            "html"  => Some(Language::Html),
            "css"   => Some(Language::Css),
            "scss"  => Some(Language::Scss),
            "md"    => Some(Language::Markdown),
            "json"  => Some(Language::Json),
            "yaml" | "yml" => Some(Language::Yaml),
            "toml"  => Some(Language::Toml),
            "xml"   => Some(Language::Xml),
            "proto" => Some(Language::Proto),
            "graphql" | "gql" => Some(Language::GraphQL),
            _ => Self::from_filename(path),
        }
    }

    fn from_filename(path: &Path) -> Option<Self> {
        match path.file_name()?.to_str()? {
            "Dockerfile" | "Dockerfile.*" => Some(Language::Dockerfile),
            "Makefile" | "GNUmakefile" => Some(Language::Make),
            _ => None,
        }
    }

    /// Whether this language supports tree-sitter-based symbol extraction.
    pub fn supports_tree_sitter(&self) -> bool {
        matches!(
            self,
            Language::Rust | Language::Python | Language::TypeScript
            | Language::JavaScript | Language::Go | Language::Java
            | Language::C | Language::Cpp | Language::Ruby
        )
    }
}
```

---

## 5. Symbol Extraction

### 5.1 Extraction Architecture

```
 ┌───────────────────────────────────────────────────────────────────────┐
 │                     Symbol Extraction Pipeline                        │
 │                                                                       │
 │   ┌───────────────┐                                                  │
 │   │ Source File   │                                                  │
 │   └───────┬───────┘                                                  │
 │           │                                                           │
 │     ┌─────┴──────────────┐                                           │
 │     ▼                    ▼                                           │
 │  ┌────────────┐    ┌────────────┐                                    │
 │  │ Tree-sitter│    │   Regex    │                                    │
 │  │ (primary)  │    │ (fallback) │                                    │
 │  └─────┬──────┘    └─────┬──────┘                                    │
 │        │                 │                                           │
 │        └────────┬────────┘                                           │
 │                 ▼                                                     │
 │        ┌─────────────────┐                                           │
 │        │  Symbol Table   │                                           │
 │        │  - functions    │                                           │
 │        │  - structs/enums│                                           │
 │        │  - classes      │                                           │
 │        │  - methods      │                                           │
 │        │  - constants    │                                           │
 │        │  - imports      │                                           │
 │        │  - variables    │                                           │
 │        └─────────────────┘                                           │
 └───────────────────────────────────────────────────────────────────────┘
```

### 5.2 Symbol Model

```rust
/// A symbol extracted from a source file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    /// Unique identifier.
    pub id: SymbolId,
    /// Name of the symbol (e.g., "MyStruct", "process_data").
    pub name: String,
    /// Kind of symbol.
    pub kind: SymbolKind,
    /// File path (relative to repo root).
    pub file: PathBuf,
    /// Line number where the symbol is defined.
    pub line_start: usize,
    /// Line number where the symbol ends.
    pub line_end: usize,
    /// Byte offset in the file.
    pub byte_start: usize,
    /// Byte offset end.
    pub byte_end: usize,
    /// Documentation comment (if present).
    pub doc_comment: Option<String>,
    /// Parent symbol (for nested definitions).
    pub parent: Option<SymbolId>,
    /// Visibility/access modifier.
    pub visibility: Visibility,
    /// Language of the source file.
    pub language: Language,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    EnumVariant,
    Interface,
    Trait,
    Class,
    Module,
    Constant,
    Variable,
    TypeAlias,
    Macro,
    Import,
    Field,
    Property,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Visibility {
    Public,
    Private,
    Protected,
    Internal,
    Default,
}

/// A complete symbol table for the repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolTable {
    pub symbols: HashMap<SymbolId, Symbol>,
    /// Index: name → symbol IDs (for fast lookup by name).
    pub name_index: HashMap<String, Vec<SymbolId>>,
    /// Index: file → symbol IDs (for fast lookup by file).
    pub file_index: HashMap<PathBuf, Vec<SymbolId>>,
    /// Index: kind → symbol IDs (for fast lookup by kind).
    pub kind_index: HashMap<SymbolKind, Vec<SymbolId>>,
}
```

### 5.3 Tree-sitter Extraction

```rust
pub struct TreeSitterExtractor {
    parsers: HashMap<Language, Parser>,
}

impl TreeSitterExtractor {
    pub fn new() -> Result<Self, ExtractionError> {
        let mut parsers = HashMap::new();

        // Initialize parsers for supported languages
        parsers.insert(Language::Rust, Parser::new(&tree_sitter_rust::LANGUAGE)?);
        parsers.insert(Language::Python, Parser::new(&tree_sitter_python::LANGUAGE)?);
        parsers.insert(Language::TypeScript, Parser::new(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT)?);
        parsers.insert(Language::JavaScript, Parser::new(&tree_sitter_javascript::LANGUAGE)?);
        parsers.insert(Language::Go, Parser::new(&tree_sitter_go::LANGUAGE)?);

        Ok(Self { parsers })
    }

    /// Extract symbols from a source file using tree-sitter.
    pub fn extract(&self, file: &FileEntry, content: &str) -> Result<Vec<Symbol>, ExtractionError> {
        let language = file.language.as_ref()
            .ok_or(ExtractionError::NoLanguage { path: file.relative_path.clone() })?;

        let parser = self.parsers.get(language)
            .ok_or(ExtractionError::UnsupportedLanguage {
                language: language.clone(),
            })?;

        let tree = parser.parse(content, None)
            .ok_or(ExtractionError::ParseFailed { path: file.relative_path.clone() })?;

        let mut symbols = Vec::new();
        let mut cursor = tree.root_node().walk();

        self.visit_node(&mut cursor, &mut symbols, content, &file.relative_path, None);

        Ok(symbols)
    }

    fn visit_node(
        &self,
        cursor: &mut tree_sitter::TreeCursor,
        symbols: &mut Vec<Symbol>,
        content: &str,
        file_path: &Path,
        parent_id: Option<SymbolId>,
    ) {
        let node = cursor.node();

        if let Some(symbol) = self.node_to_symbol(node, content, file_path, parent_id) {
            let symbol_id = symbol.id;
            symbols.push(symbol);

            // Recurse into children with this symbol as parent
            if cursor.goto_first_child() {
                loop {
                    self.visit_node(cursor, symbols, content, file_path, Some(symbol_id));
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
                cursor.goto_parent();
            }
        } else {
            // No symbol for this node; recurse into children
            if cursor.goto_first_child() {
                loop {
                    self.visit_node(cursor, symbols, content, file_path, parent_id);
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
                cursor.goto_parent();
            }
        }
    }

    fn node_to_symbol(
        &self,
        node: tree_sitter::Node,
        content: &str,
        file_path: &Path,
        parent_id: Option<SymbolId>,
    ) -> Option<Symbol> {
        let kind = match node.kind() {
            "function_item" | "function_definition" => SymbolKind::Function,
            "impl_item" => SymbolKind::Trait, // simplified
            "struct_item" | "struct_definition" | "class_definition" => SymbolKind::Struct,
            "enum_item" | "enum_definition" => SymbolKind::Enum,
            "type_item" | "type_alias" => SymbolKind::TypeAlias,
            "trait_item" | "protocol_definition" => SymbolKind::Trait,
            "constant_item" | "const_declaration" => SymbolKind::Constant,
            "mod_item" | "module" => SymbolKind::Module,
            "macro_definition" => SymbolKind::Macro,
            "use_declaration" | "import_statement" | "import_from_statement" => SymbolKind::Import,
            "field_definition" | "property_definition" => SymbolKind::Field,
            _ => return None,
        };

        let name_node = node.child_by_field_name("name")?;
        let name = name_node.utf8_text(content.as_bytes()).ok()?;

        let line_start = node.start_position().row + 1;
        let line_end = node.end_position().row + 1;

        // Extract doc comment (Rust: /// or //!)
        let doc_comment = self.extract_doc_comment(node, content);

        Some(Symbol {
            id: SymbolId::new(),
            name: name.to_string(),
            kind,
            file: file_path.to_path_buf(),
            line_start,
            line_end,
            byte_start: node.start_byte(),
            byte_end: node.end_byte(),
            doc_comment,
            parent: parent_id,
            visibility: Visibility::Default, // simplified
            language: Language::from_path(file_path).unwrap_or(Language::Unknown("".into())),
        })
    }

    fn extract_doc_comment(&self, node: tree_sitter::Node, content: &str) -> Option<String> {
        // Walk backwards from the node to find preceding doc comments
        let mut prev = node.prev_named_sibling()?;
        if prev.kind() == "line_comment" || prev.kind() == "block_comment" {
            prev.utf8_text(content.as_bytes()).ok().map(|s| s.to_string())
        } else {
            None
        }
    }
}
```

---

## 6. RAG / Knowledge Pipeline

### 6.1 Pipeline Overview

```
 ┌──────────────────────────────────────────────────────────────────────────┐
 │                        RAG / Knowledge Pipeline                          │
 │                                                                          │
 │  ┌───────────┐    ┌───────────────┐    ┌──────────────┐   ┌──────────┐ │
 │  │  Query     │───▶│  Embedding    │───▶│   Vector     │──▶│  Result  │ │
 │  │  (user     │    │  Generation   │    │   Search     │   │  Chunks  │ │
 │  │   intent)  │    │  (query →     │    │  (cosine     │   │          │ │
 │  │            │    │   vector)     │    │   similarity)│   │          │ │
 │  └───────────┘    └───────────────┘    └──────────────┘   └────┬─────┘ │
 │                                                                  │      │
 │                                                                  ▼      │
 │  ┌───────────┐    ┌───────────────┐    ┌──────────────┐   ┌──────────┐ │
 │  │  Context   │◀──│  Re-ranking   │◀───│  Expansion   │◀──│  Top-K   │ │
 │  │  Injection │    │  (cross-enc.) │    │  (query      │   │  Chunks  │ │
 │  │  (into LLM │    │              │    │   expansion)  │   │          │ │
 │  │   prompt)  │    │              │    │              │   │          │ │
 │  └───────────┘    └───────────────┘    └──────────────┘   └──────────┘ │
 └──────────────────────────────────────────────────────────────────────────┘
```

### 6.2 Knowledge Module

```rust
/// The RAG knowledge module provides semantic search over repository content.
pub struct KnowledgeModule {
    vector_store: VectorStore,
    embedder: Embedder,
    chunk_store: ChunkStore,
    config: KnowledgeConfig,
}

#[derive(Debug, Clone)]
pub struct KnowledgeConfig {
    /// Embedding model to use.
    pub embedding_model: EmbeddingModel,
    /// Number of results to return from initial search.
    pub top_k: usize,                  // default: 20
    /// Number of results to return after re-ranking.
    pub final_k: usize,                // default: 5
    /// Minimum similarity score for a result to be included.
    pub min_similarity: f32,           // default: 0.5
    /// Whether to use query expansion.
    pub use_query_expansion: bool,     // default: true
    /// Whether to use cross-encoder re-ranking.
    pub use_reranking: bool,           // default: true
    /// Maximum context tokens to inject into the LLM prompt.
    pub max_context_tokens: usize,     // default: 8000
}

impl KnowledgeModule {
    /// Search the knowledge base for context relevant to a query.
    pub async fn search(&self, query: &str) -> Result<SearchResult, KnowledgeError> {
        // Step 1: Generate query embedding
        let query_embedding = self.embedder.embed(query).await?;

        // Step 2: Query expansion (optional)
        let queries = if self.config.use_query_expansion {
            self.expand_query(query).await?
        } else {
            vec![query.to_string()]
        };

        // Step 3: Vector search for each expanded query
        let mut all_chunks: Vec<ScoredChunk> = Vec::new();
        for q in &queries {
            let embedding = if q == query {
                query_embedding.clone()
            } else {
                self.embedder.embed(q).await?
            };

            let results = self.vector_store.search(
                &embedding,
                self.config.top_k,
                self.config.min_similarity,
            ).await?;

            all_chunks.extend(results);
        }

        // Deduplicate
        all_chunks.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
        all_chunks.dedup_by(|a, b| a.chunk.id == b.chunk.id);

        // Step 4: Re-rank with cross-encoder (optional)
        let ranked = if self.config.use_reranking {
            self.rerank(query, all_chunks).await?
        } else {
            all_chunks
        };

        // Step 5: Select top-K results
        let final_chunks: Vec<ScoredChunk> = ranked
            .into_iter()
            .take(self.config.final_k)
            .collect();

        // Step 6: Check token budget
        let truncated = self.truncate_to_token_budget(final_chunks);

        Ok(SearchResult {
            query: query.to_string(),
            chunks: truncated,
            total_candidates: all_chunks.len(),
        })
    }

    async fn expand_query(&self, query: &str) -> Result<Vec<String>, KnowledgeError> {
        // Use the LLM to generate alternative phrasings
        let prompt = format!(
            "Generate 3 alternative search queries for finding relevant code. \
             Original query: \"{}\". \
             Return one per line, no numbering.",
            query
        );

        let response = self.embedder.llm_client().chat(prompt).await?;
        let mut queries = vec![query.to_string()];

        for line in response.content.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                queries.push(trimmed.to_string());
            }
        }

        Ok(queries)
    }

    async fn rerank(
        &self,
        query: &str,
        chunks: Vec<ScoredChunk>,
    ) -> Result<Vec<ScoredChunk>, KnowledgeError> {
        // Use a cross-encoder to re-score chunk relevance
        let mut reranked = Vec::new();
        for scored in chunks {
            let score = self.cross_encoder_score(query, &scored.chunk.content).await?;
            reranked.push(ScoredChunk {
                chunk: scored.chunk,
                score,
            });
        }
        reranked.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
        Ok(reranked)
    }

    fn truncate_to_token_budget(&self, chunks: Vec<ScoredChunk>) -> Vec<ScoredChunk> {
        let mut result = Vec::new();
        let mut token_count = 0;

        for chunk in chunks {
            let chunk_tokens = self.estimate_tokens(&chunk.chunk.content);
            if token_count + chunk_tokens > self.config.max_context_tokens {
                break;
            }
            token_count += chunk_tokens;
            result.push(chunk);
        }

        result
    }

    fn estimate_tokens(&self, text: &str) -> usize {
        // Rough estimation: 1 token ≈ 4 characters
        text.len() / 4
    }
}
```

---

## 7. Embedding Generation

### 7.1 Embedding Models

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmbeddingModel {
    /// Local embedding via ONNX runtime (e.g., all-MiniLM-L6-v2).
    LocalOnnx {
        model_path: PathBuf,
        dimensions: usize,           // default: 384
    },
    /// OpenAI text-embedding-3-small.
    OpenAi {
        model: String,               // default: "text-embedding-3-small"
        dimensions: usize,           // default: 1536
    },
    /// Custom embedding endpoint.
    Custom {
        endpoint: String,
        dimensions: usize,
        auth_header: Option<String>,
    },
}

pub struct Embedder {
    model: EmbeddingModel,
    client: reqwest::Client,
    onnx_session: Option<Session>,  // for local models
}

impl Embedder {
    /// Generate an embedding for a single text.
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        match &self.model {
            EmbeddingModel::LocalOnnx { model_path, dimensions } => {
                self.embed_local(text, *dimensions)
            }
            EmbeddingModel::OpenAi { model, dimensions } => {
                self.embed_openai(text, model, *dimensions).await
            }
            EmbeddingModel::Custom { endpoint, dimensions, auth_header } => {
                self.embed_custom(text, endpoint, *dimensions, auth_header).await
            }
        }
    }

    /// Generate embeddings for a batch of texts (more efficient).
    pub async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        // Most APIs support batch embedding
        match &self.model {
            EmbeddingModel::OpenAi { model, dimensions } => {
                let request = serde_json::json!({
                    "model": model,
                    "input": texts,
                    "dimensions": dimensions,
                });

                let response = self.client
                    .post("https://api.openai.com/v1/embeddings")
                    .json(&request)
                    .send()
                    .await?;

                let result: OpenAiEmbeddingResponse = response.json().await?;
                Ok(result.data.into_iter()
                    .map(|d| d.embedding)
                    .collect())
            }
            _ => {
                // Fall back to individual embedding
                let mut embeddings = Vec::new();
                for text in texts {
                    embeddings.push(self.embed(text).await?);
                }
                Ok(embeddings)
            }
        }
    }

    fn embed_local(&self, text: &str, dimensions: usize) -> Result<Vec<f32>, EmbeddingError> {
        // Use ONNX runtime to run the model locally
        let session = self.onnx_session.as_ref()
            .ok_or(EmbeddingError::OnnxSessionNotInitialized)?;

        // Tokenize input (simplified; real impl would use a proper tokenizer)
        let tokens = self.tokenize(text);

        // Run inference
        let input_tensor = Ort::value_from_array(&tokens)?;
        let outputs = session.run(vec![input_tensor])?;

        // Extract embedding vector
        let embedding: Vec<f32> = outputs[0]
            .try_into()
            .map_err(|_| EmbeddingError::OutputExtraction)?;

        Ok(embedding)
    }
}
```

---

## 8. Chunk Strategies

### 8.1 Strategy Overview

```
 ┌──────────────────────────────────────────────────────────────────────┐
 │                       Chunk Strategies                                │
 │                                                                      │
 │  Strategy 1: Fixed-Size Chunks                                      │
 │  ┌────────────────────────────────────────────────────────────────┐ │
 │  │ Split content into fixed-size blocks of N tokens.             │ │
 │  │ Overlap of O tokens between consecutive chunks.               │ │
 │  │ Pros: Simple, predictable size.                                │ │
 │  │ Cons: May split mid-function/mid-class.                       │ │
 │  └────────────────────────────────────────────────────────────────┘ │
 │                                                                      │
 │  Strategy 2: AST-Aware Chunks                                       │
 │  ┌────────────────────────────────────────────────────────────────┐ │
 │  │ Use tree-sitter AST to split at natural boundaries:           │ │
 │  │ functions, classes, modules, impl blocks.                     │ │
 │  │ Pros: Preserves semantic coherence.                            │ │
 │  │ Cons: Requires language support; variable chunk sizes.        │ │
 │  └────────────────────────────────────────────────────────────────┘ │
 │                                                                      │
 │  Strategy 3: Symbol-Based Chunks                                    │
 │  ┌────────────────────────────────────────────────────────────────┐ │
 │  │ Each symbol (function, class, etc.) becomes its own chunk.    │ │
 │  │ Include doc comments and imports.                             │ │
 │  │ Pros: Best semantic granularity.                               │ │
 │  │ Cons: Small symbols may lack context; large ones exceed size. │ │
 │  └────────────────────────────────────────────────────────────────┘ │
 │                                                                      │
 │  Strategy 4: Hybrid (Recommended)                                   │
 │  ┌────────────────────────────────────────────────────────────────┐ │
 │  │ Use AST-aware chunks as primary; fall back to fixed-size for  │ │
 │  │ unsupported languages. Merge small chunks; split large ones.  │ │
 │  └────────────────────────────────────────────────────────────────┘ │
 └──────────────────────────────────────────────────────────────────────┘
```

### 8.2 Chunk Model

```rust
/// A chunk of repository content, suitable for embedding and retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    /// Unique identifier.
    pub id: ChunkId,
    /// Content of the chunk.
    pub content: String,
    /// File path.
    pub file: PathBuf,
    /// Start line in the source file.
    pub line_start: usize,
    /// End line in the source file.
    pub line_end: usize,
    /// Language of the source file.
    pub language: Language,
    /// Chunking strategy used.
    pub strategy: ChunkStrategy,
    /// Associated symbols (if AST-aware).
    pub symbols: Vec<SymbolId>,
    /// Embedding vector (populated after embedding generation).
    pub embedding: Option<Vec<f32>>,
    /// Hash of the content (for incremental re-indexing).
    pub content_hash: ContentHash,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ChunkStrategy {
    FixedSize { chunk_tokens: usize, overlap_tokens: usize },
    AstAware,
    SymbolBased,
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentHash(pub String);
```

### 8.3 Hybrid Chunker Implementation

```rust
pub struct HybridChunker {
    config: ChunkConfig,
    tree_sitter: TreeSitterExtractor,
}

#[derive(Debug, Clone)]
pub struct ChunkConfig {
    /// Maximum tokens per chunk.
    pub max_chunk_tokens: usize,      // default: 500
    /// Minimum tokens per chunk (smaller chunks are merged).
    pub min_chunk_tokens: usize,      // default: 50
    /// Overlap tokens for fixed-size fallback.
    pub overlap_tokens: usize,        // default: 50
    /// Primary strategy.
    pub strategy: ChunkStrategy,      // default: Hybrid
}

impl HybridChunker {
    pub fn chunk_file(
        &self,
        file: &FileEntry,
        content: &str,
    ) -> Result<Vec<Chunk>, ChunkError> {
        let language = file.language.as_ref()
            .ok_or(ChunkError::NoLanguage)?;

        if language.supports_tree_sitter() {
            // Use AST-aware chunking
            self.ast_aware_chunk(file, content)
        } else {
            // Fall back to fixed-size chunking
            self.fixed_size_chunk(file, content)
        }
    }

    fn ast_aware_chunk(
        &self,
        file: &FileEntry,
        content: &str,
    ) -> Result<Vec<Chunk>, ChunkError> {
        let symbols = self.tree_sitter.extract(file, content)?;

        let mut chunks = Vec::new();
        let mut current_buffer = String::new();
        let mut buffer_start_line = 0;
        let mut buffer_symbols = Vec::new();

        for symbol in &symbols {
            let symbol_content = self.extract_symbol_content(content, symbol);
            let symbol_tokens = self.estimate_tokens(&symbol_content);

            if symbol_tokens > self.config.max_chunk_tokens {
                // Symbol is too large; split it further
                if !current_buffer.is_empty() {
                    chunks.push(self.make_chunk(
                        &current_buffer, file, buffer_start_line,
                        symbol.line_start - 1, &buffer_symbols,
                    ));
                    current_buffer.clear();
                    buffer_symbols.clear();
                }

                // Split the large symbol into fixed-size sub-chunks
                let sub_chunks = self.split_large_symbol(file, content, symbol)?;
                chunks.extend(sub_chunks);
            } else if self.estimate_tokens(&current_buffer) + symbol_tokens
                > self.config.max_chunk_tokens
            {
                // Buffer would overflow; flush it first
                chunks.push(self.make_chunk(
                    &current_buffer, file, buffer_start_line,
                    symbol.line_start - 1, &buffer_symbols,
                ));
                current_buffer = symbol_content;
                buffer_start_line = symbol.line_start;
                buffer_symbols = vec![symbol.id];
            } else {
                // Add to current buffer
                if current_buffer.is_empty() {
                    buffer_start_line = symbol.line_start;
                }
                current_buffer.push_str(&symbol_content);
                current_buffer.push('\n');
                buffer_symbols.push(symbol.id);
            }
        }

        // Flush remaining buffer
        if !current_buffer.is_empty() {
            let last_line = content.lines().count();
            chunks.push(self.make_chunk(
                &current_buffer, file, buffer_start_line,
                last_line, &buffer_symbols,
            ));
        }

        Ok(chunks)
    }

    fn fixed_size_chunk(
        &self,
        file: &FileEntry,
        content: &str,
    ) -> Result<Vec<Chunk>, ChunkError> {
        let lines: Vec<&str> = content.lines().collect();
        let tokens_per_line = self.estimate_tokens(content) / lines.len().max(1);
        let lines_per_chunk = self.config.max_chunk_tokens / tokens_per_line.max(1);
        let overlap_lines = self.config.overlap_tokens / tokens_per_line.max(1);

        let mut chunks = Vec::new();
        let mut start = 0;

        while start < lines.len() {
            let end = (start + lines_per_chunk).min(lines.len());
            let chunk_content: String = lines[start..end].join("\n");
            let chunk_tokens = self.estimate_tokens(&chunk_content);

            if chunk_tokens >= self.config.min_chunk_tokens {
                chunks.push(Chunk {
                    id: ChunkId::new(),
                    content: chunk_content,
                    file: file.relative_path.clone(),
                    line_start: start + 1,
                    line_end: end,
                    language: file.language.clone().unwrap_or(Language::Unknown("".into())),
                    strategy: ChunkStrategy::FixedSize {
                        chunk_tokens: self.config.max_chunk_tokens,
                        overlap_tokens: self.config.overlap_tokens,
                    },
                    symbols: vec![],
                    embedding: None,
                    content_hash: ContentHash::compute(&chunk_content),
                });
            }

            start += lines_per_chunk - overlap_lines;
        }

        Ok(chunks)
    }

    fn make_chunk(
        &self,
        content: &str,
        file: &FileEntry,
        start_line: usize,
        end_line: usize,
        symbols: &[SymbolId],
    ) -> Chunk {
        Chunk {
            id: ChunkId::new(),
            content: content.to_string(),
            file: file.relative_path.clone(),
            line_start: start_line,
            line_end: end_line,
            language: file.language.clone().unwrap_or(Language::Unknown("".into())),
            strategy: ChunkStrategy::AstAware,
            symbols: symbols.to_vec(),
            embedding: None,
            content_hash: ContentHash::compute(content),
        }
    }
}
```

---

## 9. Context Injection

### 9.1 Injection Strategy

Context from the knowledge module is injected into the LLM prompt in a structured format.

```rust
/// Injects retrieved context into the LLM prompt.
pub struct ContextInjector {
    config: InjectionConfig,
}

#[derive(Debug, Clone)]
pub struct InjectionConfig {
    /// Template for the context section of the prompt.
    pub template: String,
    /// Maximum context tokens.
    pub max_context_tokens: usize,    // default: 8000
    /// Whether to include file paths in the context.
    pub include_file_paths: bool,     // default: true
    /// Whether to include line numbers in the context.
    pub include_line_numbers: bool,   // default: true
    /// Priority ordering for context sources.
    pub priority: Vec<ContextSource>, // default: [Symbol, Chunk, FileListing]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContextSource {
    Symbol,
    Chunk,
    FileListing,
    GitHistory,
}

impl ContextInjector {
    /// Build the context section for an LLM prompt.
    pub fn inject(&self, search_result: &SearchResult, symbol_table: &SymbolTable) -> String {
        let mut context = String::new();

        context.push_str("## Repository Context\n\n");

        for (i, scored_chunk) in search_result.chunks.iter().enumerate() {
            let chunk = &scored_chunk.chunk;

            context.push_str(&format!("### Context Block {} (score: {:.3})\n", i + 1, scored_chunk.score));

            if self.config.include_file_paths {
                context.push_str(&format!("File: `{}`", chunk.file.display()));
                if self.config.include_line_numbers {
                    context.push_str(&format!(" (lines {}-{})", chunk.line_start, chunk.line_end));
                }
                context.push('\n');
            }

            // Look up associated symbols
            if !chunk.symbols.is_empty() {
                let symbol_names: Vec<String> = chunk.symbols.iter()
                    .filter_map(|sid| symbol_table.symbols.get(sid))
                    .map(|s| format!("{} ({:?})", s.name, s.kind))
                    .collect();
                context.push_str(&format!("Symbols: {}\n", symbol_names.join(", ")));
            }

            context.push('\n');
            context.push_str("```\n");
            context.push_str(&chunk.content);
            context.push_str("\n```\n\n");
        }

        context
    }
}
```

### 9.2 Prompt Assembly

```
 ┌─────────────────────────────────────────────────────────────────┐
 │                    LLM Prompt Assembly                          │
 │                                                                 │
 │  ┌─────────────────────────────────────────────────────────┐   │
 │  │ System Prompt                                            │   │
 │  │ - Role: autonomous coding agent                          │   │
 │  │ - Available tools                                        │   │
 │  │ - Operating constraints                                  │   │
 │  └─────────────────────────────────────────────────────────┘   │
 │                                                                 │
 │  ┌─────────────────────────────────────────────────────────┐   │
 │  │ Repository Context (from ContextInjector)                │   │
 │  │ - Relevant code chunks                                   │   │
 │  │ - Symbol information                                     │   │
 │  │ - File listings                                          │   │
 │  └─────────────────────────────────────────────────────────┘   │
 │                                                                 │
 │  ┌─────────────────────────────────────────────────────────┐   │
 │  │ Intent / Task                                            │   │
 │  │ - Goal                                                   │   │
 │  │ - Constraints                                            │   │
 │  │ - Acceptance criteria                                    │   │
 │  └─────────────────────────────────────────────────────────┘   │
 │                                                                 │
 │  ┌─────────────────────────────────────────────────────────┐   │
 │  │ Conversation History                                     │   │
 │  │ - Prior turns (if any)                                   │   │
 │  └─────────────────────────────────────────────────────────┘   │
 └─────────────────────────────────────────────────────────────────┘
```

---

## 10. Incremental Re-Indexing

### 10.1 Change Detection

```
 ┌──────────────────────────────────────────────────────────────────────┐
 │                     Incremental Re-Indexing Flow                      │
 │                                                                      │
 │  File Change Detected (inotify / polling / git diff)                │
 │         │                                                            │
 │         ▼                                                            │
 │  ┌──────────────────┐                                               │
 │  │ Compute File Hash │                                               │
 │  └────────┬─────────┘                                               │
 │           │                                                          │
 │     ┌─────┴──────────────────────┐                                  │
 │     ▼                            ▼                                  │
 │  Hash Changed                 Hash Unchanged                         │
 │     │                            │                                  │
 │     ▼                            ▼                                  │
 │  ┌──────────────┐          ┌──────────────┐                        │
 │  │ Re-index File│          │    Skip      │                        │
 │  │ - Re-scan    │          └──────────────┘                        │
 │  │ - Re-extract │                                                  │
 │  │ - Re-chunk   │                                                  │
 │  │ - Re-embed   │                                                  │
 │  │ - Update     │                                                  │
 │  │   vector     │                                                  │
 │  │   store      │                                                  │
 │  └──────────────┘                                                  │
 └──────────────────────────────────────────────────────────────────────┘
```

### 10.2 Implementation

```rust
/// Manages incremental re-indexing of the repository.
pub struct IncrementalIndexer {
    repo_root: PathBuf,
    scanner: FileTreeScanner,
    chunker: HybridChunker,
    embedder: Embedder,
    vector_store: VectorStore,
    chunk_store: ChunkStore,
    index_state: IndexState,
    config: IndexerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexState {
    /// Map of file paths to their last-known content hashes.
    pub file_hashes: HashMap<PathBuf, ContentHash>,
    /// Map of file paths to their chunk IDs.
    pub file_chunks: HashMap<PathBuf, Vec<ChunkId>>,
    /// Last full index timestamp.
    pub last_full_index: DateTime<Utc>,
    /// Version counter for the index.
    pub version: u64,
}

#[derive(Debug, Clone)]
pub struct IndexerConfig {
    /// Whether to watch for file changes in real-time.
    pub watch_enabled: bool,          // default: true
    /// Debounce interval for file change events (ms).
    pub debounce_ms: u64,             // default: 500
    /// Interval for periodic full re-index (seconds). 0 = disabled.
    pub periodic_reindex_secs: u64,   // default: 0 (disabled)
    /// Maximum number of files to re-index in one batch.
    pub batch_size: usize,            // default: 50
}

impl IncrementalIndexer {
    /// Perform a full index of the repository.
    pub async fn full_index(&mut self) -> Result<IndexReport, IndexError> {
        let scan_result = self.scanner.scan().await?;
        let mut report = IndexReport::default();

        for file in &scan_result.entries {
            if file.is_binary || file.language.is_none() {
                continue;
            }

            let content = fs::read_to_string(&file.path).await?;
            let chunks = self.chunker.chunk_file(file, &content)?;

            // Generate embeddings for all chunks
            let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
            let embeddings = self.embedder.embed_batch(&texts).await?;

            // Store chunks and embeddings
            let chunk_ids: Vec<ChunkId> = chunks.iter().map(|c| c.id).collect();
            for (mut chunk, embedding) in chunks.into_iter().zip(embeddings.into_iter()) {
                chunk.embedding = Some(embedding);
                self.chunk_store.put(chunk)?;
            }

            // Update vector store
            let vectors: Vec<(ChunkId, Vec<f32>)> = chunk_ids.iter()
                .zip(self.chunk_store.get_embeddings(&chunk_ids)?)
                .collect();
            self.vector_store.upsert(vectors).await?;

            // Update index state
            self.index_state.file_hashes.insert(
                file.relative_path.clone(),
                file.hash.clone(),
            );
            self.index_state.file_chunks.insert(
                file.relative_path.clone(),
                chunk_ids,
            );

            report.files_indexed += 1;
            report.chunks_created += chunk_ids.len();
        }

        self.index_state.last_full_index = Utc::now();
        self.index_state.version += 1;

        Ok(report)
    }

    /// Incrementally re-index changed files.
    pub async fn incremental_index(
        &mut self,
        changed_paths: &[PathBuf],
    ) -> Result<IndexReport, IndexError> {
        let mut report = IndexReport::default();

        for path in changed_paths {
            let relative = path.strip_prefix(&self.repo_root)?.to_path_buf();

            // Compute new hash
            let content = match fs::read_to_string(path).await {
                Ok(c) => c,
                Err(_) => {
                    // File was deleted
                    self.remove_file_from_index(&relative)?;
                    report.files_removed += 1;
                    continue;
                }
            };

            let new_hash = ContentHash::compute(&content);

            // Check if hash changed
            if let Some(existing_hash) = self.index_state.file_hashes.get(&relative) {
                if *existing_hash == new_hash {
                    continue; // No change
                }
            }

            // Remove old chunks for this file
            self.remove_file_from_index(&relative)?;

            // Re-index the file
            let file_entry = FileEntry {
                path: path.clone(),
                relative_path: relative.clone(),
                size_bytes: content.len() as u64,
                modified_at: None,
                language: Language::from_path(&relative),
                is_binary: false,
                hash: new_hash.clone(),
            };

            let chunks = self.chunker.chunk_file(&file_entry, &content)?;
            let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
            let embeddings = self.embedder.embed_batch(&texts).await?;

            let chunk_ids: Vec<ChunkId> = chunks.iter().map(|c| c.id).collect();
            for (mut chunk, embedding) in chunks.into_iter().zip(embeddings.into_iter()) {
                chunk.embedding = Some(embedding);
                self.chunk_store.put(chunk)?;
            }

            let vectors: Vec<(ChunkId, Vec<f32>)> = chunk_ids.iter()
                .zip(self.chunk_store.get_embeddings(&chunk_ids)?)
                .collect();
            self.vector_store.upsert(vectors).await?;

            self.index_state.file_hashes.insert(relative.clone(), new_hash);
            self.index_state.file_chunks.insert(relative.clone(), chunk_ids);

            report.files_reindexed += 1;
            report.chunks_created += chunk_ids.len();
        }

        self.index_state.version += 1;
        Ok(report)
    }

    fn remove_file_from_index(&mut self, relative_path: &Path) -> Result<(), IndexError> {
        if let Some(chunk_ids) = self.index_state.file_chunks.remove(relative_path) {
            for id in &chunk_ids {
                self.chunk_store.delete(id)?;
                self.vector_store.delete(id).await?;
            }
        }
        self.index_state.file_hashes.remove(relative_path);
        Ok(())
    }
}

/// Watch for file changes using inotify/FSEvents.
pub async fn watch_for_changes(
    repo_root: &Path,
    config: &IndexerConfig,
) -> Result<watcher::Receiver<PathBuf>, IndexError> {
    let (sender, receiver) = watch::channel(PathBuf::new());

    let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
        if let Ok(event) = res {
            if event.kind.is_modify() || event.kind.is_create() || event.kind.is_remove() {
                for path in event.paths {
                    let _ = sender.send(path);
                }
            }
        }
    })?;

    watcher.watch(repo_root, RecursiveMode::Recursive)?;

    Ok(receiver)
}
```

---

## 11. Vector Store

### 11.1 Interface

```rust
/// Abstraction over vector storage backends.
pub trait VectorStore: Send + Sync {
    /// Insert or update vectors.
    async fn upsert(&self, vectors: Vec<(ChunkId, Vec<f32>)>) -> Result<(), VectorError>;

    /// Delete vectors by ID.
    async fn delete(&self, ids: &[ChunkId]) -> Result<(), VectorError>;

    /// Search for similar vectors.
    async fn search(
        &self,
        query: &Vec<f32>,
        top_k: usize,
        min_similarity: f32,
    ) -> Result<Vec<ScoredChunk>, VectorError>;
}

/// In-memory vector store (for small repos or testing).
pub struct InMemoryVectorStore {
    vectors: DashMap<ChunkId, Vec<f32>>,
    chunk_store: ChunkStore,
}

/// SQLite-backed vector store (for medium repos).
pub struct SqliteVectorStore {
    conn: rusqlite::Connection,
    chunk_store: ChunkStore,
}

/// Qdrant vector store (for large repos or production use).
pub struct QdrantVectorStore {
    client: qdrant_client::Qdrant,
    collection_name: String,
    chunk_store: ChunkStore,
}
```

---

## 12. Configuration

```toml
# .xaft.toml

[indexing]
# File scanner settings
max_file_size = 1000000
max_depth = 20
follow_symlinks = false
include_hidden = false
parallelism = 4

[indexing.scanner]
extra_ignores = ["target/", "node_modules/", ".git/", "dist/", "build/"]

[indexing.chunking]
strategy = "hybrid"             # fixed_size | ast_aware | symbol_based | hybrid
max_chunk_tokens = 500
min_chunk_tokens = 50
overlap_tokens = 50

[indexing.embedding]
model = "local_onnx"           # local_onnx | openai | custom
dimensions = 384

[indexing.embedding.local_onnx]
model_path = "~/.xaft/models/all-MiniLM-L6-v2.onnx"

[indexing.embedding.openai]
model = "text-embedding-3-small"
dimensions = 1536

[indexing.knowledge]
top_k = 20
final_k = 5
min_similarity = 0.5
use_query_expansion = true
use_reranking = true
max_context_tokens = 8000

[indexing.vector_store]
backend = "sqlite"             # memory | sqlite | qdrant

[indexing.incremental]
watch_enabled = true
debounce_ms = 500
periodic_reindex_secs = 0
batch_size = 50
```

---

## 13. Error Taxonomy

| Error                              | Code   | Recovery                                    |
|------------------------------------|--------|---------------------------------------------|
| `ScannerError::PermissionDenied`  | I-001  | Check file permissions                      |
| `ExtractionError::UnsupportedLanguage` | I-002 | Fall back to regex extraction            |
| `ExtractionError::ParseFailed`    | I-003  | Fall back to fixed-size chunking            |
| `ChunkError::NoLanguage`          | I-004  | Use generic chunking strategy               |
| `EmbeddingError::OnnxSessionNotInit` | I-005 | Download model or use API embedding      |
| `EmbeddingError::ApiError`        | I-006  | Retry with backoff; fall back to local      |
| `VectorError::StoreFull`          | I-007  | Migrate to larger backend                   |
| `IndexError::CorruptIndex`        | I-008  | Delete index; trigger full re-index         |

---

## 14. Future Considerations

1. **Multi-repo indexing** — Index multiple repositories and search across them.
2. **Code graph** — Build a call/dependency graph for more intelligent context retrieval.
3. **Semantic versioning of index** — Track index schema version for migration.
4. **Incremental embedding** — Re-embed only changed chunks instead of full files.
5. **Federated search** — Combine local index with web search for external documentation.
6. **Learning from feedback** — Adjust relevance scoring based on which context the agent actually used.
