//! The knowledge store: run-scoped shared semantic memory (ADR 0014).
//!
//! Written by three paths — auto-ingest of every Message body and every
//! task-completion result, plus the passive `write_knowledge` Note — and read
//! by explicit `search_knowledge` calls and context assembly's auto-retrieval.
//! Entries are never chunked; the store is unbounded with no eviction (entry
//! count and byte size are meta-visible metrics instead of an error path).
//!
//! The [`VectorStore`] seam speaks **text**, never raw vectors, so "the query
//! and every document are embedded by the identical function" is a structural
//! invariant. The in-memory implementation embeds through the internal
//! [`Embedder`] seam (injectable in tests, so cosine ranking is testable
//! without a live mock); the production adapter over the wire
//! `/v1/embeddings` call arrives with the runtime half.

use std::future::Future;
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use openteam_wire::AgentId;
use serde::{Deserialize, Serialize};

use crate::ids::{EventId, KnowledgeEntryId};

/// What produced a knowledge entry — the store's only filter/provenance
/// dimension; there are no freeform tags (ADR 0014). Serializes as the exact
/// PascalCase strings of `knowledge.jsonl` (`"Message"` / `"TaskCompletion"`
/// / `"Note"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KnowledgeKind {
    Message,
    TaskCompletion,
    Note,
}

impl KnowledgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Message => "Message",
            Self::TaskCompletion => "TaskCompletion",
            Self::Note => "Note",
        }
    }
}

impl std::fmt::Display for KnowledgeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One stored item: text plus its embedding and provenance (ADR 0014).
///
/// Serde skips the embedding, so a serialized entry is exactly one
/// `knowledge.jsonl` line — `{ id, text, author, source_event, kind }` —
/// per ADR 0022 (embeddings are deterministically recomputable from text).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeEntry {
    pub id: KnowledgeEntryId,
    pub text: String,
    #[serde(skip)]
    pub embedding: Vec<f32>,
    pub author: AgentId,
    /// The causing event: the `message_sent` / `task_completed` /
    /// `knowledge_written` this entry mirrors (ADR 0014/0022).
    pub source_event: EventId,
    pub kind: KnowledgeKind,
}

/// A search hit: the entry plus its cosine score.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredEntry {
    pub entry: KnowledgeEntry,
    pub score: f32,
}

/// A fault from an embedding backend.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EmbedError {
    #[error("embedding backend error: {0}")]
    Backend(String),
}

/// A fault from a knowledge-store backend.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KnowledgeError {
    #[error(transparent)]
    Embed(#[from] EmbedError),
}

/// The internal embedding seam (ADR 0014) — a plain trait with a native
/// async fn (static dispatch; **not** one of the two `#[async_trait]` dyn
/// seams, ADR 0013), so the store is testable with an injected embedder.
pub trait Embedder: Send + Sync {
    fn embed(&self, text: &str) -> impl Future<Output = Result<Vec<f32>, EmbedError>> + Send;
}

/// The knowledge-store seam (ADR 0014) — one of the two `#[async_trait]`
/// dyn seams (with `LlmClient`, ADR 0013). It speaks **text** only: callers
/// never pre-embed, so query and documents provably share one embedder. The
/// count/bytes/entries accessors feed the run artifacts and the
/// meta-visible store-size metrics.
#[async_trait]
pub trait VectorStore: Send + Sync {
    /// Embed and store one entry, minting its 1-based id.
    async fn insert(
        &self,
        text: &str,
        author: AgentId,
        source_event: EventId,
        kind: KnowledgeKind,
    ) -> Result<KnowledgeEntryId, KnowledgeError>;

    /// Top-k cosine search (CONTEXT.md: Knowledge retrieval).
    async fn search(&self, query: &str, k: usize) -> Result<Vec<ScoredEntry>, KnowledgeError>;

    /// Number of stored entries (meta-visible metric, ADR 0014).
    async fn entry_count(&self) -> usize;

    /// Total text bytes stored (meta-visible metric, ADR 0014).
    async fn byte_size(&self) -> usize;

    /// All entries in id order (for `knowledge.jsonl`, ADR 0022).
    async fn entries(&self) -> Vec<KnowledgeEntry>;
}

/// ADR 0014's pinned deterministic, **seed-independent** lexical projection:
/// lowercase, split on non-alphanumeric, hand-rolled FNV-1a-64 per token into
/// `D` signed buckets (index = `h % D`, sign from the top bit of `h`),
/// L2-normalized; default `D = 256`.
///
/// Identical text always yields the identical vector, and similarity is
/// honest lexical overlap. Doubles as the injectable test embedder.
#[derive(Debug, Clone, Copy)]
pub struct FeatureHashEmbedder {
    dimensions: usize,
}

