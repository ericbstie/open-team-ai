//! The nine ADR 0023 fixtures, each backing its one ADR 0025 assertion —
//! the pathologies the built-in arc structurally cannot produce.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code

mod common;

use common::{drive, fixture, of_kind};

#[test]
fn stall_claimed_task_never_completes() {
    let dir = tempfile::tempdir().unwrap();
    let run = drive(
        "goal",
        dir.path(),
        &[
            "--seed",
            "1",
            "--agents",
            "2",
            "--scenario",
            &fixture("stall.json"),
            "--max-llm-calls",
            "40",
            "--max-duration",
            "60",
        ],
        &[],
    );
    assert_eq!(run.exit_code(), 2, "only a cap ends a stall");
    let events = run.events();
    assert!(of_kind(&events, "task_completed").is_empty(), "no progress");
    assert!(
        !of_kind(&events, "task_claimed").is_empty(),
        "claimed, then stalled"
    );
    // The claimant kept burning turns on the task without completing it.
    let claimant_turns = of_kind(&events, "turn_completed")
        .iter()
        .filter(|e| e["data"]["on_task"].as_u64() == Some(1))
        .count();
    assert!(claimant_turns >= 3, "got {claimant_turns} working turns");
    let board = run.board();
    assert!(
        board["tasks"][0]["state"]["Claimed"].is_object(),
        "the stalled task is still Claimed in the final snapshot"
    );
}

#[test]
fn livelock_messages_churn_with_no_completion() {
    let dir = tempfile::tempdir().unwrap();
    let run = drive(
        "goal",
        dir.path(),
        &[
            "--seed",
            "1",
            "--agents",
            "2",
            "--scenario",
            &fixture("livelock.json"),
            "--max-llm-calls",
            "60",
            "--max-duration",
            "60",
        ],
        &[],
    );
    assert_eq!(run.exit_code(), 2);
    let events = run.events();
    assert!(of_kind(&events, "task_completed").is_empty());
    assert!(
        of_kind(&events, "task_claimed").is_empty(),
        "nobody ever claims"
    );
    let pair_messages = of_kind(&events, "message_sent")
        .iter()
        .filter(|e| e["data"]["address"]["Direct"].is_object())
        .count();
    assert!(
        pair_messages >= 6,
        "pair churn, got {pair_messages} messages"
    );
}

#[test]
fn message_flood_builds_mailbox_pressure() {
    let dir = tempfile::tempdir().unwrap();
    let run = drive(
        "goal",
        dir.path(),
        &[
            "--seed",
            "1",
            "--agents",
            "2",
            "--scenario",
            &fixture("message-flood.json"),
            "--max-llm-calls",
            "60",
            "--max-duration",
            "60",
        ],
        &[],
    );
    assert_eq!(run.exit_code(), 2);
    let events = run.events();
    let broadcasts = of_kind(&events, "message_sent")
        .iter()
        .filter(|e| e["data"]["address"] == "Broadcast")
        .count();
    assert!(broadcasts >= 5, "volume by address kind, got {broadcasts}");
    assert!(
        !of_kind(&events, "messages_delivered").is_empty(),
        "recipients drained under pressure"
    );
}

#[test]
fn context_collapse_degrades_retrievals_and_fresh_messages() {
    let dir = tempfile::tempdir().unwrap();
    let run = drive(
        "goal with several extra words to retrieve against",
        dir.path(),
        &[
            "--seed",
            "1",
            "--agents",
            "2",
            "--scenario",
            &fixture("context-collapse.json"),
            "--max-llm-calls",
            "60",
            "--max-duration",
            "60",
        ],
        &[("OPENTEAM_ASSEMBLY_BUDGET", "40")],
    );
    assert_eq!(run.exit_code(), 2);
    let events = run.events();
    let degraded = of_kind(&events, "context_degraded");
    assert!(!degraded.is_empty(), "the tiny pool must force degradation");
    let kinds: std::collections::HashSet<String> = degraded
        .iter()
        .flat_map(|e| e["data"]["sections"].as_array().unwrap().iter())
        .map(|s| s["kind"].as_str().unwrap().to_string())
        .collect();
    assert!(
        kinds.contains("knowledge_retrievals"),
        "retrievals degraded; saw {kinds:?}"
    );
    assert!(
        kinds.contains("fresh_messages"),
        "fresh messages degraded; saw {kinds:?}"
    );
}

