//! Integration tests for the SSE stream endpoint (ADR 0030 tier 2): real
//! loopback `serve()`. `data:` payloads are byte-golden vs the events.jsonl
//! lines; framing is asserted semantics-only (id present/absent, event name,
//! status codes) — keep-alive/field-order/blank-line placement are not
//! contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code

use std::path::Path;
use std::time::Duration;

use rustix::fs::{FlockOperation, flock};
use tempfile::TempDir;

use openteam_serve::{ServeConfig, serve};

const RUN_ID: &str = "0192f1a0-7e3c-7abc-9def-000000000000";

fn fast_config() -> ServeConfig {
    ServeConfig {
        poll_interval: Duration::from_millis(5),
        keep_alive: Duration::from_secs(30),
        retry_ms: 2000,
        broadcast_capacity: 16,
    }
}

fn run_started(run_id: &str) -> String {
    format!(
        "{{\"id\":0,\"at\":\"2026-07-17T00:00:00Z\",\"source\":\"system\",\
         \"kind\":\"run_started\",\"data\":{{\"run_id\":\"{run_id}\",\"seed\":42,\
         \"goal\":\"g\",\"agents\":2,\"meta_agents\":0,\"parallel\":2,\
         \"scenario\":null,\"caps\":{{}}}}}}"
    )
}

/// A finished run: run_started, a claim, then run_finished.
fn finished_events() -> String {
    [
        run_started(RUN_ID),
        r#"{"id":1,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"task_claimed","data":{"task":1,"team":null}}"#.into(),
        r#"{"id":2,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"run_finished","data":{"reason":"CleanFinish","exit_code":0}}"#.into(),
    ]
    .join("\n")
        + "\n"
}

fn write_run(root: &Path, events: &str) -> std::path::PathBuf {
    let dir = root.join(RUN_ID);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("events.jsonl"), events).unwrap();
    dir
}

/// A parsed SSE frame — semantics only (framing incidentals are not contract).
#[derive(Debug, Default)]
struct Frame {
    id: Option<String>,
    event: Option<String>,
    data: Option<String>,
    retry: Option<String>,
}

impl Frame {
    fn is_comment(&self) -> bool {
        self.id.is_none() && self.event.is_none() && self.data.is_none() && self.retry.is_none()
    }
}

fn parse_sse(raw: &str) -> Vec<Frame> {
    raw.split("\n\n")
        .map(|block| {
            let mut frame = Frame::default();
            for line in block.split('\n') {
                // SSE strips exactly one optional leading space after `field:`.
                let value = |rest: &str| rest.strip_prefix(' ').unwrap_or(rest).to_string();
                if let Some(rest) = line.strip_prefix("id:") {
                    frame.id = Some(value(rest));
                } else if let Some(rest) = line.strip_prefix("event:") {
                    frame.event = Some(value(rest));
                } else if let Some(rest) = line.strip_prefix("data:") {
                    frame.data = Some(value(rest));
                } else if let Some(rest) = line.strip_prefix("retry:") {
                    frame.retry = Some(value(rest));
                }
            }
            frame
        })
        .filter(|frame| !frame.is_comment())
        .collect()
}

/// Data (log-event) frames only, in order.
fn data_frames(frames: &[Frame]) -> Vec<&Frame> {
    frames
        .iter()
        .filter(|f| f.data.is_some() && f.event.is_none())
        .collect()
}

/// GET a finite (terminal) SSE stream to EOF, with a guard timeout.
async fn get_stream(url: &str) -> (reqwest::StatusCode, String) {
    let resp = reqwest::get(url).await.unwrap();
    let status = resp.status();
    let body = tokio::time::timeout(Duration::from_secs(10), resp.text())
        .await
        .expect("stream ends within the timeout")
        .unwrap();
    (status, body)
}