impl FeatureHashEmbedder {
    /// The ADR 0014 default dimensionality.
    pub const DEFAULT_DIMENSIONS: usize = 256;

    pub fn new() -> Self {
        Self::with_dimensions(Self::DEFAULT_DIMENSIONS)
    }

    /// `dimensions`-overridable per the wire embeddings request (ADR 0014).
    pub fn with_dimensions(dimensions: usize) -> Self {
        Self {
            dimensions: dimensions.max(1),
        }
    }

    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// The pure projection — synchronous by construction.
    pub fn embed_text(&self, text: &str) -> Vec<f32> {
        let mut buckets = vec![0.0f32; self.dimensions];
        for token in text
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
        {
            let h = fnv1a64(token.as_bytes());
            #[allow(clippy::cast_possible_truncation)]
            let index = (h % self.dimensions as u64) as usize;
            let sign = if h >> 63 == 1 { -1.0 } else { 1.0 };
            buckets[index] += sign;
        }
        let norm = buckets.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut buckets {
                *v /= norm;
            }
        }
        buckets
    }
}

impl Default for FeatureHashEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

impl Embedder for FeatureHashEmbedder {
    fn embed(&self, text: &str) -> impl Future<Output = Result<Vec<f32>, EmbedError>> + Send {
        let vector = self.embed_text(text);
        async move { Ok(vector) }
    }
}

/// Hand-rolled FNV-1a-64 — the same hash family ADR 0025 pins for seed
/// derivation; zero new deps.
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Hand-rolled cosine similarity; 0.0 when either vector has zero norm.
/// (L2-normalized inputs make this the plain dot product, but the guard
/// keeps it honest for any injected embedder.)
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|v| v * v).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

/// The v1 in-memory store (ADR 0014): hand-rolled cosine over an internal
/// 1-based id counter, no eviction and no size cap. Interior-mutable so it
/// sits behind the shared `VectorStore` seam; all writes arrive via the
/// runtime's single serial write path.
#[derive(Debug)]
pub struct InMemoryVectorStore<E> {
    embedder: E,
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    entries: Vec<KnowledgeEntry>,
    next_id: u64,
}