#[test]
fn malformed_k3_parks_after_three_all_invalid_turns() {
    let dir = tempfile::tempdir().unwrap();
    let run = drive(
        "goal",
        dir.path(),
        &[
            "--seed",
            "1",
            "--agents",
            "1",
            "--scenario",
            &fixture("malformed-k3.json"),
            // No meta layer: this test targets the K=3 park path alone;
            // the pending-directive/watchdog interaction (#28) is the
            // deadlock fixture's job.
            "--meta-agents",
            "0",
            // Generous tick headroom: the park must land before any cap;
            // the wall-clock cap then ends the claimed-by-asleep stall.
            "--max-ticks",
            "50",
            "--max-duration",
            "6",
        ],
        &[],
    );
    assert_eq!(run.exit_code(), 2);
    let events = run.events();
    let parks = of_kind(&events, "agent_parked");
    assert_eq!(parks.len(), 1, "parked exactly once");
    assert_eq!(parks[0]["data"]["count"], 3, "at K=3");
    assert_eq!(
        parks[0]["source"], "agent-1",
        "source = the malformed agent"
    );

    // The rejected re-claim was a well-formed turn: malformed=false, and it
    // did not count toward the park.
    let agent_turns: Vec<_> = of_kind(&events, "turn_completed")
        .into_iter()
        .filter(|e| e["source"] == "agent-1")
        .collect();
    let malformed: Vec<bool> = agent_turns
        .iter()
        .map(|e| e["data"]["malformed"].as_bool().unwrap())
        .collect();
    assert_eq!(
        malformed.iter().filter(|&&m| m).count(),
        3,
        "exactly the three bogus turns are malformed: {malformed:?}"
    );
    assert!(!malformed[1], "the rejected re-claim turn is well-formed");

    // Park preserves the claimed task (ADR 0015).
    let board = run.board();
    assert_eq!(board["tasks"][0]["state"]["Claimed"]["by"], "agent-1");
}

#[test]
fn cap_hit_persists_partial_artifacts_with_leftover_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let run = drive(
        "goal",
        dir.path(),
        &[
            "--seed",
            "1",
            "--agents",
            "2",
            "--scenario",
            &fixture("cap-hit.json"),
            "--max-llm-calls",
            "30",
            "--max-duration",
            "60",
        ],
        &[],
    );
    assert_eq!(run.exit_code(), 2);
    let events = run.events();
    let cap = of_kind(&events, "cap_hit");
    assert_eq!(cap.len(), 1);
    assert_eq!(cap[0]["data"]["cap"], "MaxLlmCalls");
    let finished = of_kind(&events, "run_finished");
    assert_eq!(finished[0]["data"]["reason"]["CapHit"], "MaxLlmCalls");
    assert_eq!(finished[0]["source"], "system");

    // Partial artifacts persisted with leftover non-terminal tasks.
    let board = run.board();
    let leftover = board["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["state"] == "Open" || t["state"]["Claimed"].is_object())
        .count();
    assert!(leftover >= 1, "runaway leaves Open/Claimed tasks behind");
    assert!(
        run.report_md()
            .contains("terminated: MaxLlmCalls cap before finish_run")
    );
    assert_eq!(run.stdout(), run.report_md(), "stub report still == stdout");
}

