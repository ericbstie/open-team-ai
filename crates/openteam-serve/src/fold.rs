//! The reader-side fold: `events.jsonl` → a server-folded snapshot (ADR 0028).
//!
//! One uniform fold serves all three run states — `board.json` is written only
//! at finalize, so **live** runs don't have one yet and **aborted** runs never
//! get one; the server folds `events.jsonl` and **never reads `board.json`**
//! (ADR 0028's grounding fact). The fold drives core's public `Board` mutators
//! and `Metrics::fold` (ADR 0030), so for finished runs *folded snapshot ≡
//! board.json* holds by construction — the equivalence contract test.
//!
//! Agents carry the **four-state** wire vocabulary (`idle | working | asleep |
//! parked`, ADR 0028); the internal `MeterState` is untouched.

use openteam_core::{Board, BoardSnapshot, Event, EventKind, Metrics, RestoredState, RunId};
use openteam_wire::{AgentId, SpecialtySlug};

use crate::discovery::{RunHeader, RunState};
use crate::schema::{AgentEntry, AgentWireState, SnapshotResponse};

/// Per-team-agent fold state: the four-state position plus the current
/// specialty. Agents boot generalist and Idle from `run_started`'s count.
struct AgentFold {
    entries: Vec<(AgentId, AgentWireState, SpecialtySlug)>,
}

impl AgentFold {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Enumerate `agent-1..count`, all Idle + generalist (from `run_started`).
    fn init(&mut self, count: u32) {
        self.entries = (1..=count as usize)
            .map(|n| {
                (
                    AgentId::team(n),
                    AgentWireState::Idle,
                    SpecialtySlug::generalist(),
                )
            })
            .collect();
    }

    fn set_state(&mut self, agent: &AgentId, state: AgentWireState) {
        if let Some(entry) = self.entries.iter_mut().find(|(id, ..)| id == agent) {
            entry.1 = state;
        }
    }

    fn set_specialty(&mut self, agent: &AgentId, specialty: SpecialtySlug) {
        if let Some(entry) = self.entries.iter_mut().find(|(id, ..)| id == agent) {
            entry.2 = specialty;
        }
    }

    fn into_entries(self) -> Vec<AgentEntry> {
        self.entries
            .into_iter()
            .map(|(handle, state, specialty)| AgentEntry {
                handle,
                specialty,
                state,
            })
            .collect()
    }
}

/// The folded projection over a run's events (board + four-state agents +
/// metrics + `as_of`). `board.json` is never read — this fold is the only path.
struct Folded {
    as_of: u64,
    board: Board,
    agents: AgentFold,
    metrics: Metrics,
}

/// Fold a run's complete events in order, replaying board mutations through
/// core's public `Board` mutators and the four-state agent transitions, and
/// folding `Metrics` in lockstep (ADR 0020/0028).
fn fold_events(events: &[Event]) -> Folded {
    let mut board = Board::new();
    let mut agents = AgentFold::new();
    let mut metrics = Metrics::new();
    let mut as_of = 0_u64;

    for event in events {
        as_of = event.id.get();
        metrics.fold(event);
        apply_board(&mut board, event);
        apply_agent_state(&mut agents, event);
    }

    Folded {
        as_of,
        board,
        agents,
        metrics,
    }
}

/// Replay one event's board mutation through core's public mutators. A valid
/// log's mutations always succeed (the runtime emitted the event only after
/// the mutation committed); a rejection would surface as folded-snapshot drift
/// caught by the equivalence test, so errors are dropped, not panicked on.
fn apply_board(board: &mut Board, event: &Event) {
    let source = event.source.agent().cloned();
    match &event.kind {
        EventKind::TaskCreated {
            task,
            title,
            description,
            team,
        } => {
            let created_by = source.unwrap_or_else(AgentId::orchestrator);
            let _ = board.create_task(
                *task,
                title.clone(),
                description.clone(),
                created_by,
                event.id,
                team.clone(),
            );
        }
        EventKind::TaskClaimed { task, .. } => {
            if let Some(agent) = source {
                let _ = board.claim(&agent, *task);
            }
        }
        EventKind::TaskReleased { .. } => {
            if let Some(agent) = source {
                let _ = board.release(&agent);
            }
        }
        EventKind::TaskUnassigned { task, .. } => {
            let _ = board.unassign(*task);
        }
        EventKind::TaskCompleted {
            result, result_ref, ..
        } => {
            if let Some(agent) = source {
                let _ = board.complete(&agent, result.clone(), *result_ref);
            }
        }
        EventKind::TaskCancelled { task, reason } => {
            let _ = board.cancel(*task, reason.clone());
        }
        EventKind::TeamFormed { team, members } => {
            let _ = board.form_team(team.clone(), members.clone());
        }
        EventKind::TeamMembersSet { team, members, .. } => {
            let _ = board.set_team_members(team, members.clone());
        }
        EventKind::TeamDissolved { team } => {
            let _ = board.dissolve_team(team);
        }
        _ => {}
    }
}

