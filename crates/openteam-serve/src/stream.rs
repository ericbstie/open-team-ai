//! The SSE stream endpoint `GET /v1/runs/{run_id}/events` (ADR 0028/0029).
//!
//! One SSE event per `events.jsonl` line: `id:` = the decimal `EventId`,
//! `data:` = the **verbatim line bytes** (byte-golden, ADR 0030), no `event:`
//! field. Resume is exact arithmetic on the contiguous `EventId`
//! (`Last-Event-ID: n` / `?from=n` → replay from `n + 1`; absent → from 0; a
//! non-`u64` id → **400**). A caught-up connect to any **terminal** run gets
//! **204**; a live → aborted transition sends the id-less
//! `event: run_state` / `data: {"state":"aborted"}` control frame, then EOF.
//!
//! Live tailing rides a per-run bounded `tokio::sync::broadcast`: a connection
//! **subscribes first**, then catches up from the file, then tails the
//! broadcast — deduping the overlap by `EventId` and **disconnecting on lag**
//! (continuing past a gap is forbidden by `EventId` contiguity; the client
//! reconnects and replays losslessly from the durable file).

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use openteam_core::RunId;
use tokio::sync::{broadcast, mpsc};

use crate::config::ServeConfig;
use crate::discovery::{self, RunState};
use crate::server::AppState;
use crate::tail::Tailer;

/// The mpsc buffer between the per-connection producer task and the HTTP body.
/// Small on purpose: a slow client backpressures the task, which then stops
/// draining the broadcast and lags — exactly the disconnect-on-lag path.
const STREAM_BUFFER: usize = 32;

/// The id-less abort control frame's payload (pins §9).
const ABORT_DATA: &str = r#"{"state":"aborted"}"#;

/// One message on a live run's broadcast: a freshly committed event line, or
/// the terminal live → aborted transition.
#[derive(Debug, Clone)]
enum LiveMsg {
    Event { id: u64, line: Arc<str> },
    Aborted,
}

/// The per-live-run broadcast registry (ADR 0028). Each live run being tailed
/// has one bounded `broadcast::Sender` fed by a single poll-tailer task;
/// connections subscribe to it. Finished/aborted runs are served entirely from
/// the file, so they never appear here.
#[derive(Default)]
pub(crate) struct LiveRegistry {
    runs: Mutex<HashMap<RunId, broadcast::Sender<LiveMsg>>>,
}

impl LiveRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<RunId, broadcast::Sender<LiveMsg>>> {
        self.runs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Subscribe to a live run's broadcast, starting its poll-tailer if this is
    /// the first subscriber. The tailer is positioned at the current EOF
    /// boundary **synchronously here**, before the caller reads the file for
    /// catch-up — so no event slips between the snapshot and the tail.
    fn subscribe(
        self: &Arc<Self>,
        run_id: RunId,
        dir: PathBuf,
        config: &ServeConfig,
    ) -> broadcast::Receiver<LiveMsg> {
        let mut runs = self.lock();
        if let Some(sender) = runs.get(&run_id) {
            return sender.subscribe();
        }
        let (sender, receiver) = broadcast::channel(config.broadcast_capacity);
        runs.insert(run_id, sender.clone());
        let mut tailer = Tailer::new(dir.join("events.jsonl"));
        tailer.skip_to_end();
        tokio::spawn(tail_task(
            run_id,
            tailer,
            dir,
            sender,
            Arc::clone(self),
            config.poll_interval,
        ));
        receiver
    }
}

/// The per-run poll-tailer: broadcast each freshly committed line, then watch
/// for the terminal transition (finished → stop, since `run_finished` was
/// delivered in-band; aborted → broadcast `Aborted`, then stop). Stops early
/// once no subscribers remain, cleaning itself out of the registry.
async fn tail_task(
    run_id: RunId,
    mut tailer: Tailer,
    dir: PathBuf,
    sender: broadcast::Sender<LiveMsg>,
    registry: Arc<LiveRegistry>,
    poll_interval: Duration,
) {
    loop {
        for line in tailer.poll().unwrap_or_default() {
            if let Ok(text) = String::from_utf8(line)
                && let Some(id) = discovery::event_id(&text)
            {
                let _ = sender.send(LiveMsg::Event {
                    id,
                    line: Arc::from(text.as_str()),
                });
            }
        }
        match discovery::classify(&dir) {
            RunState::Finished => break,
            RunState::Aborted => {
                let _ = sender.send(LiveMsg::Aborted);
                break;
            }
            RunState::Live => {}
        }
        // No subscribers → stop (re-checked under the lock to avoid racing a
        // concurrent subscribe). A later connect restarts a fresh tailer.
        if sender.receiver_count() == 0 {
            let mut runs = registry.lock();
            if sender.receiver_count() == 0 {
                runs.remove(&run_id);
                return;
            }
        }
        tokio::time::sleep(poll_interval).await;
    }
    registry.lock().remove(&run_id);
}

