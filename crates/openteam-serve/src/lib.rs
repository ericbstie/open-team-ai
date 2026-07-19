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
// `discovery` is wired into the router in step 5, `tail` into the SSE stream in
// step 6; the `allow` is removed as each module's surface is consumed.
#[allow(dead_code)]
mod discovery;
mod server;
#[allow(dead_code)]
mod tail;

pub use config::ServeConfig;
pub use server::{ShutdownHandle, build_router, serve};
