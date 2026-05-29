use std::sync::Arc;

use async_trait::async_trait;

use crate::types::{GcStats, MemoryEntry, MemoryId, MemoryScope, SearchResult};
use crate::{MemoryError, MemoryQuery};

// ---------------------------------------------------------------------------
// MemoryOperation
// ---------------------------------------------------------------------------

/// Identifies which operation a hook context was created for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryOperation {
    Store,
    Retrieve,
    Search,
    Update,
    Delete,
    Gc,
    Clear,
}

// ---------------------------------------------------------------------------
// MemoryHookContext
// ---------------------------------------------------------------------------

/// Contextual information passed to every hook invocation.
#[derive(Clone, Debug)]
pub struct MemoryHookContext {
    /// Name / identifier of the backend that is executing the operation.
    pub store_backend: String,

    /// The entry being operated on, when applicable.
    pub entry_id: Option<MemoryId>,

    /// The scope the operation targets, when applicable.
    pub scope: Option<MemoryScope>,

    /// Which operation triggered this hook.
    pub operation: MemoryOperation,
}

// ---------------------------------------------------------------------------
// MemoryHookDecision
// ---------------------------------------------------------------------------

/// Returned by `before_store` to control what happens next.
#[derive(Debug)]
pub enum MemoryHookDecision {
    /// Allow the operation to proceed normally.
    Allow,

    /// Block the operation. The `String` is a human-readable reason.
    Reject(String),

    /// Replace the entry that is about to be stored with the supplied value.
    /// Only meaningful for `Store` operations; treated as `Allow` elsewhere.
    Transform(MemoryEntry),
}

// ---------------------------------------------------------------------------
// MemoryHook trait
// ---------------------------------------------------------------------------

/// A hook that can observe and intercept memory operations.
///
/// All methods have default no-op implementations so that implementors only
/// need to override the hooks they care about.
#[async_trait]
pub trait MemoryHook: Send + Sync {
    /// Called before a `Store` operation.
    ///
    /// Returns a [`MemoryHookDecision`] that can allow, reject, or transform
    /// the entry that is about to be written.
    async fn before_store(
        &self,
        _ctx: &MemoryHookContext,
        _entry: &MemoryEntry,
    ) -> Result<MemoryHookDecision, MemoryError> {
        Ok(MemoryHookDecision::Allow)
    }

    /// Called after a `Store` operation has completed successfully.
    async fn after_store(
        &self,
        _ctx: &MemoryHookContext,
        _id: &MemoryId,
    ) -> Result<(), MemoryError> {
        Ok(())
    }

    /// Called before a `Search` operation.
    async fn before_search(
        &self,
        _ctx: &MemoryHookContext,
        _query: &MemoryQuery,
    ) -> Result<(), MemoryError> {
        Ok(())
    }

    /// Called after a `Search` operation has returned results.
    async fn after_search(
        &self,
        _ctx: &MemoryHookContext,
        _results: &[SearchResult],
    ) -> Result<(), MemoryError> {
        Ok(())
    }

    /// Called when a garbage-collection cycle completes.
    async fn on_gc(
        &self,
        _ctx: &MemoryHookContext,
        _stats: &GcStats,
    ) -> Result<(), MemoryError> {
        Ok(())
    }

    /// Called when an entry is evicted from the store (e.g. capacity limit).
    async fn on_evicted(
        &self,
        _ctx: &MemoryHookContext,
        _entry: &MemoryEntry,
    ) -> Result<(), MemoryError> {
        Ok(())
    }

    /// Called when an entry has passed its `expires_at` timestamp and is
    /// removed from the store.
    async fn on_expired(
        &self,
        _ctx: &MemoryHookContext,
        _entry: &MemoryEntry,
    ) -> Result<(), MemoryError> {
        Ok(())
    }

    /// Called when a write conflict is detected (e.g. concurrent update).
    async fn on_conflict(
        &self,
        _ctx: &MemoryHookContext,
        _reason: &str,
    ) -> Result<(), MemoryError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// HookChain
// ---------------------------------------------------------------------------

/// An ordered chain of [`MemoryHook`] implementations that are invoked in
/// registration order for every memory event.
#[derive(Default)]
pub struct HookChain {
    hooks: Vec<Arc<dyn MemoryHook>>,
}

impl HookChain {
    /// Create an empty chain.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a hook to the end of the chain.
    pub fn add(&mut self, hook: Arc<dyn MemoryHook>) {
        self.hooks.push(hook);
    }

    // -----------------------------------------------------------------------
    // before_store — first Reject wins; first Transform wins over Allow
    // -----------------------------------------------------------------------

    /// Run all `before_store` hooks.
    ///
    /// Evaluation rules (in order of registered hooks):
    ///
    /// * The first [`MemoryHookDecision::Reject`] encountered short-circuits
    ///   the chain and its reason is returned.
    /// * The first [`MemoryHookDecision::Transform`] encountered replaces the
    ///   entry for subsequent hooks *and* is returned as the final decision
    ///   (unless a later hook rejects it).
    /// * If all hooks return [`MemoryHookDecision::Allow`], `Allow` is
    ///   returned.
    pub async fn run_before_store(
        &self,
        ctx: &MemoryHookContext,
        entry: &MemoryEntry,
    ) -> Result<MemoryHookDecision, MemoryError> {
        let mut current_entry: Option<MemoryEntry> = None;

        for hook in &self.hooks {
            let candidate = current_entry.as_ref().unwrap_or(entry);
            let decision = hook.before_store(ctx, candidate).await?;
            match decision {
                MemoryHookDecision::Reject(reason) => {
                    return Ok(MemoryHookDecision::Reject(reason));
                }
                MemoryHookDecision::Transform(new_entry) => {
                    current_entry = Some(new_entry);
                }
                MemoryHookDecision::Allow => {}
            }
        }

        Ok(match current_entry {
            Some(e) => MemoryHookDecision::Transform(e),
            None => MemoryHookDecision::Allow,
        })
    }