/// The four-state agent transitions, pinned by ADR 0028: `task_claimed` →
/// working; `task_released` / `task_unassigned` / `task_completed` → idle;
/// `agent_slept` → asleep; `agent_parked` → parked; `agent_woke` → the
/// restored `Working{task}` / `Idle`. Non-team subjects (orchestrator/meta)
/// are ignored.
fn apply_agent_state(agents: &mut AgentFold, event: &Event) {
    match &event.kind {
        EventKind::RunStarted { agents: count, .. } => agents.init(*count),
        EventKind::TaskClaimed { task, .. } => {
            if let Some(agent) = event.source.agent() {
                agents.set_state(agent, AgentWireState::Working { task: *task });
            }
        }
        EventKind::TaskReleased { .. } | EventKind::TaskCompleted { .. } => {
            if let Some(agent) = event.source.agent() {
                agents.set_state(agent, AgentWireState::Idle);
            }
        }
        EventKind::TaskUnassigned { prev_claimant, .. } => {
            agents.set_state(prev_claimant, AgentWireState::Idle);
        }
        EventKind::AgentSlept { agent, .. } => agents.set_state(agent, AgentWireState::Asleep),
        EventKind::AgentParked { agent, .. } => agents.set_state(agent, AgentWireState::Parked),
        EventKind::AgentWoke {
            agent, restored, ..
        } => {
            let state = match restored {
                RestoredState::Working { task } => AgentWireState::Working { task: *task },
                RestoredState::Idle => AgentWireState::Idle,
            };
            agents.set_state(agent, state);
        }
        EventKind::AgentRespecialized { agent, to, .. } => agents.set_specialty(agent, to.clone()),
        _ => {}
    }
}

/// Build the `run` block: the `run_started.data` object verbatim plus the
/// `state` field (ADR 0028). Non-object headers (should never happen for a
/// discovered run) degrade to a bare `{ "state": … }`.
fn run_block(header: &RunHeader, state: RunState) -> serde_json::Value {
    let mut run = header.data.clone();
    if let Some(object) = run.as_object_mut() {
        object.insert("state".into(), state.as_wire_str().into());
    }
    run
}

/// Assemble the full snapshot response for a run (ADR 0028): fold its events,
/// then wrap in `{ as_of, run, board, agents, metrics }`. `run_id`, `goal`,
/// and `seed` come from the header — the same values `board.json` carries.
pub(crate) fn snapshot(header: &RunHeader, state: RunState, events: &[Event]) -> SnapshotResponse {
    let folded = fold_events(events);
    let board = BoardSnapshot::new(
        header.run_id,
        header.goal.clone(),
        header.seed,
        &folded.board,
    );
    SnapshotResponse {
        as_of: folded.as_of,
        run: run_block(header, state),
        board,
        agents: folded.agents.into_entries(),
        metrics: folded.metrics.summary(),
    }
}

/// Fold a run's board into a [`BoardSnapshot`] alone — the cheap equivalence
/// helper (ADR 0030): fold `events.jsonl`, compare to `board.json`. Exposed
/// publicly through [`crate::folded_board`] for the e2e invariant tier.
pub(crate) fn board_snapshot(
    run_id: RunId,
    goal: &str,
    seed: u64,
    events: &[Event],
) -> BoardSnapshot {
    let folded = fold_events(events);
    BoardSnapshot::new(run_id, goal, seed, &folded.board)
}

#[cfg(test)]
mod tests {
    use super::*;
    use openteam_core::{KnowledgeEntryId, TaskId, TeamId};

