//! Stream-server e2e (ADR 0030's thin bin tier): drive a real `--mock` run to
//! completion, then (a) spawn `openteam serve` as a child process and hit its
//! endpoints over loopback, and (b) fold every fresh run dir with the library
//! helper, asserting *folded snapshot ≡ board.json* for any seed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code

mod common;

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::drive;
use serde_json::Value;

/// Kills the spawned server on drop, even if an assertion panics.
struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawn `openteam serve --dir <root> --port 0`, piping stdout so the caller
/// can read the bound-address line.
fn spawn_serve(runs_root: &std::path::Path) -> ChildGuard {
    let child = Command::new(assert_cmd::cargo::cargo_bin("openteam"))
        .arg("serve")
        .arg("--dir")
        .arg(runs_root)
        .arg("--port")
        .arg("0")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn openteam serve");
    ChildGuard(child)
}

/// Run `openteam run --mock` with `cwd = <root>` and no `--out-dir`, so the
/// artifacts land in the default `<root>/.openteam/runs/<run_id>/` layout the
/// server discovers (ADR 0027).
fn drive_default_layout(root: &std::path::Path, goal: &str) {
    assert_cmd::Command::cargo_bin("openteam")
        .unwrap()
        .current_dir(root)
        .args([
            "run",
            goal,
            "--mock",
            "--seed",
            "42",
            "--agents",
            "3",
            "--meta-agents",
            "1",
            "--quiet",
        ])
        .timeout(Duration::from_secs(90))
        .assert()
        .success();
}

#[test]
fn serve_child_lists_snapshots_and_streams_a_mock_run() {
    let root = tempfile::tempdir().unwrap();
    drive_default_layout(
        root.path(),
        "Write a short onboarding guide for new contributors",
    );
    let runs_root = root.path().join(".openteam/runs");

    // The single discovered run dir, named by its run_id.
    let run_dir = std::fs::read_dir(&runs_root)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("a run dir under .openteam/runs");
    let run_id = run_dir.file_name().unwrap().to_string_lossy().into_owned();
    let last_id = std::fs::read_to_string(run_dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .count() as u64
        - 1;

    // Spawn the server on an ephemeral port; read the parseable bound-address
    // line (pins §9) — the address is the token after the final space.
    let mut child = spawn_serve(&runs_root);
    let stdout = child.0.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let addr_line = lines
        .next()
        .expect("the server prints a line")
        .expect("readable stdout");
    assert!(
        addr_line.starts_with("openteam serve listening on http://"),
        "unexpected line: {addr_line:?}"
    );
    let base = addr_line.rsplit(' ').next().unwrap().to_string();

    let client = reqwest::blocking::Client::new();

    // List: one finished run.
    let list: Value = client
        .get(format!("{base}/v1/runs"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    let entries = list.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["run_id"], run_id);
    assert_eq!(entries[0]["state"], "finished");

    // Snapshot: the folded board equals board.json (finished-run equivalence).
    let board: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("board.json")).unwrap())
            .unwrap();
    let snap: Value = client
        .get(format!("{base}/v1/runs/{run_id}/snapshot"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(snap["board"], board);
    assert_eq!(snap["run"]["state"], "finished");

    // Stream: a caught-up reconnect to the finished run stops with 204.
    let resp = client
        .get(format!("{base}/v1/runs/{run_id}/events"))
        .header("Last-Event-ID", last_id.to_string())
        .send()
        .unwrap();
    assert_eq!(resp.status(), 204);

    // And a fresh connect replays the log then ends (200, non-empty body).
    let body = client
        .get(format!("{base}/v1/runs/{run_id}/events"))
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(body.contains("data:"), "the fresh stream replays events");
    // `child` (the ChildGuard) drops at end of scope, killing the server.
}

#[test]
fn folded_snapshot_equals_board_json_for_any_seed() {
    // The e2e invariant tier (ADR 0030): fold every fresh --mock run dir with
    // the public library helper — no server — and assert equivalence, lifting
    // it from "holds for one frozen log" to "holds for any seed".
    for seed in ["42", "7", "1234567"] {
        let dir = tempfile::tempdir().unwrap();
        let run = drive(
            "Summarize the release notes",
            dir.path(),
            &["--seed", seed, "--agents", "2", "--meta-agents", "1"],
            &[],
        );
        assert_eq!(run.exit_code(), 0, "seed {seed}");

        let folded = openteam_serve::folded_board(dir.path())
            .unwrap_or_else(|| panic!("seed {seed}: folded a run dir"));
        assert_eq!(
            serde_json::to_value(&folded).unwrap(),
            run.board(),
            "seed {seed}: folded snapshot ≡ board.json",
        );
    }
}