    // -----------------------------------------------------------------------
    // after_store
    // -----------------------------------------------------------------------

    /// Run all `after_store` hooks sequentially.
    pub async fn run_after_store(
        &self,
        ctx: &MemoryHookContext,
        id: &MemoryId,
    ) -> Result<(), MemoryError> {
        for hook in &self.hooks {
            hook.after_store(ctx, id).await?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // before_search
    // -----------------------------------------------------------------------

    /// Run all `before_search` hooks sequentially.
    pub async fn run_before_search(
        &self,
        ctx: &MemoryHookContext,
        query: &MemoryQuery,
    ) -> Result<(), MemoryError> {
        for hook in &self.hooks {
            hook.before_search(ctx, query).await?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // after_search
    // -----------------------------------------------------------------------

    /// Run all `after_search` hooks sequentially.
    pub async fn run_after_search(
        &self,
        ctx: &MemoryHookContext,
        results: &[SearchResult],
    ) -> Result<(), MemoryError> {
        for hook in &self.hooks {
            hook.after_search(ctx, results).await?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // on_gc
    // -----------------------------------------------------------------------

    /// Run all `on_gc` hooks sequentially.
    pub async fn run_on_gc(
        &self,
        ctx: &MemoryHookContext,
        stats: &GcStats,
    ) -> Result<(), MemoryError> {
        for hook in &self.hooks {
            hook.on_gc(ctx, stats).await?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // on_evicted
    // -----------------------------------------------------------------------

    /// Run all `on_evicted` hooks sequentially.
    pub async fn run_on_evicted(
        &self,
        ctx: &MemoryHookContext,
        entry: &MemoryEntry,
    ) -> Result<(), MemoryError> {
        for hook in &self.hooks {
            hook.on_evicted(ctx, entry).await?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // on_expired
    // -----------------------------------------------------------------------

    /// Run all `on_expired` hooks sequentially.
    pub async fn run_on_expired(
        &self,
        ctx: &MemoryHookContext,
        entry: &MemoryEntry,
    ) -> Result<(), MemoryError> {
        for hook in &self.hooks {
            hook.on_expired(ctx, entry).await?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // on_conflict
    // -----------------------------------------------------------------------

    /// Run all `on_conflict` hooks sequentially.
    pub async fn run_on_conflict(
        &self,
        ctx: &MemoryHookContext,
        reason: &str,
    ) -> Result<(), MemoryError> {
        for hook in &self.hooks {
            hook.on_conflict(ctx, reason).await?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    // ------------------------------------------------------------------
    // Minimal stub types so the tests compile without the rest of the crate.
    // In a real build these come from `crate::types`.
    // ------------------------------------------------------------------

    struct AllowHook;

    #[async_trait]
    impl MemoryHook for AllowHook {}

    struct RejectHook(String);

    #[async_trait]
    impl MemoryHook for RejectHook {
        async fn before_store(
            &self,
            _ctx: &MemoryHookContext,
            _entry: &MemoryEntry,
        ) -> Result<MemoryHookDecision, MemoryError> {
            Ok(MemoryHookDecision::Reject(self.0.clone()))
        }
    }

    struct CountingHook(Arc<AtomicUsize>);

    #[async_trait]
    impl MemoryHook for CountingHook {
        async fn after_store(
            &self,
            _ctx: &MemoryHookContext,
            _id: &MemoryId,
        ) -> Result<(), MemoryError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn ctx() -> MemoryHookContext {
        MemoryHookContext {
            store_backend: "test".into(),
            entry_id: None,
            scope: None,
            operation: MemoryOperation::Store,
        }
    }

    #[tokio::test]
    async fn empty_chain_allows() {
        let chain = HookChain::new();
        let entry = MemoryEntry::default();
        let decision = chain.run_before_store(&ctx(), &entry).await.unwrap();
        assert!(matches!(decision, MemoryHookDecision::Allow));
    }

    #[tokio::test]
    async fn reject_short_circuits() {
        let mut chain = HookChain::new();
        chain.add(Arc::new(AllowHook));
        chain.add(Arc::new(RejectHook("no".into())));
        chain.add(Arc::new(AllowHook));

        let entry = MemoryEntry::default();
        let decision = chain.run_before_store(&ctx(), &entry).await.unwrap();
        assert!(matches!(decision, MemoryHookDecision::Reject(_)));
    }

    #[tokio::test]
    async fn after_store_all_hooks_called() {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut chain = HookChain::new();
        chain.add(Arc::new(CountingHook(Arc::clone(&counter))));
        chain.add(Arc::new(CountingHook(Arc::clone(&counter))));

        let id = MemoryId::default();
        chain.run_after_store(&ctx(), &id).await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }
}