/// The resume query fallback: `?from=<id>` (the reload/curl path for the
/// per-`EventSource`-object `Last-Event-ID`, ADR 0028).
#[derive(serde::Deserialize)]
pub(crate) struct FromQuery {
    from: Option<String>,
}

/// `GET /v1/runs/{run_id}/events` — the per-run SSE stream (ADR 0028).
pub(crate) async fn events(
    State(app): State<AppState>,
    axum::extract::Path(run_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<FromQuery>,
) -> Response {
    let start_id = match resume_start(&headers, query.from.as_deref()) {
        Ok(start) => start,
        // A non-u64 resume id is a client bug: fail loudly, don't guess.
        Err(()) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let Some(run) = discovery::find_run(&app.root, &run_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    match discovery::classify(&run.path) {
        RunState::Live => {
            // Subscribe FIRST (no gap), then read the file for catch-up.
            let receiver = app
                .live
                .subscribe(run.header.run_id, run.path.clone(), &app.config);
            let replay = replay_from(&run.path, start_id);
            let (tx, rx) = mpsc::channel(STREAM_BUFFER);
            tokio::spawn(live_producer(
                tx,
                app.config.retry_ms,
                start_id,
                replay,
                receiver,
            ));
            sse_response(rx, app.config.keep_alive)
        }
        state @ (RunState::Finished | RunState::Aborted) => {
            let lines = discovery::read_event_lines(&run.path);
            let caught_up = lines
                .last()
                .map(|(id, _)| *id)
                .is_none_or(|last| start_id > last);
            // A caught-up connect/reconnect to a terminal run stops permanently.
            if caught_up {
                return StatusCode::NO_CONTENT.into_response();
            }
            let replay = lines
                .into_iter()
                .filter(|(id, _)| *id >= start_id)
                .collect();
            let (tx, rx) = mpsc::channel(STREAM_BUFFER);
            tokio::spawn(terminal_producer(
                tx,
                app.config.retry_ms,
                replay,
                state == RunState::Aborted,
            ));
            sse_response(rx, app.config.keep_alive)
        }
    }
}

/// The file events to replay for a resume from `start_id` (id ≥ `start_id`).
fn replay_from(dir: &std::path::Path, start_id: u64) -> Vec<(u64, String)> {
    discovery::read_event_lines(dir)
        .into_iter()
        .filter(|(id, _)| *id >= start_id)
        .collect()
}

/// Parse the resume point → the first `EventId` to replay. `Last-Event-ID: n`
/// (or `?from=n`) ⇒ `n + 1`; absent/empty ⇒ 0; non-`u64` ⇒ `Err` (→ 400).
fn resume_start(headers: &HeaderMap, from_query: Option<&str>) -> Result<u64, ()> {
    let header = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let raw = header.or(from_query.map(str::trim).filter(|value| !value.is_empty()));
    match raw {
        None => Ok(0),
        Some(value) => value
            .parse::<u64>()
            .map(|n| n.saturating_add(1))
            .map_err(|_| ()),
    }
}

/// The producer for a **terminal** run: retry hint, replay, then (for aborted)
/// the control frame, then EOF. No broadcast — nothing new can arrive.
async fn terminal_producer(
    tx: mpsc::Sender<Result<Event, Infallible>>,
    retry_ms: u64,
    replay: Vec<(u64, String)>,
    aborted: bool,
) {
    if send(&tx, retry_frame(retry_ms)).await.is_err() {
        return;
    }
    for (id, line) in replay {
        if send(&tx, data_frame(id, &line)).await.is_err() {
            return;
        }
    }
    if aborted {
        let _ = send(&tx, abort_frame()).await;
    }
}

/// The producer for a **live** run: retry hint, file catch-up, then the live
/// tail off the broadcast — deduping the overlap by `EventId`, ending on the
/// abort control frame, on a lag (client reconnects), or when the tailer stops
/// (run finished; `run_finished` already delivered in-band).
async fn live_producer(
    tx: mpsc::Sender<Result<Event, Infallible>>,
    retry_ms: u64,
    start_id: u64,
    replay: Vec<(u64, String)>,
    mut receiver: broadcast::Receiver<LiveMsg>,
) {
    if send(&tx, retry_frame(retry_ms)).await.is_err() {
        return;
    }
    // `next` enforces contiguity + dedupe: only ever emit the next expected id.
    let mut next = start_id;
    for (id, line) in replay {
        if id >= next {
            if send(&tx, data_frame(id, &line)).await.is_err() {
                return;
            }
            next = id + 1;
        }
    }
    loop {
        match receiver.recv().await {
            Ok(LiveMsg::Event { id, line }) if id >= next => {
                if send(&tx, data_frame(id, &line)).await.is_err() {
                    return;
                }
                next = id + 1;
            }
            // Overlap already replayed from the file — skip (dedupe).
            Ok(LiveMsg::Event { .. }) => {}
            Ok(LiveMsg::Aborted) => {
                let _ = send(&tx, abort_frame()).await;
                return;
            }
            // Lagged past the bounded buffer: end the stream rather than
            // deliver a gap; the client reconnects and replays from the file.
            Err(broadcast::error::RecvError::Lagged(_)) => return,
            // Tailer stopped (run finished) — end the stream.
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Push one frame; `Err` means the client disconnected (stop the producer).
async fn send(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    frame: Event,
) -> Result<(), mpsc::error::SendError<Result<Event, Infallible>>> {
    tx.send(Ok(frame)).await
}

/// A log-event frame: `id:` + verbatim `data:`, no `event:` (default type).
fn data_frame(id: u64, line: &str) -> Event {
    Event::default().id(id.to_string()).data(line)
}

/// The id-less, named abort control frame (a server-origin control frame, not
/// a log event, ADR 0028).
fn abort_frame() -> Event {
    Event::default().event("run_state").data(ABORT_DATA)
}

/// A `retry:`-only frame — the reconnection hint sent once at stream start.
fn retry_frame(retry_ms: u64) -> Event {
    Event::default().retry(Duration::from_millis(retry_ms))
}

/// Wrap the producer's mpsc receiver as the SSE body, with the pinned stream
/// headers and keep-alive (ADR 0028, pins §9).
fn sse_response(rx: mpsc::Receiver<Result<Event, Infallible>>, keep_alive: Duration) -> Response {
    let sse = Sse::new(SseBody { rx }).keep_alive(KeepAlive::new().interval(keep_alive));
    let mut response = sse.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response
}

/// The SSE body: a `Stream` over the producer's mpsc receiver (no extra deps —
/// `mpsc::Receiver::poll_recv` is the whole implementation).
struct SseBody {
    rx: mpsc::Receiver<Result<Event, Infallible>>,
}

impl futures_core::Stream for SseBody {
    type Item = Result<Event, Infallible>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().rx.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_map(last_event_id: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(value) = last_event_id {
            headers.insert("last-event-id", HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn resume_arithmetic_matches_the_pinned_semantics() {
        // Absent → from 0.
        assert_eq!(resume_start(&header_map(None), None), Ok(0));
        // Last-Event-ID: n → n + 1.
        assert_eq!(resume_start(&header_map(Some("5")), None), Ok(6));
        // ?from=n → n + 1 (the reload/curl fallback).
        assert_eq!(resume_start(&header_map(None), Some("5")), Ok(6));
        // Empty header/param → fresh (0).
        assert_eq!(resume_start(&header_map(Some("")), None), Ok(0));
        assert_eq!(resume_start(&header_map(None), Some("")), Ok(0));
        // Non-u64 → Err (→ 400).
        assert_eq!(resume_start(&header_map(Some("nope")), None), Err(()));
        assert_eq!(resume_start(&header_map(None), Some("1.5")), Err(()));
    }

    /// The lag path: a subscriber that falls behind a tiny broadcast buffer
    /// gets `RecvError::Lagged`, and the live producer ends the stream rather
    /// than deliver a gap (disconnect-on-lag, ADR 0028) — deterministic, no
    /// wall-clock timing.
    #[tokio::test]
    async fn live_producer_ends_the_stream_on_broadcast_lag() {
        let (sender, receiver) = broadcast::channel::<LiveMsg>(2);
        // Overflow the capacity-2 buffer before the producer drains it → the
        // receiver's first recv() will report Lagged.
        for id in 0..5 {
            sender
                .send(LiveMsg::Event {
                    id,
                    line: Arc::from(format!("{{\"id\":{id}}}").as_str()),
                })
                .unwrap();
        }
        let (tx, mut rx) = mpsc::channel(8);
        // Empty replay so the producer goes straight to the (lagged) broadcast.
        live_producer(tx, 2000, 0, Vec::new(), receiver).await;

        // The stream carried only the retry frame, then ended (no gap, no hang).
        assert!(rx.recv().await.is_some(), "the retry frame");
        assert!(
            rx.recv().await.is_none(),
            "the producer ends the stream on lag rather than delivering a gap"
        );
    }
}
