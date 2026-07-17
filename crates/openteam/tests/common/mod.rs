//! Shared helpers for the e2e suite (ADR 0025 tier 2): drive the real bin
//! with `assert_cmd`, then assert on the persisted artifacts.

#![allow(dead_code)] // each tests/*.rs target uses a subset

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;

/// One driven run: the process output plus the artifacts directory.
pub struct DrivenRun {
    pub output: Output,
    pub dir: PathBuf,
}

impl DrivenRun {
    pub fn exit_code(&self) -> i32 {
        self.output.status.code().unwrap_or(-1)
    }

    pub fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.output.stdout).into_owned()
    }

    pub fn events(&self) -> Vec<Value> {
        let raw = std::fs::read_to_string(self.dir.join("events.jsonl"))
            .expect("events.jsonl must exist");
        raw.lines()
            .map(|line| serde_json::from_str(line).expect("every event line parses"))
            .collect()
    }

    pub fn board(&self) -> Value {
        serde_json::from_str(
            &std::fs::read_to_string(self.dir.join("board.json")).expect("board.json must exist"),
        )
        .expect("board.json parses")
    }

    pub fn report_md(&self) -> String {
        std::fs::read_to_string(self.dir.join("report.md")).expect("report.md must exist")
    }
}

/// Run `openteam run` with a tempdir `--out-dir` plus the given extra args.
/// `--mock` pins the deterministic offline backend (ADR 0026 — the default
/// path now targets a real endpoint); `--quiet` keeps stdout == report.md; a
/// wall-clock cap guards CI.
pub fn drive(goal: &str, dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> DrivenRun {
    let mut cmd = assert_cmd::Command::cargo_bin("openteam").expect("bin builds");
    cmd.arg("run")
        .arg(goal)
        .arg("--mock")
        .arg("--out-dir")
        .arg(dir)
        .arg("--quiet")
        .args(args);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let output = cmd
        .timeout(std::time::Duration::from_secs(90))
        .output()
        .expect("bin runs");
    DrivenRun {
        output,
        dir: dir.to_path_buf(),
    }
}

pub fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// Events of one kind.
pub fn of_kind<'a>(events: &'a [Value], kind: &str) -> Vec<&'a Value> {
    events.iter().filter(|e| e["kind"] == kind).collect()
}

/// Fold board conservation: every task_created id ends Done or Cancelled.
pub fn assert_board_conservation(events: &[Value], board: &Value) {
    let created: Vec<u64> = of_kind(events, "task_created")
        .iter()
        .map(|e| e["data"]["task"].as_u64().unwrap())
        .collect();
    for id in created {
        let task = board["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"].as_u64() == Some(id))
            .unwrap_or_else(|| panic!("task {id} missing from board.json"));
        let state = &task["state"];
        let terminal = state
            .as_object()
            .map(|o| o.contains_key("Done") || o.contains_key("Cancelled"))
            .unwrap_or(false);
        assert!(
            terminal,
            "task {id} must end Done or Cancelled, got {state}"
        );
    }
}
