//! xaft — autonomous coding agent binary entry point.

use std::sync::Arc;

use xaft_config::CliOverrides;
use xaft_config::loader::ConfigLoader;
use xaft_runtime::SessionBackend;
use xaft_runtime::runtime::XaftRuntime;
use xaft_session::SessionManager;

/// xaft's own documentation and Rust source, packed by `build.rs` for the
/// self-introspection tools (`list_xaft_docs`, `inspect_xaft_source`, …).
static CORPUS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/xaft-corpus.bin"));

#[tokio::main]
async fn main() {
    // Before anything builds a tool registry. A damaged archive is recorded
    // by the call and reported by the tools themselves.
    let _ = xaft_tools::install_embedded_corpus(CORPUS);

    let config = ConfigLoader::load(&CliOverrides::default()).unwrap_or_else(|e| {
        eprintln!("xaft: configuration error: {e}");
        std::process::exit(3);
    });

    // Bootstrap SQLite session persistence. Falls back to FsSessionStore if
    // the database can't be opened (permissions, disk full, etc.).
    let (runtime, sessions) = bootstrap_runtime(config).await.unwrap_or_else(|e| {
        eprintln!("xaft: failed to start runtime: {e}");
        std::process::exit(1);
    });

    xaft_cli::run(Arc::new(runtime), sessions).await;
}

/// Build the runtime and the history-reading session handle.
///
/// Returns the runtime plus an `Arc<dyn SessionBackend>` that is the *same*
/// `SessionManager` the runtime's session store came from — not a second open
/// of the same directory, and not an independent resolution of
/// `core.data_dir`. That is what makes `xaft session list` and
/// `xaft session inspect` structurally unable to disagree.
///
/// The handle is `None` only on the fallback path: when SQLite cannot be
/// opened, the runtime uses `FsSessionStore`, which keeps no message history,
/// so `xaft session export`/`inspect` have nothing to read and say so rather
/// than quietly consulting a different database.
async fn bootstrap_runtime(
    config: xaft_config::XaftConfig,
) -> Result<(XaftRuntime, Option<Arc<dyn SessionBackend>>), xaft_runtime::RuntimeError> {
    let runtime = XaftRuntime::bootstrap(config.clone()).await?;

    // Upgrade to SQLite session store if possible
    let (runtime, sessions) = match SessionManager::new(&config.core.data_dir).await {
        Ok(mgr) => {
            tracing::info!("xaft: SQLite session store active");
            let backend: Arc<dyn SessionBackend> = Arc::new(mgr.clone());
            (
                runtime.with_stores(mgr.session_store(), mgr.conversation_store()),
                Some(backend),
            )
        }
        Err(e) => {
            tracing::warn!(error = %e, "xaft: SQLite session store unavailable, using FsSessionStore");
            (runtime, None)
        }
    };

    // Bootstrap memory system if enabled
    let runtime = if config.memory.enabled {
        let backend = match config.memory.backend.as_str() {
            "in_memory" => xaft_memory::config::MemoryBackend::InMemory,
            _ => xaft_memory::config::MemoryBackend::Sqlite,
        };
        let mem_config = xaft_memory::MemoryConfig {
            enabled: config.memory.enabled,
            backend: backend.clone(),
            auto_remember: config.memory.auto_remember,
            auto_summarize: config.memory.auto_summarize,
            project_scope_default: config.memory.project_scope_default,
            max_entries: config.memory.max_entries,
            // Previously hardcoded to `None`, which made this the one field
            // on the runtime type with no way for a user to set it — see
            // `RUNTIME_ONLY_FIELDS` in crates/xaft-memory/src/config.rs.
            auto_remember_ttl_secs: config.memory.auto_remember_ttl_secs,
            max_search_results: config.memory.max_search_results,
        };
        let workspace_id = std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "default".into());

        // `project_memory_path` relocates the SQLite store (parity row 120).
        // Absent, the manager falls back to the platform data directory.
        let mem_dir = config
            .memory
            .project_memory_path
            .as_deref()
            .map(std::path::PathBuf::from);

        match xaft_memory::XaftMemoryManager::new_in_dir(
            backend,
            mem_config,
            workspace_id,
            Some(runtime.signals().clone()),
            mem_dir.as_deref(),
        )
        .await
        {
            Ok(mgr) => {
                tracing::info!("xaft: memory system active");
                runtime.with_memory(Arc::new(mgr))
            }
            Err(e) => {
                tracing::warn!(error = %e, "xaft: memory system unavailable");
                runtime
            }
        }
    } else {
        tracing::info!("xaft: memory system disabled");
        runtime
    };

    Ok((runtime, sessions))
}
// ── Bootstrap wiring ────────────────────────────────────────────────────────
//
// `bootstrap_runtime` is the ONLY place where a loaded `XaftConfig` is turned
// into live subsystems, so it is the only place that can answer "does the
// config I wrote actually reach the SQLite session store / the memory
// manager?". Before these tests the whole binary crate was 0/51 covered: the
// suite exercised the subsystems in isolation and nothing proved the wiring.
//
// Every assertion below is made against an on-disk artefact the subsystem is
// required to produce, not against a field on the struct under test — the
// manager handles are private to `xaft-runtime`, so (and only so) the test
// observes what the user would actually see on disk.

