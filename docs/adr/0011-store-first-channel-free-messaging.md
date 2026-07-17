# Messages are store-first; the bus has no channels

A message send is accepted on the run's single serialized write path: validate the
address, assign a monotonic `MessageId`, ingest the body into the knowledge store,
append the `message_sent` event, push the id onto each recipient's mailbox — a plain
`VecDeque<MessageId>` in shared state — and nudge the scheduler. No tokio channel
carries message data: `broadcast` drops on lag, silently violating "always ingested,
never lost" (see docs/research/rust-crate-inventory.md), and per-agent `mpsc` would
only duplicate the store, which is the source of truth. Store-first makes losslessness
structural rather than aspirational. Delivery is between-turn injection: context
assembly drains the mailbox oldest-first under a token budget with carryover — never
dropped, never summarized away. "Realtime" is satisfied at turn granularity, the
finest that exists for an LLM agent; the accept-time scheduler nudge is what makes it
feel realtime, so do not bolt channels back on chasing a phantom sub-turn requirement.
Guarantees: run-wide total order at accept; every recipient sees `MessageId` order
(per-sender FIFO plus cross-recipient agreement); which racing send gets the lower id
is nondeterministic, within the charted determinism bar. Mailboxes are unbounded — a
message costs its sender a tool call, so volume is bounded by the run caps — with
mailbox depth and oldest-pending age as meta-visible metrics instead of a backpressure
error path. Broadcasts exclude meta-agents (they observe via events, they don't
participate); directives are not Messages and ride their own typed queue (ADR 0005).

**Amended by the #22 dry-run gate (2026-07-17).** `MessageId`, `EventId`, and
`KnowledgeEntryId` — with `TaskId` (ADR 0023) — are **four independent monotonic
counters**, each advanced on this one serial write path, **not a single shared id
space**. "Allocated up front within one write-path step" (ADR 0014) means allocated
atomically in one step, which is what preserves the run-wide total order and the coherent
`Message.knowledge_ref` ↔ `KnowledgeEntry.source_event` ↔ emitting-event cross-reference.
Each counter is **contiguous**: `EventId` is 0-based (`run_started` = event 0);
`MessageId` / `KnowledgeEntryId` / `TaskId` are 1-based. Contiguous per-type ids are what
make #23/#20's "the Nth event is X" fixtures and assertions tractable (a single shared
counter would gap the EventIds). Validated in docs/prototypes/dry-run-transcript.md.
