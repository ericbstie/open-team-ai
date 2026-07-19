//! Happy-path and CLI e2e (ADR 0025 tier 2): the built-in arc at a pinned
//! seed IS the canonical clean run — invariant-only assertions over the
//! persisted artifacts, never global byte-identity.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code

mod common;

use common::{assert_board_conservation, drive, of_kind};

#[test]
fn happy_path_pinned_seed_full_parallel() {
    let dir = tempfile::tempdir().unwrap();
    let run = drive(
        "Write a short onboarding guide for new contributors",
        dir.path(),
        &[
            "--seed",
            "42",
            "--agents",
            "3",
            "--meta-agents",
            "1",
            "--max-duration",
            "60",
        ],
        &[],
    );
    assert_eq!(run.exit_code(), 0, "stderr: {:?}", run.output);

    let events = run.events();
    assert_eq!(events[0]["kind"], "run_started", "event 0 is the header");
    assert_eq!(events[0]["data"]["seed"], 42);

    // Contiguous 0-based EventIds (ADR 0011 amendment).
    for (i, event) in events.iter().enumerate() {
        assert_eq!(event["id"].as_u64(), Some(i as u64));
    }

    // Termination via finish_run, run_finished last.
    let last = events.last().unwrap();
    assert_eq!(last["kind"], "run_finished");
    assert_eq!(last["data"]["reason"], "CleanFinish");
    assert_eq!(last["data"]["exit_code"], 0);
    assert_eq!(last["source"], "orchestrator");

    // Board conservation.
    assert!(!of_kind(&events, "task_created").is_empty());
    assert_board_conservation(&events, &run.board());

    // The happy path never degrades and the watchdog never fires.
    assert!(of_kind(&events, "liveness_nudge").is_empty());
    assert!(of_kind(&events, "context_degraded").is_empty());
    assert!(of_kind(&events, "agent_parked").is_empty());

    // `openteam run` holds a `run.lock` flock for the run's lifetime
    // (ADR 0027) — the file stays after finalize; the stream server's
    // classifier reads its lock state, not its presence.
    assert!(
        dir.path().join("run.lock").exists(),
        "run.lock must exist in the run dir"
    );

    // --quiet: stdout is byte-identical to the persisted report.md
    // (ADR 0022/0024).
    assert_eq!(run.stdout(), run.report_md());
    assert!(run.stdout().contains("## Run summary"));
}

#[test]
fn happy_path_pinned_seed_parallel_one_orders_tighter() {
    let dir = tempfile::tempdir().unwrap();
    let run = drive(
        "Write a short onboarding guide for new contributors",
        dir.path(),
        &[
            "--seed",
            "42",
            "--agents",
            "3",
            "--meta-agents",
            "1",
            "--parallel",
            "1",
            "--max-duration",
            "60",
        ],
        &[],
    );
    assert_eq!(run.exit_code(), 0);

    let events = run.events();
    assert_board_conservation(&events, &run.board());
    assert!(of_kind(&events, "liveness_nudge").is_empty());

    // Per-source ordered subsequences (ADR 0025): for every completed task,
    // the claimant's task_claimed precedes its task_completed — phrased
    // over "the claimant of task N", never a fixed handle.
    for done in of_kind(&events, "task_completed") {
        let task = done["data"]["task"].as_u64().unwrap();
        let claimant = done["source"].as_str().unwrap();
        let claim_id =
            events
                .iter()
                .find(|e| {
                    e["kind"] == "task_claimed"
                        && e["data"]["task"].as_u64() == Some(task)
                        && e["source"] == done["source"]
                })
                .unwrap_or_else(|| panic!("task {task} completed by {claimant} without its claim"))
                ["id"]
                .as_u64()
                .unwrap();
        assert!(claim_id < done["id"].as_u64().unwrap());
    }

    // Per-agent event order is a clean filter on source: each agent's
    // turn_completed call-seq spans are strictly increasing.
    let mut spans: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for turn in of_kind(&events, "turn_completed") {
        let source = turn["source"].as_str().unwrap().to_string();
        let first = turn["data"]["first_call_seq"].as_u64().unwrap();
        let last = turn["data"]["last_call_seq"].as_u64().unwrap();
        assert!(first <= last);
        if let Some(previous_last) = spans.get(&source) {
            assert!(
                first > *previous_last,
                "{source}: call-seq span must advance monotonically"
            );
        }
        spans.insert(source, last);
    }
}

#[test]
fn seed_variety_two_seeds_still_hold_invariants() {
    for seed in ["7", "1234567"] {
        let dir = tempfile::tempdir().unwrap();
        let run = drive(
            "Summarize the release notes",
            dir.path(),
            &[
                "--seed",
                seed,
                "--agents",
                "2",
                "--meta-agents",
                "1",
                "--max-duration",
                "60",
            ],
            &[],
        );
        assert_eq!(run.exit_code(), 0, "seed {seed}");
        let events = run.events();
        let created = of_kind(&events, "task_created").len() as u64;
        assert!((1..=8).contains(&created), "T in [1,8], got {created}");
        assert_board_conservation(&events, &run.board());
        assert!(of_kind(&events, "liveness_nudge").is_empty());
    }
}

#[test]
fn usage_error_parallel_above_agents_exits_2_with_no_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("never-created");
    let mut cmd = assert_cmd::Command::cargo_bin("openteam").unwrap();
    let assert = cmd
        .args([
            "run",
            "goal",
            "--agents",
            "2",
            "--parallel",
            "5",
            "--out-dir",
        ])
        .arg(&out)
        .assert()
        .code(2);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.contains("--parallel"), "stderr: {stderr}");
    // The exit-2 discriminator (ADR 0024): a usage error leaves no
    // artifacts directory and no run_started — unlike a cap-hit 2.
    assert!(!out.exists(), "usage errors must not create artifacts");
}

#[test]
fn invalid_scenario_exits_2_before_any_run() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, r#"{"version": 99, "scripts": []}"#).unwrap();
    let out = dir.path().join("never-created");
    let mut cmd = assert_cmd::Command::cargo_bin("openteam").unwrap();
    cmd.args(["run", "goal", "--mock", "--scenario"])
        .arg(&bad)
        .arg("--out-dir")
        .arg(&out)
        .assert()
        .code(2);
    assert!(!out.exists());
}

#[test]
fn missing_goal_is_a_clap_usage_error() {
    let mut cmd = assert_cmd::Command::cargo_bin("openteam").unwrap();
    cmd.arg("run").assert().code(2);
}