#[tokio::test]
async fn sse_data_payloads_are_byte_golden_and_the_stream_ends() {
    let root = TempDir::new().unwrap();
    let events = finished_events();
    write_run(root.path(), &events);
    let (addr, handle) = serve(root.path().to_path_buf(), fast_config(), 0)
        .await
        .unwrap();

    let (status, body) = get_stream(&format!("http://{addr}/v1/runs/{RUN_ID}/events")).await;
    assert_eq!(status, 200);
    let frames = parse_sse(&body);

    // The retry hint is present (reconnection time).
    assert!(
        frames.iter().any(|f| f.retry.as_deref() == Some("2000")),
        "retry hint frame present"
    );

    // Each data payload is byte-identical to its events.jsonl line, with its
    // decimal EventId as the `id:` and no `event:` name.
    let data = data_frames(&frames);
    let lines: Vec<&str> = events.lines().collect();
    assert_eq!(data.len(), lines.len(), "one frame per line");
    for (frame, line) in data.iter().zip(lines.iter()) {
        assert_eq!(
            frame.data.as_deref(),
            Some(*line),
            "byte-golden data payload"
        );
        assert!(frame.id.is_some(), "log events carry an id");
        assert!(frame.event.is_none(), "no event: field on log events");
    }
    // The ids are the decimal EventIds 0,1,2.
    assert_eq!(
        data.iter()
            .map(|f| f.id.clone().unwrap())
            .collect::<Vec<_>>(),
        vec!["0", "1", "2"]
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn resume_from_header_query_and_fresh_are_equivalent() {
    let root = TempDir::new().unwrap();
    write_run(root.path(), &finished_events());
    let (addr, handle) = serve(root.path().to_path_buf(), fast_config(), 0)
        .await
        .unwrap();
    let base = format!("http://{addr}/v1/runs/{RUN_ID}/events");

    // Fresh connect → from EventId 0 → all three events.
    let (_, fresh) = get_stream(&base).await;
    let fresh_ids: Vec<String> = data_frames(&parse_sse(&fresh))
        .iter()
        .map(|f| f.id.clone().unwrap())
        .collect();
    assert_eq!(fresh_ids, vec!["0", "1", "2"]);

    // Last-Event-ID: 1 → replay from 2.
    let client = reqwest::Client::new();
    let resp = client
        .get(&base)
        .header("Last-Event-ID", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let header_body = resp.text().await.unwrap();
    let header_ids: Vec<String> = data_frames(&parse_sse(&header_body))
        .iter()
        .map(|f| f.id.clone().unwrap())
        .collect();
    assert_eq!(header_ids, vec!["2"], "Last-Event-ID: n replays from n+1");

    // ?from=1 → identical to Last-Event-ID: 1.
    let (_, query_body) = get_stream(&format!("{base}?from=1")).await;
    let query_ids: Vec<String> = data_frames(&parse_sse(&query_body))
        .iter()
        .map(|f| f.id.clone().unwrap())
        .collect();
    assert_eq!(query_ids, header_ids, "?from= equals Last-Event-ID");

    handle.shutdown().await;
}

#[tokio::test]
async fn unparseable_resume_id_is_400() {
    let root = TempDir::new().unwrap();
    write_run(root.path(), &finished_events());
    let (addr, handle) = serve(root.path().to_path_buf(), fast_config(), 0)
        .await
        .unwrap();
    let base = format!("http://{addr}/v1/runs/{RUN_ID}/events");

    let client = reqwest::Client::new();
    let resp = client
        .get(&base)
        .header("Last-Event-ID", "not-a-number")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    let resp = reqwest::get(format!("{base}?from=1.5")).await.unwrap();
    assert_eq!(resp.status(), 400);

    handle.shutdown().await;
}

#[tokio::test]
async fn caught_up_connect_to_terminal_runs_is_204() {
    let root = TempDir::new().unwrap();
    write_run(root.path(), &finished_events());
    let (addr, handle) = serve(root.path().to_path_buf(), fast_config(), 0)
        .await
        .unwrap();
    let base = format!("http://{addr}/v1/runs/{RUN_ID}/events");

    // Finished run, already past the last event (id 2) → 204.
    let client = reqwest::Client::new();
    let resp = client
        .get(&base)
        .header("Last-Event-ID", "2")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204, "caught-up finished run");

    // An aborted run (no bookend, no lock), caught up → also 204.
    let aborted = [
        run_started(RUN_ID),
        r#"{"id":1,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"task_claimed","data":{"task":1,"team":null}}"#.into(),
    ]
    .join("\n")
        + "\n";
    let root2 = TempDir::new().unwrap();
    write_run(root2.path(), &aborted);
    let (addr2, handle2) = serve(root2.path().to_path_buf(), fast_config(), 0)
        .await
        .unwrap();
    let resp = client
        .get(format!("http://{addr2}/v1/runs/{RUN_ID}/events"))
        .header("Last-Event-ID", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204, "caught-up aborted run");

    handle.shutdown().await;
    handle2.shutdown().await;
}

#[tokio::test]
async fn live_to_aborted_emits_the_id_less_control_frame_then_eof() {
    let root = TempDir::new().unwrap();
    // A live run: run_started + a claim, holding the flock (the test is the
    // writer, ADR 0030).
    let live = [
        run_started(RUN_ID),
        r#"{"id":1,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"task_claimed","data":{"task":1,"team":null}}"#.into(),
    ]
    .join("\n")
        + "\n";
    let dir = write_run(root.path(), &live);
    let lock = std::fs::File::create(dir.join("run.lock")).unwrap();
    flock(&lock, FlockOperation::LockExclusive).unwrap();

    let (addr, handle) = serve(root.path().to_path_buf(), fast_config(), 0)
        .await
        .unwrap();

    // Connect while live (the handler classifies live under the held lock).
    let resp = reqwest::get(format!("http://{addr}/v1/runs/{RUN_ID}/events"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Append a fresh event, then drop the lock (no run_finished bookend) → the
    // tailer sees the flock freed and the run aborts.
    std::fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .and_then(|mut f| {
            std::io::Write::write_all(
                &mut f,
                b"{\"id\":2,\"at\":\"2026-07-17T00:00:00Z\",\"source\":\"agent-1\",\"kind\":\"task_completed\",\"data\":{\"task\":1,\"result\":\"r\",\"result_ref\":1}}\n",
            )
        })
        .unwrap();
    drop(lock);

    // The stream ends after the abort control frame — read to EOF.
    let body = tokio::time::timeout(Duration::from_secs(10), resp.text())
        .await
        .expect("the aborted stream ends within the timeout")
        .unwrap();
    let frames = parse_sse(&body);

    // The fresh event was live-tailed (byte-golden), then the id-less named
    // control frame, then EOF.
    let data = data_frames(&frames);
    assert_eq!(
        data.iter()
            .map(|f| f.id.clone().unwrap())
            .collect::<Vec<_>>(),
        vec!["0", "1", "2"],
        "replay 0,1 + live-tailed 2"
    );

    let control = frames
        .iter()
        .find(|f| f.event.as_deref() == Some("run_state"))
        .expect("an abort control frame");
    assert_eq!(control.data.as_deref(), Some(r#"{"state":"aborted"}"#));
    assert!(control.id.is_none(), "the control frame is id-less");

    handle.shutdown().await;
}