#[test]
fn meta_directive_round_trip_issued_then_fulfilled() {
    let dir = tempfile::tempdir().unwrap();
    let run = drive(
        "goal",
        dir.path(),
        &[
            "--seed",
            "1",
            "--agents",
            "2",
            "--meta-agents",
            "1",
            "--scenario",
            &fixture("meta-directive.json"),
            "--max-ticks",
            "60",
            "--max-duration",
            "60",
        ],
        &[],
    );
    assert_eq!(run.exit_code(), 0, "stdout: {}", run.stdout());
    let events = run.events();
    let issued = of_kind(&events, "directive_issued");
    assert!(
        issued.iter().any(|e| e["data"]["tier"] == "Judgment"
            && e["data"]["kind"] == "propose_respecialize"),
        "the judgment directive was issued"
    );
    let fulfilled = of_kind(&events, "directive_fulfilled");
    assert_eq!(fulfilled.len(), 1);
    assert_eq!(fulfilled[0]["data"]["directive"], 1);
    assert_eq!(fulfilled[0]["data"]["by"], "orchestrator");
    assert!(
        of_kind(&events, "agent_respecialized")
            .iter()
            .any(|e| e["data"]["via_directive"] == 1 && e["data"]["agent"] == "agent-2"),
        "the action event carries via_directive"
    );
}

#[test]
fn declined_directive_records_the_reason_and_wakes_the_meta() {
    let dir = tempfile::tempdir().unwrap();
    // Nobody works in this fixture, so the run ends on the tick cap — which
    // guarantees the meta's priority-wake turn lands before termination.
    let run = drive(
        "goal",
        dir.path(),
        &[
            "--seed",
            "1",
            "--agents",
            "1",
            "--meta-agents",
            "1",
            "--scenario",
            &fixture("declined-directive.json"),
            "--max-ticks",
            "8",
            "--max-duration",
            "60",
        ],
        &[],
    );
    assert_eq!(run.exit_code(), 2, "stdout: {}", run.stdout());
    let events = run.events();
    let declined = of_kind(&events, "directive_declined");
    assert_eq!(declined.len(), 1);
    assert_eq!(declined[0]["data"]["reason"], "allocation is fine");
    assert_eq!(declined[0]["data"]["by"], "orchestrator");
    let declined_id = declined[0]["id"].as_u64().unwrap();
    // The decline priority-wakes the meta: it takes another turn after.
    assert!(
        of_kind(&events, "turn_completed")
            .iter()
            .any(|e| e["source"] == "meta-1" && e["id"].as_u64().unwrap() > declined_id),
        "a meta turn follows the decline"
    );
}

#[test]
fn deadlock_fires_the_liveness_watchdog_despite_a_pending_directive() {
    let dir = tempfile::tempdir().unwrap();
    let run = drive(
        "goal",
        dir.path(),
        &[
            "--seed",
            "1",
            "--agents",
            "2",
            "--scenario",
            &fixture("deadlock.json"),
            // The scripted meta parks a judgment directive in the
            // yield-forever orchestrator's queue. Before the #28 fix that
            // pending directive suppressed the watchdog forever (this test
            // ran `--meta-agents 0` to sidestep it); now a directive the
            // orchestrator has seen and left pending stops generating
            // ticks, quiescence is reached, and the watchdog fires.
            "--meta-agents",
            "1",
            // Tick headroom so the ~500 ms watchdog gets its window; the
            // wall-clock cap terminates the quiescent run.
            "--max-ticks",
            "50",
            "--max-duration",
            "6",
        ],
        &[],
    );
    assert_eq!(run.exit_code(), 2, "terminates on a cap");
    let events = run.events();
    let issued = of_kind(&events, "directive_issued");
    assert!(
        issued.iter().any(|e| e["data"]["tier"] == "Judgment"
            && e["data"]["kind"] == "propose_respecialize"),
        "the meta's judgment directive was issued"
    );
    assert!(
        of_kind(&events, "directive_fulfilled").is_empty()
            && of_kind(&events, "directive_declined").is_empty(),
        "the directive stayed pending for the whole run"
    );
    assert!(
        !of_kind(&events, "liveness_nudge").is_empty(),
        "the watchdog fired on quiescent-but-unfinished despite the pending directive (#28)"
    );
    assert_eq!(
        of_kind(&events, "agent_slept").len(),
        2,
        "both agents self-slept"
    );
    assert!(
        of_kind(&events, "agent_woke").is_empty(),
        "the nudge never auto-wakes"
    );
}
