//! The `/v1/` contract endpoints: the run list and the folded snapshot
//! (ADR 0028/0029). The SSE stream endpoint is mounted alongside these in
//! step 6.
//!
//! Both handlers are pure reads over run-dir bytes (ADR 0030): discover /
//! classify / fold, then serialize. The snapshot's fold never reads
//! `board.json` — it is the one uniform path for finished, live, and aborted
//! runs alike.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;

use crate::discovery::{self, RunState};
use crate::fold;
use crate::schema::{FinishedBlock, RunListEntry, SnapshotResponse};
use crate::server::AppState;

/// `GET /v1/runs` — the run list: a JSON array sorted by `run_id` ascending
/// (UUIDv7 ⇒ chronological), cheap fields only, no counts (ADR 0028, pins §9).
pub(crate) async fn list_runs(State(app): State<AppState>) -> Json<Vec<RunListEntry>> {
    let entries = discovery::discover(&app.root)
        .into_iter()
        .map(|run| list_entry(&run.path, &run.header))
        .collect();
    Json(entries)
}

/// Build one run-list entry: classify, then read the last complete event for
/// `last_event_id` and (only when finished) the `finished` block.
fn list_entry(dir: &std::path::Path, header: &discovery::RunHeader) -> RunListEntry {
    let state = discovery::classify(dir);
    let last = discovery::last_event(dir);
    let last_event_id = last
        .as_ref()
        .and_then(|event| event["id"].as_u64())
        .unwrap_or(0);
    // The `finished` block is present **only** for finished runs (pins §9);
    // its `reason` is the `run_finished.reason` representation verbatim.
    let finished = (state == RunState::Finished)
        .then(|| last.as_ref().map(finished_block))
        .flatten();
    RunListEntry {
        run_id: header.run_id,
        state: state.as_wire_str(),
        goal: header.goal.clone(),
        seed: header.seed,
        started_at: header.started_at,
        last_event_id,
        finished,
    }
}

/// Extract the `finished` block from a `run_finished` event value.
fn finished_block(event: &serde_json::Value) -> FinishedBlock {
    FinishedBlock {
        reason: event["data"]["reason"].clone(),
        exit_code: event["data"]["exit_code"].as_u64().unwrap_or(0) as u8,
    }
}

/// `GET /v1/runs/{run_id}/snapshot` — the server-folded projection
/// `{ as_of, run, board, agents, metrics }` (ADR 0028). **404** on an unknown
/// `run_id` (pins §9).
pub(crate) async fn snapshot(
    State(app): State<AppState>,
    Path(run_id): Path<String>,
) -> Result<Json<SnapshotResponse>, StatusCode> {
    let run = discovery::find_run(&app.root, &run_id).ok_or(StatusCode::NOT_FOUND)?;
    let state = discovery::classify(&run.path);
    let events = discovery::read_events(&run.path);
    Ok(Json(fold::snapshot(&run.header, state, &events)))
}
