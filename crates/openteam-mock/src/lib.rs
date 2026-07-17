//! `openteam-mock` — the deterministic OpenAI-schema mock (ADR 0019/0021/0023).
//!
//! The mock plays the model for every agent: served over real loopback HTTP,
//! **stateless per request** — every response is a pure function of the
//! request body plus its identity channels (`user`, `X-OpenTeam-Call-Seq`,
//! `X-OpenTeam-Seed`). Its only internal dependency is `openteam-wire`
//! (ADR 0013): the behavior model learns the coordination verbs solely from
//! each request's `tools` array, so the mock provably serves any
//! OpenAI-schema client.
//!
//! - [`BehaviorModel`] / [`ChatDecision`] — the synchronous behavior seam;
//!   the server owns the envelope, so an invalid response is unrepresentable.
//! - [`BuiltinArc`] — the default adapter: the bounded decompose → work →
//!   converge behavior arc, re-derived from the rendered world each
//!   completion (ADR 0021).
//! - [`ScenarioPlayer`] / [`Scenario`] — the second adapter: call-seq-indexed
//!   scripted responses with arc fallthrough (ADR 0023).
//! - [`embed_text`] — the fixed, seed-independent embedding function
//!   (ADR 0014), bypassing the seam.
//! - [`build_router`] / [`serve`] / [`AppState`] — the axum app, mounted
//!   identically embedded, standalone, and under contract tests.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod arc;
mod behavior;
mod clock;
mod embeddings;
mod parse;
mod scenario;
mod seed;
mod server;

#[cfg(test)]
mod fixtures;

pub use arc::BuiltinArc;
pub use behavior::{BehaviorModel, ChatDecision};
pub use clock::{FrozenClock, MockClock, SystemClock};
pub use embeddings::{DEFAULT_DIMENSIONS, embed_text};
pub use parse::{
    AgentStateLine, BoardDigest, ClaimedTask, DirectiveArgs, DirectiveLine, DirectiveOutcomes,
    FreshMessage, RecentActivity, RenderedWorld, TaskLine, TaskState, ToolIndex, ToolStatus,
    TurnLocal, UtilizationLine,
};
pub use scenario::{
    Call, Response, SCENARIO_VERSION, Say, Scenario, ScenarioError, ScenarioPlayer, Script,
    Selector,
};
pub use seed::{derive_rng, fnv1a64};
pub use server::{AppState, MockError, ShutdownHandle, build_router, serve};