#[cfg(test)]
mod corpus_tests {
    use super::CORPUS;
    use xaft_tools::Corpus;
    use xaft_tools::introspect::{EntryKind, rust_items};

    /// The binary embeds the docs and the source the tools promise, at a
    /// size worth carrying.
    #[test]
    fn the_embedded_corpus_carries_xafts_docs_and_source() {
        let corpus = Corpus::from_static(CORPUS).expect("build.rs wrote a valid archive");
        for doc in [
            "llms.txt",
            "llms-full.txt",
            "README.md",
            "guides/tools.md",
            "parity-matrix.md",
        ] {
            assert!(
                corpus.find(EntryKind::Doc, doc).is_some(),
                "{doc} is not embedded"
            );
        }
        for file in [
            "src/main.rs",
            "crates/xaft-tools/src/introspect/mod.rs",
            "crates/xaft-runtime/src/runtime.rs",
            "crates/xaft-tools/tests/tool_parity_reachability.rs",
        ] {
            assert!(
                corpus.find(EntryKind::Source, file).is_some(),
                "{file} is not embedded"
            );
        }
        assert_eq!(
            corpus.entries().len().to_string(),
            env!("XAFT_CORPUS_ENTRIES"),
            "the archive holds what build.rs packed"
        );
        assert!(
            corpus.archive_bytes() < 8 * 1024 * 1024,
            "the corpus grew to {} bytes; revisit what build.rs packs",
            corpus.archive_bytes()
        );
        let main = corpus
            .find(EntryKind::Source, "src/main.rs")
            .and_then(|e| corpus.text(e).ok())
            .expect("main.rs inflates");
        let outline = rust_items::outline(&main);
        assert!(
            outline.items.iter().any(|i| i.name == "bootstrap_runtime"),
            "the outliner reads the real source"
        );
    }
}

#[cfg(test)]
mod bootstrap_tests {
    use super::*;

    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Create a fresh, uniquely-named directory under the platform temp dir.
    ///
    /// `tempfile` is not a dependency of the binary crate and adding one for
    /// tests would mean editing a manifest while the agtrs sandbox is engaged,
    /// so this is a few lines of `std` instead. The counter plus the process id
    /// makes collisions between concurrently-running tests impossible.
    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("xaft-boot-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn config_with_data_dir(data_dir: &Path) -> xaft_config::XaftConfig {
        let mut config = xaft_config::XaftConfig::default();
        config.core.data_dir = data_dir.to_path_buf();
        // Keep the tests off the developer's real state directories: memory is
        // exercised explicitly below, always with an explicit location.
        config.memory.project_memory_path = None;
        config
    }

    /// `sessions.db` is what `xaft_session::SessionManager::new` opens, and it
    /// only opens it when the SQLite session store upgrade SUCCEEDED. Its
    /// presence is therefore the observable proof that the upgrade path ran.
    fn session_db_exists(data_dir: &Path) -> bool {
        data_dir.join("sessions.db").exists()
    }

