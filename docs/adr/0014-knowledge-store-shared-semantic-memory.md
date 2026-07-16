# The knowledge store is shared semantic memory, not a message log

The run-scoped vector store is written by three paths — auto-ingest of every
Message body and every task-completion `result` (store-first, on the serial
write path), plus a first-class **passive** `write_knowledge { text }` verb that
records a `Note` with no mailbox delivery. The passive write is load-bearing,
not a convenience: the headline requirement is one shared knowledge base every
agent reads and writes, and a broadcast Message interrupts every mailbox (the
message-flood failure mode from the prior-art research) whereas a Note is
deposited silently and surfaces only when a later agent's search matches it —
that asymmetry (push vs. passive contribution) is the point, and without the
write verb the store is merely a searchable message log. Entries are **never
chunked** (bodies are turn-sized, bounded by run caps); each carries
`{ id, text, embedding, author, source_event, kind }` where
`KnowledgeKind ∈ { Message, TaskCompletion, Note }`. `MessageId`,
`KnowledgeEntryId`, and `EventId` are all monotonic counters off the single
serial write path, so the mutual reference — `Message.knowledge_ref` ↔
`KnowledgeEntry.source_event` ↔ the `message_sent` / `task_done` /
`knowledge_written` event — is coherent only because all three ids are allocated
up front within one write-path step, then cross-referenced, then committed; there
is no real circularity to trip on. The `VectorStore` trait speaks **text**, never
raw vectors (`insert(text, author, source_event, kind)`, `search(query)`), so
"the query and every document are embedded by the identical function" is a
*structural* invariant rather than a call-site convention, and a real
text-accepting backend (pgvector, a hosted index) drops in behind the same seam;
the in-memory hand-rolled-cosine impl embeds through an **internal** `Embedder`
seam over the wire `/v1/embeddings` call (injectable in tests so cosine ranking
is testable without a live mock). The store is **unbounded with no eviction and
no size cap**: entry count is bounded by `--max-llm-calls` / `--max-ticks`
exactly as mailboxes are (ADR 0011), eviction would need a drop policy and could
silently lose retrievable run history, so entry-count and byte-size are
meta-visible metrics — a runaway store is a process signal, not an error path.
Mock embeddings are a **deterministic, seed-independent** lexical projection
(signed feature-hashing: lowercase, split on non-alphanumeric, FNV-1a per token
into `D` signed buckets, L2-normalize; default `D = 256`, `dimensions`-overridable,
base64 f32-LE on the wire): identical text always yields the identical vector,
and similarity is honest lexical overlap — the most a content-blind mock can
plausibly promise. Rejected: caller-pre-embeds vectors (leaks the
identical-embedder invariant to every call site); chunking (no payoff at
turn-sized scale); eviction / size caps (unbounded is safe here and drops
nothing retrievable); seed-dependent embeddings (would make cosine meaningless
across runs and misrepresent what an embedding endpoint is).
