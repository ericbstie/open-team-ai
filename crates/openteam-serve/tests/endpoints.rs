//! Integration tests for the `/v1/` list + snapshot endpoints (ADR 0030 tier
//! 2): real loopback `serve()`, in-process, deterministic. Run dirs are
//! hand-built tempdirs (the frozen fixture lands in step 8); JSON is
//! value-golden (key order is not contract, ADR 0030).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code

use std::path::Path;
use std::time::Duration;

use rustix::fs::{FlockOperation, flock};
use serde_json::Value;
use tempfile::TempDir;

use openteam_serve::{ServeConfig, serve};

const RUN_ID: &str = "0192f1a0-7e3c-7abc-9def-000000000000";

/// A test-fast config (no CLI surface, ADR 0030).
fn fast_config() -> ServeConfig {
    ServeConfig {
        poll_interval: Duration::from_millis(5),
        keep_alive: Duration::from_millis(50),
        retry_ms: 10,
        broadcast_capacity: 8,
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

/// A complete finished run: form a team, create + claim + complete a task,
/// then finish clean.
fn finished_events(run_id: &str) -> String {
    [
        run_started(run_id),
        r#"{"id":1,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"team_formed","data":{"team":"t1","members":["agent-1","agent-2"]}}"#.into(),
        r#"{"id":2,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"task_created","data":{"task":1,"title":"Setup","description":"d","team":"t1"}}"#.into(),
        r#"{"id":3,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"task_claimed","data":{"task":1,"team":"t1"}}"#.into(),
        r#"{"id":4,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"task_completed","data":{"task":1,"result":"done1","result_ref":1}}"#.into(),
        r#"{"id":5,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"run_finished","data":{"reason":"CleanFinish","exit_code":0}}"#.into(),
    ]
    .join("\n")
        + "\n"
}

/// A partial (unfinished) run — no `run_finished` bookend.
fn partial_events(run_id: &str) -> String {
    [
        run_started(run_id),
        r#"{"id":1,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"task_claimed","data":{"task":1,"team":null}}"#.into(),
    ]
    .join("\n")
        + "\n"
}

fn write_run(root: &Path, run_id: &str, events: &str) -> std::path::PathBuf {
    let dir = root.join(run_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("events.jsonl"), events).unwrap();
    dir
}

async fn get_json(url: &str) -> (reqwest::StatusCode, Value) {
    let resp = reqwest::get(url).await.unwrap();
    let status = resp.status();
    let value = resp.json::<Value>().await.unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn run_list_is_value_golden_for_a_finished_run() {
    let root = TempDir::new().unwrap();
    write_run(root.path(), RUN_ID, &finished_events(RUN_ID));
    let (addr, handle) = serve(root.path().to_path_buf(), fast_config(), 0)
        .await
        .unwrap();

    let (status, list) = get_json(&format!("http://{addr}/v1/runs")).await;
    assert_eq!(status, 200);
    let entries = list.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry["run_id"], RUN_ID);
    assert_eq!(entry["state"], "finished");
    assert_eq!(entry["goal"], "g");
    assert_eq!(entry["seed"], 42);
    assert_eq!(entry["started_at"], "2026-07-17T00:00:00Z");
    assert_eq!(entry["last_event_id"], 5);
    assert_eq!(
        entry["finished"],
        serde_json::json!({"reason":"CleanFinish","exit_code":0})
    );
    // No counts in the run list (ADR 0028).
    assert!(entry.get("tasks_completed").is_none());

    handle.shutdown().await;
}

#[tokio::test]
async fn run_list_sorts_by_run_id_ascending_and_omits_finished_for_live() {
    let root = TempDir::new().unwrap();
    let earlier = "0192f1a0-0000-7000-8000-000000000000";
    let later = "0192f1a0-ffff-7000-8000-000000000000";
    write_run(root.path(), later, &partial_events(later));
    let earlier_dir = write_run(root.path(), earlier, &partial_events(earlier));

    // Hold `earlier`'s run.lock so it classifies live (the test is the writer,
    // ADR 0030 — a same-process open conflicts under flock).
    let lock = std::fs::File::create(earlier_dir.join("run.lock")).unwrap();
    flock(&lock, FlockOperation::LockExclusive).unwrap();

    let (addr, handle) = serve(root.path().to_path_buf(), fast_config(), 0)
        .await
        .unwrap();
    let (status, list) = get_json(&format!("http://{addr}/v1/runs")).await;
    assert_eq!(status, 200);
    let entries = list.as_array().unwrap();
    assert_eq!(entries.len(), 2);
    // UUIDv7 ascending.
    assert_eq!(entries[0]["run_id"], earlier);
    assert_eq!(entries[1]["run_id"], later);
    // `earlier` is live (lock held); `later` aborted (no lock). Neither carries
    // a `finished` block.
    assert_eq!(entries[0]["state"], "live");
    assert_eq!(entries[1]["state"], "aborted");
    assert!(entries[0].get("finished").is_none());
    assert!(entries[1].get("finished").is_none());

    drop(lock);
    handle.shutdown().await;
}

#[tokio::test]
async fn snapshot_is_value_golden_for_a_finished_run() {
    let root = TempDir::new().unwrap();
    write_run(root.path(), RUN_ID, &finished_events(RUN_ID));
    let (addr, handle) = serve(root.path().to_path_buf(), fast_config(), 0)
        .await
        .unwrap();

    let (status, snap) = get_json(&format!("http://{addr}/v1/runs/{RUN_ID}/snapshot")).await;
    assert_eq!(status, 200);

    // Top-level keys exactly as_of / run / board / agents / metrics.
    assert_eq!(snap["as_of"], 5);
    assert_eq!(snap["run"]["state"], "finished");
    assert_eq!(snap["run"]["run_id"], RUN_ID);
    assert_eq!(snap["run"]["seed"], 42);

    // Board: the board.json shape, folded (no board.json on disk here).
    let tasks = snap["board"]["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["id"], 1);
    assert_eq!(tasks[0]["state"]["Done"]["result"], "done1");
    assert_eq!(snap["board"]["teams"][0]["id"], "t1");

    // Agents: four-state, handle order.
    let agents = snap["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[0]["handle"], "agent-1");
    assert_eq!(agents[0]["state"], "idle");
    assert_eq!(agents[1]["handle"], "agent-2");

    // Metrics: RunSummary serialized; a finished run carries an outcome.
    assert_eq!(
        snap["metrics"]["outcome"],
        serde_json::json!(["CleanFinish", 0])
    );
    assert_eq!(snap["metrics"]["tasks_completed"], 1);

    handle.shutdown().await;
}

#[tokio::test]
async fn snapshot_folds_a_live_and_an_aborted_run_without_board_json() {
    let root = TempDir::new().unwrap();
    // Aborted: partial log, no lock, and crucially NO board.json on disk — the
    // fold is the only path (ADR 0028's grounding fact).
    let dir = write_run(root.path(), RUN_ID, &partial_events(RUN_ID));
    assert!(!dir.join("board.json").exists());

    let (addr, handle) = serve(root.path().to_path_buf(), fast_config(), 0)
        .await
        .unwrap();

    let (status, snap) = get_json(&format!("http://{addr}/v1/runs/{RUN_ID}/snapshot")).await;
    assert_eq!(status, 200);
    assert_eq!(snap["run"]["state"], "aborted");
    assert_eq!(
        snap["metrics"]["outcome"],
        Value::Null,
        "unfinished → null outcome"
    );
    // agent-1 claimed task 1 → working; agent-2 idle.
    let agents = snap["agents"].as_array().unwrap();
    assert_eq!(
        agents[0]["state"],
        serde_json::json!({"working": {"task": 1}})
    );

    // Now make it live by holding the lock, and re-snapshot.
    let lock = std::fs::File::create(dir.join("run.lock")).unwrap();
    flock(&lock, FlockOperation::LockExclusive).unwrap();
    let (_, snap) = get_json(&format!("http://{addr}/v1/runs/{RUN_ID}/snapshot")).await;
    assert_eq!(snap["run"]["state"], "live");
    drop(lock);

    handle.shutdown().await;
}

#[tokio::test]
async fn snapshot_of_an_unknown_run_is_404() {
    let root = TempDir::new().unwrap();
    write_run(root.path(), RUN_ID, &finished_events(RUN_ID));
    let (addr, handle) = serve(root.path().to_path_buf(), fast_config(), 0)
        .await
        .unwrap();

    let unknown = "0192f1a0-dead-7000-8000-000000000000";
    let resp = reqwest::get(format!("http://{addr}/v1/runs/{unknown}/snapshot"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    // A non-UUID id is also 404 (parse guard, no path traversal).
    let resp = reqwest::get(format!("http://{addr}/v1/runs/not-a-uuid/snapshot"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    handle.shutdown().await;
}

#[tokio::test]
async fn debug_page_is_served_at_root_as_html() {
    // The single permitted assertion for the non-contract debug page (ADR
    // 0029/0030): GET / returns 200 text/html.
    let root = TempDir::new().unwrap();
    let (addr, handle) = serve(root.path().to_path_buf(), fast_config(), 0)
        .await
        .unwrap();

    let resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        content_type.starts_with("text/html"),
        "content-type was {content_type:?}"
    );

    handle.shutdown().await;
}