    fn parse_events(jsonl: &str) -> Vec<Event> {
        jsonl
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn agent_state(entries: &[AgentEntry], handle: &str) -> AgentWireState {
        entries
            .iter()
            .find(|e| e.handle.as_str() == handle)
            .unwrap_or_else(|| panic!("agent {handle} missing"))
            .state
    }

    const RUN_ID: &str = "0192f1a0-7e3c-7abc-9def-000000000000";

    /// A small but complete finished run: form a team, create + claim +
    /// complete a task, respecialize the now-idle claimant, then finish.
    fn finished_log() -> String {
        [
            r#"{"id":0,"at":"2026-07-17T00:00:00Z","source":"system","kind":"run_started","data":{"run_id":"0192f1a0-7e3c-7abc-9def-000000000000","seed":42,"goal":"g","agents":2,"meta_agents":0,"parallel":2,"scenario":null,"caps":{}}}"#,
            r#"{"id":1,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"team_formed","data":{"team":"t1","members":["agent-1","agent-2"]}}"#,
            r#"{"id":2,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"task_created","data":{"task":1,"title":"Setup","description":"d","team":"t1"}}"#,
            r#"{"id":3,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"task_claimed","data":{"task":1,"team":"t1"}}"#,
            r#"{"id":4,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"task_completed","data":{"task":1,"result":"done1","result_ref":1}}"#,
            r#"{"id":5,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"agent_respecialized","data":{"agent":"agent-1","from":"generalist","to":"reviewer"}}"#,
            r#"{"id":6,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"run_finished","data":{"reason":"CleanFinish","exit_code":0}}"#,
        ]
        .join("\n")
    }

    /// The **folded snapshot ≡ board.json** equivalence contract (ADR 0028/
    /// 0030): folding the events reconstructs exactly the board the runtime
    /// would persist — proven here against a board built directly via the same
    /// public mutators (the runtime's own path), value-equal.
    #[test]
    fn folded_board_equals_the_directly_built_board_json() {
        let events = parse_events(&finished_log());
        let folded = fold_events(&events);
        let folded_board =
            BoardSnapshot::new(RUN_ID.parse::<RunId>().unwrap(), "g", 42, &folded.board);

        // The runtime's construction: apply the same mutations directly.
        let mut expected = Board::new();
        expected
            .form_team(
                TeamId::parse("t1").unwrap(),
                vec![AgentId::team(1), AgentId::team(2)],
            )
            .unwrap();
        expected
            .create_task(
                TaskId::new(1),
                "Setup",
                "d",
                AgentId::orchestrator(),
                openteam_core::EventId::new(2),
                Some(TeamId::parse("t1").unwrap()),
            )
            .unwrap();
        expected.claim(&AgentId::team(1), TaskId::new(1)).unwrap();
        expected
            .complete(&AgentId::team(1), "done1", KnowledgeEntryId::new(1))
            .unwrap();
        let expected_board =
            BoardSnapshot::new(RUN_ID.parse::<RunId>().unwrap(), "g", 42, &expected);

        assert_eq!(
            serde_json::to_value(&folded_board).unwrap(),
            serde_json::to_value(&expected_board).unwrap(),
        );
    }

    /// The finished run folds to all-Idle agents with agent-1 respecialized,
    /// and `as_of` = the last event id.
    #[test]
    fn folding_a_finished_run_yields_four_state_agents_and_as_of() {
        let events = parse_events(&finished_log());
        let folded = fold_events(&events);
        let entries = folded.agents.into_entries();

        assert_eq!(folded.as_of, 6, "as_of is the last event id");
        assert_eq!(entries.len(), 2);
        assert_eq!(agent_state(&entries, "agent-1"), AgentWireState::Idle);
        assert_eq!(agent_state(&entries, "agent-2"), AgentWireState::Idle);
        assert_eq!(
            entries
                .iter()
                .find(|e| e.handle.as_str() == "agent-1")
                .unwrap()
                .specialty
                .as_str(),
            "reviewer"
        );
    }

    /// The full four-state transition table, including park and woke-restores.
    #[test]
    fn four_state_transitions_cover_working_asleep_parked_and_woke() {
        // run_started(2 agents) → claim → slept → park → woke(restored Idle).
        let jsonl = r#"{"id":0,"at":"2026-07-17T00:00:00Z","source":"system","kind":"run_started","data":{"run_id":"0192f1a0-7e3c-7abc-9def-000000000000","seed":1,"goal":"g","agents":2,"meta_agents":0,"parallel":2,"scenario":null,"caps":{}}}
{"id":1,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"task_created","data":{"task":1,"title":"t","description":"d","team":null}}
{"id":2,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"task_claimed","data":{"task":1,"team":null}}"#;
        let entries = fold_events(&parse_events(jsonl)).agents.into_entries();
        assert_eq!(
            agent_state(&entries, "agent-1"),
            AgentWireState::Working {
                task: TaskId::new(1)
            }
        );
        assert_eq!(agent_state(&entries, "agent-2"), AgentWireState::Idle);

        // agent-2 sleeps, agent-1 parks, agent-1 woken back to Working.
        let jsonl = format!(
            "{jsonl}\n{}\n{}\n{}",
            r#"{"id":3,"at":"2026-07-17T00:00:00Z","source":"agent-2","kind":"agent_slept","data":{"agent":"agent-2"}}"#,
            r#"{"id":4,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"agent_parked","data":{"agent":"agent-1","count":3}}"#,
            r#"{"id":5,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"agent_woke","data":{"agent":"agent-1","restored":{"Working":{"task":1}}}}"#,
        );
        let entries = fold_events(&parse_events(&jsonl)).agents.into_entries();
        assert_eq!(agent_state(&entries, "agent-2"), AgentWireState::Asleep);
        assert_eq!(
            agent_state(&entries, "agent-1"),
            AgentWireState::Working {
                task: TaskId::new(1)
            },
            "woke restored the still-claimed task"
        );

        // A K=3 park that is NOT woken stays surfaced as parked (not asleep).
        let parked_only = format!(
            "{}\n{}\n{}",
            r#"{"id":0,"at":"2026-07-17T00:00:00Z","source":"system","kind":"run_started","data":{"run_id":"0192f1a0-7e3c-7abc-9def-000000000000","seed":1,"goal":"g","agents":1,"meta_agents":0,"parallel":1,"scenario":null,"caps":{}}}"#,
            r#"{"id":1,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"agent_parked","data":{"agent":"agent-1","count":3}}"#,
            r#"{"id":2,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"turn_completed","data":{"first_call_seq":0,"last_call_seq":0,"tool_iters":0,"outcome":"Yielded","malformed":false,"usage":{"prompt":1,"completion":1,"total":2},"on_task":null}}"#,
        );
        let entries = fold_events(&parse_events(&parked_only))
            .agents
            .into_entries();
        assert_eq!(
            agent_state(&entries, "agent-1"),
            AgentWireState::Parked,
            "the malformed park is surfaced, never collapsed into asleep"
        );
    }

    /// The four-state agents serialize to the pinned lowercase, externally
    /// tagged shapes (pins §9).
    #[test]
    fn agent_states_serialize_to_the_pinned_shapes() {
        assert_eq!(
            serde_json::to_value(AgentWireState::Idle).unwrap(),
            serde_json::json!("idle")
        );
        assert_eq!(
            serde_json::to_value(AgentWireState::Asleep).unwrap(),
            serde_json::json!("asleep")
        );
        assert_eq!(
            serde_json::to_value(AgentWireState::Parked).unwrap(),
            serde_json::json!("parked")
        );
        assert_eq!(
            serde_json::to_value(AgentWireState::Working {
                task: TaskId::new(3)
            })
            .unwrap(),
            serde_json::json!({"working": {"task": 3}})
        );
    }

    /// An empty/partial log yields a coherent live snapshot with correct
    /// `as_of` and a null (unfinished) metrics outcome.
    #[test]
    fn partial_log_yields_a_coherent_live_snapshot() {
        let jsonl = r#"{"id":0,"at":"2026-07-17T00:00:00Z","source":"system","kind":"run_started","data":{"run_id":"0192f1a0-7e3c-7abc-9def-000000000000","seed":7,"goal":"g","agents":2,"meta_agents":0,"parallel":2,"scenario":null,"caps":{}}}
{"id":1,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"task_created","data":{"task":1,"title":"t","description":"d","team":null}}"#;
        let events = parse_events(jsonl);
        let folded = fold_events(&events);
        assert_eq!(folded.as_of, 1);
        // Both agents idle, one open task, run not finished.
        let summary = folded.metrics.summary();
        assert_eq!(summary.outcome, None, "a live run has no outcome yet");
        assert_eq!(summary.tasks_created, 1);
    }
}
