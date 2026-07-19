//! `openteam-serve` — the read-only stream server for `.openteam/runs`
//! (ADRs 0027–0030).
//!
//! A **sidecar log-tailer**: it discovers run directories under one filesystem
//! root, classifies each run `finished | live | aborted` (the `run_finished`
//! bookend × `run.lock` flock trichotomy, ADR 0027), folds `events.jsonl` into
//! a server-side projection, and serves three contract endpoints under `/v1/`
//! plus a debug page at `/` (ADR 0028/0029). It is a **pure reader** — a
//! deterministic function of run-dir bytes, no RNG — so "same run-dir bytes →
//! same responses" is its whole determinism statement (ADR 0030).
//!
//! - [`ServeConfig`] — the constructor-injectable timing knobs (never on the
//!   CLI); the binary wires the pinned defaults, tests inject fast values.
//! - [`build_router`] / [`serve`] — the axum app, mounted identically by the
//!   `openteam serve` CLI and the crate's integration tests (ADR 0019/0030).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod config;
mod debug;
mod discovery;
mod fold;
mod routes;
mod schema;
mod server;
mod stream;
mod tail;

pub use config::ServeConfig;
pub use server::{ShutdownHandle, build_router, serve};

use std::path::Path;

/// Fold a run directory's `events.jsonl` into its board snapshot — the public
/// entry point for the finished-run **folded snapshot ≡ board.json** invariant
/// (ADR 0028/0030), callable from e2e tests without spawning the server. It is
/// the same fold the `/v1/.../snapshot` endpoint runs, over a run dir the
/// harness just produced. Returns `None` if the directory has no readable
/// `run_started` header (not a run).
pub fn folded_board(dir: &Path) -> Option<openteam_core::BoardSnapshot> {
    let header = discovery::run_header(dir)?;
    let events = discovery::read_events(dir);
    Some(fold::board_snapshot(
        header.run_id,
        &header.goal,
        header.seed,
        &events,
    ))
}