    fn memory_db_exists(mem_dir: &Path) -> bool {
        mem_dir.join("memory.db").exists()
    }

    #[tokio::test]
    async fn a_disabled_memory_system_never_creates_a_memory_database() {
        // Regression guard for the `enabled` gate. If `enabled` were ignored,
        // or read from the wrong field, xaft would silently open (and create)
        // a memory store for users who explicitly turned the feature off —
        // writing a `memory.db` into their data directory.
        let data_dir = temp_dir("mem-off");
        let mem_dir = temp_dir("mem-off-target");
        let mut config = config_with_data_dir(&data_dir);
        config.memory.enabled = false;
        config.memory.backend = "sqlite".into();
        config.memory.project_memory_path = Some(mem_dir.display().to_string());

        bootstrap_runtime(config)
            .await
            .expect("bootstrap must succeed with memory disabled");

        assert!(
            !memory_db_exists(&mem_dir),
            "memory was disabled but a memory database was still written to {}",
            mem_dir.display()
        );
        // The session half must still have been bootstrapped — `enabled`
        // gates memory only, not persistence.
        assert!(session_db_exists(&data_dir));
    }

    #[tokio::test]
    async fn the_sqlite_backend_honours_project_memory_path() {
        // `project_memory_path` is parity row 120: it exists so a user (or a
        // test) can relocate persistent memory off the platform data
        // directory. The only way that can be honoured is if the path really
        // reaches `XaftMemoryManager::new_in_dir`, so assert on the file.
        let data_dir = temp_dir("mem-sqlite");
        let mem_dir = temp_dir("mem-sqlite-target");
        assert!(!memory_db_exists(&mem_dir), "precondition");

        let mut config = config_with_data_dir(&data_dir);
        config.memory.enabled = true;
        config.memory.backend = "sqlite".into();
        config.memory.project_memory_path = Some(mem_dir.display().to_string());

        bootstrap_runtime(config)
            .await
            .expect("bootstrap must succeed");

        assert!(
            memory_db_exists(&mem_dir),
            "sqlite memory was not written to the configured project_memory_path {}",
            mem_dir.display()
        );
    }

    #[tokio::test]
    async fn the_in_memory_backend_writes_nothing_even_with_a_path_configured() {
        // The backend string is matched against `"in_memory"` and every other
        // value falls through to Sqlite. This pins the matching half: a config
        // that asks for in-memory storage AND names a directory must still
        // leave that directory empty, proving the directory setting does not
        // silently override the backend choice.
        let data_dir = temp_dir("mem-ram");
        let mem_dir = temp_dir("mem-ram-target");

        let mut config = config_with_data_dir(&data_dir);
        config.memory.enabled = true;
        config.memory.backend = "in_memory".into();
        config.memory.project_memory_path = Some(mem_dir.display().to_string());

        bootstrap_runtime(config)
            .await
            .expect("bootstrap must succeed");

        assert!(
            !memory_db_exists(&mem_dir),
            "the in_memory backend must not create a database"
        );
    }

    #[tokio::test]
    async fn the_sqlite_session_store_is_opened_under_core_data_dir() {
        // `with_stores` upgrades the runtime from `FsSessionStore` to the
        // SQLite pair. If the upgrade silently failed, xaft would run with
        // filesystem sessions while the user's config says `data_dir` — the
        // exact kind of drift that is invisible until sessions go missing.
        let data_dir = temp_dir("sessions");
        let config = config_with_data_dir(&data_dir);

        bootstrap_runtime(config)
            .await
            .expect("bootstrap must succeed");

        assert!(
            session_db_exists(&data_dir),
            "sessions.db missing in {data_dir:?}"
        );
        assert!(
            data_dir.join("conversations.db").exists(),
            "conversations.db missing in {data_dir:?}"
        );
    }

