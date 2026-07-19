//! The GUI-facing wire types (ADR 0028), defined beside their only producer
//! (ADR 0030).
//!
//! The **canonical** contract is the pinned JSON shapes in the ADRs +
//! `docs/implementation-pins.md` §9, not these Rust types — so the tests assert
//! serialized values, not type identity. `openteam-wire` stays untouched (its
//! ADR 0013 charter is the LLM contract shared with the mock); the four-state
//! agent vocabulary is defined here, and the internal `MeterState` is left
//! alone.

use jiff::Timestamp;
use openteam_core::{BoardSnapshot, RunSummary, TaskId};
use openteam_wire::{AgentId, SpecialtySlug};
use serde::Serialize;

/// The four-state agent wire vocabulary (ADR 0028) — the one genuinely new
/// surface. Serialized lowercase, externally tagged: `"idle"` /
/// `{"working":{"task":3}}` / `"asleep"` / `"parked"` (pins §9). Distinct from
/// the internal `MeterState`, which collapses `parked` into `asleep`; a
/// dashboard must surface the K=3-malformed park, so the fold keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentWireState {
    Idle,
    Working { task: TaskId },
    Asleep,
    Parked,
}

/// One team agent's snapshot entry (pins §9): `{ handle, specialty, state }`,
/// emitted in handle order `agent-1..N`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AgentEntry {
    pub(crate) handle: AgentId,
    pub(crate) specialty: SpecialtySlug,
    pub(crate) state: AgentWireState,
}

/// The `finished` block of a run-list entry (present only for finished runs,
/// pins §9): the `run_finished.reason` representation verbatim plus its exit
/// code.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FinishedBlock {
    pub(crate) reason: serde_json::Value,
    pub(crate) exit_code: u8,
}

/// One entry of `GET /v1/runs` — cheap fields only, no counts (pins §9).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RunListEntry {
    pub(crate) run_id: openteam_core::RunId,
    pub(crate) state: &'static str,
    pub(crate) goal: String,
    pub(crate) seed: u64,
    /// Informational RFC3339 from event 0's `at`.
    pub(crate) started_at: Timestamp,
    pub(crate) last_event_id: u64,
    /// Present **only** when `state == "finished"` (pins §9).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) finished: Option<FinishedBlock>,
}

/// The `GET /v1/runs/{run_id}/snapshot` body — top-level keys exactly
/// `as_of`, `run`, `board`, `agents`, `metrics` (pins §9).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SnapshotResponse {
    /// The `EventId` the fold has consumed through (doubles as `?from=`).
    pub(crate) as_of: u64,
    /// The `run_started.data` fields verbatim plus `state` (ADR 0028).
    pub(crate) run: serde_json::Value,
    /// The `board.json` object shape verbatim (ADR 0022).
    pub(crate) board: BoardSnapshot,
    /// Per-team-agent four-state entries, in handle order.
    pub(crate) agents: Vec<AgentEntry>,
    /// `RunSummary` serialized — ADR 0020's fourth view.
    pub(crate) metrics: RunSummary,
}
