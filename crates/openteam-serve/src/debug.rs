//! The debug page at `GET /` — a non-contract surface (ADR 0029).
//!
//! One static HTML file embedded via `include_str!` (the `openteam-mock`
//! embedded-fixture idiom) — zero dependencies, zero framework, zero build
//! step. It renders all three contract surfaces with **no interpretation**:
//! the run list (click to select), the snapshot pretty-printed verbatim, and a
//! bare `EventSource` tail (one `<pre>` line per event) plus a named
//! `run_state` listener. The hard line (ADR 0029): the page **never interprets,
//! folds, or styles domain data — it only tails and dumps**.
//!
//! It ships because it is the only cheap way to exercise browser-only
//! `EventSource` semantics — auto-reconnect, `Last-Event-ID` resume, the 204
//! terminal stop, the id-less `run_state` frame — against the real server.

use axum::response::Html;

const DEBUG_PAGE: &str = include_str!("debug.html");

/// `GET /` — 200 `text/html` (pins §9). Outside `/v1/`, unversioned by design.
pub(crate) async fn page() -> Html<&'static str> {
    Html(DEBUG_PAGE)
}