    #[tokio::test]
    async fn an_unusable_data_dir_is_reported_rather_than_silently_downgraded() {
        // `bootstrap_runtime`'s error is what makes `main` exit 1 with
        // "failed to start runtime". A data_dir that cannot be created (its
        // parent is a regular file) must surface as Err, NOT be swallowed and
        // replaced with a store somewhere else — silently relocating a user's
        // sessions is worse than refusing to start.
        let blocker = temp_dir("blocked");
        let file = blocker.join("i-am-a-file");
        std::fs::write(&file, b"not a directory").unwrap();
        let impossible = file.join("nested");

        let config = config_with_data_dir(&impossible);

        // `expect_err` is unusable here: the Ok type carries
        // `Arc<dyn SessionBackend>`, which is deliberately not `Debug`.
        let err = match bootstrap_runtime(config).await {
            Ok(_) => panic!("an uncreatable data_dir must fail the bootstrap"),
            Err(e) => e,
        };

        // Assert the failure names the data_dir problem rather than being an
        // opaque variant, so a future regression that returns a different
        // error is visible here.
        let rendered = err.to_string();
        assert!(
            rendered.to_lowercase().contains("dir")
                || rendered.to_lowercase().contains("file")
                || rendered.to_lowercase().contains("no such"),
            "unexpected bootstrap error: {rendered}"
        );
    }

    #[tokio::test]
    async fn bootstrap_is_idempotent_against_an_already_populated_data_dir() {
        // Real users run xaft many times against the same data_dir. The
        // second bootstrap must reuse the existing databases rather than
        // failing, wiping them, or creating a second copy.
        let data_dir = temp_dir("reuse");
        let mem_dir = temp_dir("reuse-target");
        let make_config = || {
            let mut c = config_with_data_dir(&data_dir);
            c.memory.enabled = true;
            c.memory.backend = "sqlite".into();
            c.memory.project_memory_path = Some(mem_dir.display().to_string());
            c
        };

        bootstrap_runtime(make_config())
            .await
            .expect("first bootstrap");
        std::fs::write(data_dir.join("marker"), b"kept").unwrap();

        bootstrap_runtime(make_config())
            .await
            .expect("second bootstrap against an existing data_dir");

        assert_eq!(
            std::fs::read(data_dir.join("marker")).unwrap(),
            b"kept",
            "re-bootstrapping disturbed pre-existing state in the data dir"
        );
        assert!(session_db_exists(&data_dir));
        assert!(memory_db_exists(&mem_dir));
    }

    #[tokio::test]
    async fn bootstrap_hands_back_a_usable_session_backend() {
        // `bootstrap_runtime` now returns `(runtime, Option<SessionBackend>)`,
        // and that handle is the ONLY way `xaft session export`/`inspect` can
        // read history. A `Some(...)` that cannot actually read the store the
        // runtime just opened would reintroduce the exact split this replaced:
        // the runtime's store would answer `list`/`show` while `inspect`
        // reported nothing. So the assertion is behavioural — seed a session
        // through the manager, read it back through the returned trait object,
        // and confirm the history survived.
        use xaft_runtime::session::SessionId;

        let data_dir = temp_dir("backend");
        let mut config = config_with_data_dir(&data_dir);
        config.memory.enabled = false;

        let (_runtime, sessions) = bootstrap_runtime(config)
            .await
            .expect("bootstrap must succeed with SQLite available");
        let sessions = sessions.expect(
            "a successful SQLite bootstrap must hand back a session backend; \
             None here would silently disable `xaft session export`/`inspect`",
        );

        // Write through the manager, read through the trait object.
        let manager = SessionManager::new(&data_dir).await.expect("reopen");
        let session = xaft_runtime::session::AgentSession::new(
            "bootstrap check",
            std::env::current_dir().unwrap(),
            "default".into(),
            "claude".into(),
        );
        let id = session.id.clone();
        manager
            .save_with_history(&session, &[xaft_session::Message::user("hello")])
            .await
            .expect("seed");

        let loaded = sessions
            .load_with_history(&id)
            .await
            .expect("backend read")
            .expect("the seeded session must be visible to the injected backend");
        assert_eq!(loaded.session.id, id);
        assert_eq!(loaded.messages.len(), 1, "history must survive the handoff");
    }
}
