//! `openteam-core` — the domain and runtime crate of the `openteam` harness.
//!
//! This is the **domain half**: run-scoped ids (ADR 0011), the injectable
//! `Clock` seam, the ADR 0022 event schema, the task board and teams
//! (ADR 0009/0010), store-first messaging and mailboxes (ADR 0011), the
//! knowledge store and its `Embedder`/`VectorStore` seams (ADR 0014),
//! two-tier directives (ADR 0005/0020), and the runtime-owned `Metrics`
//! fold with its three projections (ADR 0020). The runtime half (turn loop,
//! scheduler, context assembly, tool registry, `LlmClient`) builds on these
//! types.
//!
//! Vocabulary follows CONTEXT.md; each decision cites the ADR that pins it.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod artifacts;
mod board;
mod clock;
mod context;
mod directive;
mod event;
mod ids;
mod knowledge;
mod llm;
mod message;
mod metrics;
mod runtime;
mod tools;

pub use artifacts::BoardSnapshot;
pub use board::{Board, BoardRejection, MembershipDelta, Task, TaskState, Team};
pub use clock::{Clock, FrozenClock, SystemClock};
pub use context::{
    AssembleView, AssembledPrompt, ContextPolicy, DropRule, SectionKind, SectionSpec,
    SpecialtyProfile, assemble, claimed_task_line, recent_event_line, retrieval_line, skeleton,
    window_line,
};
pub use directive::{Directive, DirectiveKind, DirectiveState, DirectiveTier};
pub use event::{
    CapKind, DegradedSection, Event, EventKind, EventSource, RestoredState, RunCaps,
    RunFinishReason, TurnOutcome, TurnUsage,
};
pub use ids::{
    DirectiveId, EventId, KnowledgeEntryId, MessageId, RunId, TaskId, TeamId, TeamIdError,
};
pub use knowledge::{
    EmbedError, Embedder, FeatureHashEmbedder, InMemoryVectorStore, KnowledgeEntry, KnowledgeError,
    KnowledgeKind, ScoredEntry, VectorStore,
};
pub use llm::{AgentChannel, LlmClient, LlmConfig, LlmError, ReqwestLlmClient, WireEmbedder};
pub use message::{Address, Mailboxes, Message};
pub use metrics::{Metrics, RunSummary};
pub use runtime::{AgentState, RunConfig, RunError, RunOutcome, run};
pub use tools::{ToolOutcome, ToolRegistry};
