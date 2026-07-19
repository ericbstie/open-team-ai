//! Integration tests against the **frozen fixture** run dir (ADR 0030): a real
//! `--mock` run's `events.jsonl` + `board.json`, captured once under
//! `tests/fixtures/<run_id>/`. This is where the list/snapshot/SSE goldens and
//! the finished-run *folded snapshot ≡ board.json* equivalence run against real
//! bytes (the hand-built tempdirs cover the edge cases).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;

use openteam_serve::{ServeConfig, folded_board, serve};

fn fast_config() -> ServeConfig {
    ServeConfig {
        poll_interval: Duration::from_millis(5),
        keep_alive: Duration::from_secs(30),
        retry_ms: 2000,
        broadcast_capacity: 16,
    }
}

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// The single frozen run under `tests/fixtures/` — its run_id (dir name) and
/// path. Copied into a tempdir root so the server sees exactly one run.
fn stage_fixture() -> (String, TempDir) {
    let root = fixtures_root();
    let run_dir = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir() && p.join("events.jsonl").exists())
        .expect("a frozen fixture run dir exists");
    let run_id = run_dir.file_name().unwrap().to_string_lossy().into_owned();

    let staged = TempDir::new().unwrap();
    let dest = staged.path().join(&run_id);
    std::fs::create_dir_all(&dest).unwrap();
    for name in ["events.jsonl", "board.json"] {
        std::fs::copy(run_dir.join(name), dest.join(name)).unwrap();
    }
    (run_id, staged)
}

fn board_json(root: &Path, run_id: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(root.join(run_id).join("board.json")).unwrap())
        .unwrap()
}

fn events_lines(root: &Path, run_id: &str) -> Vec<String> {
    std::fs::read_to_string(root.join(run_id).join("events.jsonl"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn fixture_list_and_snapshot_are_value_golden() {
    let (run_id, staged) = stage_fixture();
    let board = board_json(staged.path(), &run_id);
    let (addr, handle) = serve(staged.path().to_path_buf(), fast_config(), 0)
        .await
        .unwrap();

    // Run list: the one finished run, cheap fields from the real header.
    let list = reqwest::get(format!("http://{addr}/v1/runs"))
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let entry = &list.as_array().unwrap()[0];
    assert_eq!(entry["run_id"], run_id);
    assert_eq!(entry["state"], "finished");
    assert_eq!(entry["seed"], board["seed"]);
    assert_eq!(entry["goal"], board["goal"]);
    assert_eq!(entry["finished"]["reason"], "CleanFinish");
    assert_eq!(entry["finished"]["exit_code"], 0);

    // Snapshot: the folded board equals board.json verbatim (value-golden) —
    // the finished-run equivalence contract against real bytes (ADR 0028/0030).
    let snap = reqwest::get(format!("http://{addr}/v1/runs/{run_id}/snapshot"))
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(snap["board"], board, "folded snapshot ≡ board.json");
    assert_eq!(snap["run"]["state"], "finished");
    assert_eq!(
        snap["metrics"]["outcome"],
        serde_json::json!(["CleanFinish", 0])
    );

    handle.shutdown().await;
}

#[tokio::test]
async fn fixture_sse_replay_is_byte_golden_and_caught_up_is_204() {
    let (run_id, staged) = stage_fixture();
    let lines = events_lines(staged.path(), &run_id);
    let (addr, handle) = serve(staged.path().to_path_buf(), fast_config(), 0)
        .await
        .unwrap();
    let base = format!("http://{addr}/v1/runs/{run_id}/events");

    // Fresh connect replays every line, byte-golden in the `data:` payload.
    let body = tokio::time::timeout(
        Duration::from_secs(10),
        reqwest::get(&base).await.unwrap().text(),
    )
    .await
    .expect("terminal stream ends")
    .unwrap();
    let data: Vec<String> = body
        .split("\n\n")
        .filter_map(|block| {
            block.split('\n').find_map(|l| {
                l.strip_prefix("data:")
                    .map(|r| r.strip_prefix(' ').unwrap_or(r).to_string())
            })
        })
        // Drop any non-log frame (none here, but keep the filter honest).
        .filter(|d| d.starts_with('{'))
        .collect();
    assert_eq!(
        data, lines,
        "every SSE data payload is byte-identical to its line"
    );

    // A caught-up reconnect to the finished run stops permanently (204).
    let last_id = lines.len() as u64 - 1;
    let resp = reqwest::Client::new()
        .get(&base)
        .header("Last-Event-ID", last_id.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    handle.shutdown().await;
}

#[tokio::test]
async fn folded_board_equals_board_json_via_the_library_helper() {
    // The library-call form of the equivalence (no server): `folded_board` is
    // what the bin e2e folds over every fresh --mock run dir.
    let (run_id, staged) = stage_fixture();
    let board = board_json(staged.path(), &run_id);
    let folded = folded_board(&staged.path().join(&run_id)).unwrap();
    assert_eq!(serde_json::to_value(&folded).unwrap(), board);
}