impl<E> InMemoryVectorStore<E> {
    pub fn new(embedder: E) -> Self {
        Self {
            embedder,
            inner: Mutex::new(Inner {
                entries: Vec::new(),
                next_id: 1,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for InMemoryVectorStore<FeatureHashEmbedder> {
    fn default() -> Self {
        Self::new(FeatureHashEmbedder::default())
    }
}

#[async_trait]
impl<E: Embedder> VectorStore for InMemoryVectorStore<E> {
    async fn insert(
        &self,
        text: &str,
        author: AgentId,
        source_event: EventId,
        kind: KnowledgeKind,
    ) -> Result<KnowledgeEntryId, KnowledgeError> {
        let embedding = self.embedder.embed(text).await?;
        let mut inner = self.lock();
        let id = KnowledgeEntryId::new(inner.next_id);
        inner.next_id += 1;
        inner.entries.push(KnowledgeEntry {
            id,
            text: text.to_string(),
            embedding,
            author,
            source_event,
            kind,
        });
        Ok(id)
    }

    async fn search(&self, query: &str, k: usize) -> Result<Vec<ScoredEntry>, KnowledgeError> {
        let query_embedding = self.embedder.embed(query).await?;
        let inner = self.lock();
        let mut hits: Vec<ScoredEntry> = inner
            .entries
            .iter()
            .map(|entry| ScoredEntry {
                score: cosine(&query_embedding, &entry.embedding),
                entry: entry.clone(),
            })
            .collect();
        // Descending score; stable, so ties keep id (insertion) order.
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(k);
        Ok(hits)
    }

    async fn entry_count(&self) -> usize {
        self.lock().entries.len()
    }

    async fn byte_size(&self) -> usize {
        self.lock().entries.iter().map(|e| e.text.len()).sum()
    }

    async fn entries(&self) -> Vec<KnowledgeEntry> {
        self.lock().entries.clone()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn feature_hash_embedder_is_deterministic_normalized_d256() {
        let embedder = FeatureHashEmbedder::default();
        let a = embedder.embed_text("Install mise; then `mise run build` / `test`.");
        let b = embedder.embed_text("Install mise; then `mise run build` / `test`.");
        assert_eq!(a, b, "identical text yields the identical vector");
        assert_eq!(a.len(), 256, "default D = 256");
        let norm: f32 = a.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "L2-normalized, got {norm}");

        // Tokenization: lowercase, split on non-alphanumeric.
        assert_eq!(
            embedder.embed_text("Install, MISE!"),
            embedder.embed_text("install mise")
        );

        // dimensions-overridable.
        assert_eq!(
            FeatureHashEmbedder::with_dimensions(64)
                .embed_text("x")
                .len(),
            64
        );

        // Empty text embeds to the zero vector without panicking.
        assert!(embedder.embed_text("").iter().all(|v| *v == 0.0));
    }

    #[tokio::test]
    async fn cosine_top_k_ranks_higher_token_overlap_first() {
        let store = InMemoryVectorStore::default();
        let overlap = store
            .insert(
                "install mise then run build and test",
                AgentId::team(1),
                EventId::new(13),
                KnowledgeKind::Note,
            )
            .await
            .unwrap();
        let unrelated = store
            .insert(
                "the quantum flamingo dances at midnight",
                AgentId::team(2),
                EventId::new(14),
                KnowledgeKind::Note,
            )
            .await
            .unwrap();

        // Internal counter is 1-based and contiguous.
        assert_eq!(overlap, KnowledgeEntryId::new(1));
        assert_eq!(unrelated, KnowledgeEntryId::new(2));

        let hits = store.search("mise build test", 2).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].entry.id, overlap, "higher overlap ranks first");
        assert!(hits[0].score > hits[1].score);

        // k truncates.
        assert_eq!(store.search("mise", 1).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn accessors_expose_count_bytes_and_entries_in_id_order() {
        let store = InMemoryVectorStore::default();
        store
            .insert(
                "abc",
                AgentId::team(1),
                EventId::new(1),
                KnowledgeKind::Message,
            )
            .await
            .unwrap();
        store
            .insert(
                "defgh",
                AgentId::team(2),
                EventId::new(2),
                KnowledgeKind::TaskCompletion,
            )
            .await
            .unwrap();

        assert_eq!(store.entry_count().await, 2);
        assert_eq!(store.byte_size().await, 8);
        let entries = store.entries().await;
        assert_eq!(entries[0].id, KnowledgeEntryId::new(1));
        assert_eq!(entries[1].id, KnowledgeEntryId::new(2));
        assert_eq!(entries[1].kind, KnowledgeKind::TaskCompletion);
    }

    /// The `Embedder` seam is injectable: a stub embedder drives ranking
    /// without any real embedding function (ADR 0014/0025).
    #[tokio::test]
    async fn embedder_seam_is_injectable() {
        struct StubEmbedder;
        impl Embedder for StubEmbedder {
            fn embed(
                &self,
                text: &str,
            ) -> impl Future<Output = Result<Vec<f32>, EmbedError>> + Send {
                let vector = if text.contains("alpha") {
                    vec![1.0, 0.0]
                } else {
                    vec![0.0, 1.0]
                };
                async move { Ok(vector) }
            }
        }

        let store = InMemoryVectorStore::new(StubEmbedder);
        let alpha = store
            .insert(
                "alpha",
                AgentId::team(1),
                EventId::new(1),
                KnowledgeKind::Note,
            )
            .await
            .unwrap();
        store
            .insert(
                "beta",
                AgentId::team(1),
                EventId::new(2),
                KnowledgeKind::Note,
            )
            .await
            .unwrap();

        let hits = store.search("alpha query", 2).await.unwrap();
        assert_eq!(hits[0].entry.id, alpha);
        assert!((hits[0].score - 1.0).abs() < 1e-6);
        assert!((hits[1].score).abs() < 1e-6);
    }

    #[test]
    fn knowledge_entry_serializes_as_a_knowledge_jsonl_line() {
        let entry = KnowledgeEntry {
            id: KnowledgeEntryId::new(2),
            text: "Setup: install mise.".into(),
            embedding: vec![1.0; 256],
            author: AgentId::team(1),
            source_event: EventId::new(13),
            kind: KnowledgeKind::Note,
        };
        // Embeddings are omitted — recomputable from text (ADR 0022).
        assert_eq!(
            serde_json::to_value(&entry).unwrap(),
            json!({
                "id": 2,
                "kind": "Note",
                "author": "agent-1",
                "source_event": 13,
                "text": "Setup: install mise.",
            })
        );
        // And the transcript's knowledge.jsonl lines deserialize back.
        let line = r#"{"id":1,"kind":"Message","author":"orchestrator","source_event":10,"text":"Prioritize the setup section; the guide leads with it."}"#;
        let back: KnowledgeEntry = serde_json::from_str(line).unwrap();
        assert_eq!(back.kind, KnowledgeKind::Message);
        assert!(back.embedding.is_empty());
    }
}
