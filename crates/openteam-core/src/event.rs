//! The event schema: one envelope over a closed 26-kind taxonomy (ADR 0022).
//!
//! The append-only event log is the substrate metrics, meta-agents, the
//! report, and the e2e tests all read. Each line of `events.jsonl` is one
//! [`Event`]: `{"id":42,"at":"…","source":"agent-2","kind":"task_claimed","data":{…}}`
//! — `id` is the single ordering key, `at` an informational `Clock`
//! breadcrumb, `source` the acting agent (every non-actor subject rides in
//! the payload), and `kind`/`data` an adjacently-tagged payload a reader
//! dispatches on. Closed so the log is exhaustive and replay-capable, though
//! no replay ships in v1.

use std::fmt;

use jiff::Timestamp;
use openteam_wire::{AgentId, IdentityError, SpecialtySlug};
use serde::{Deserialize, Serialize};

use crate::directive::{DirectiveKind, DirectiveTier};
use crate::ids::{DirectiveId, EventId, KnowledgeEntryId, MessageId, RunId, TaskId, TeamId};
use crate::message::Address;

/// One append-only record of something that happened in a run (ADR 0022).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Run-scoped monotonic, 0-based, contiguous — the single ordering key.
    pub id: EventId,
    /// Informational wall-clock breadcrumb from the injected [`crate::Clock`]
    /// — never read for ordering or determinism.
    pub at: Timestamp,
    /// The acting agent (the verb caller), else `System`.
    pub source: EventSource,
    #[serde(flatten)]
    pub kind: EventKind,
}

impl Event {
    pub fn new(id: EventId, at: Timestamp, source: EventSource, kind: EventKind) -> Self {
        Self {
            id,
            at,
            source,
            kind,
        }
    }
}

/// Who an event is attributed to (CONTEXT.md: Event source) — serialized as
/// the legible agent handle (`orchestrator` / `meta-1` / `agent-2`) or the
/// literal `"system"` for runtime-internal events with no owning turn
/// (ADR 0022).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum EventSource {
    Agent(AgentId),
    System,
}

impl EventSource {
    /// The acting agent, if the source is not `system`.
    pub fn agent(&self) -> Option<&AgentId> {
        match self {
            Self::Agent(agent) => Some(agent),
            Self::System => None,
        }
    }

    pub fn is_system(&self) -> bool {
        matches!(self, Self::System)
    }
}

impl From<AgentId> for EventSource {
    fn from(agent: AgentId) -> Self {
        Self::Agent(agent)
    }
}

impl TryFrom<String> for EventSource {
    type Error = IdentityError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value == "system" {
            Ok(Self::System)
        } else {
            AgentId::parse(&value).map(Self::Agent)
        }
    }
}

impl From<EventSource> for String {
    fn from(source: EventSource) -> Self {
        match source {
            EventSource::Agent(agent) => agent.into(),
            EventSource::System => "system".into(),
        }
    }
}

impl fmt::Display for EventSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Agent(agent) => agent.fmt(f),
            Self::System => f.write_str("system"),
        }
    }
}

/// The run caps recorded in `run_started` — holds only the caps that were
/// set; serializes `{}` when none (pins §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RunCaps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ticks: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_llm_calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_duration_ms: Option<u64>,
}

/// Which cap terminated (or would terminate) the run (ADR 0022).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapKind {
    MaxTicks,
    MaxLlmCalls,
    MaxDuration,
}

impl CapKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MaxTicks => "MaxTicks",
            Self::MaxLlmCalls => "MaxLlmCalls",
            Self::MaxDuration => "MaxDuration",
        }
    }
}

/// Why the run finished — exit code 0 / 2 / 1 (ADR 0006/0022). Serializes
/// externally tagged: `"CleanFinish"` / `{"CapHit":"MaxTicks"}` /
/// `"HarnessError"` (pins §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunFinishReason {
    CleanFinish,
    CapHit(CapKind),
    HarnessError,
}

/// How a turn's inner loop ended (ADR 0015): a no-tool-call yield, or the
/// per-turn `MAX_TOOL_ITERS` cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnOutcome {
    Yielded,
    ToolIterCap,
}

/// Token usage summed over a turn's completions (`turn_completed.usage`,
/// pins §7). Informational only — the mock's numbers are synthetic
/// (ADR 0001/0018).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnUsage {
    pub prompt: u64,
    pub completion: u64,
    pub total: u64,
}

/// The state a woken agent is restored to (`agent_woke.restored`, ADR 0022):
/// `Working` with its still-claimed task, else `Idle`. Serializes externally
/// tagged: `{"Working":{"task":1}}` / `"Idle"` (pins §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RestoredState {
    Working { task: TaskId },
    Idle,
}

/// One degraded section in a `context_degraded` event (ADR 0022). `kind` is
/// the section kind in snake_case (`knowledge_retrievals`, `fresh_messages`,
/// `recent_activity`, `board_digest` — pins §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DegradedSection {
    pub kind: String,
    pub budget: u32,
    pub used: u32,
    pub dropped_items: u32,
}

/// The closed 26-kind taxonomy (ADR 0022) — adjacently tagged as
/// `"kind"`/`"data"`, flattened into the [`Event`] envelope.
///
/// Payloads carry only non-actor subjects (the actor is the envelope's
/// `source`). `via_directive` on an effect event records that a meta
/// directive caused it and is omitted when absent; every other optional
/// payload field serializes an explicit `null` (pins §7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum EventKind {
    // Lifecycle bookends (2).
    /// Event 0 — the self-describing config header.
    RunStarted {
        run_id: RunId,
        seed: u64,
        goal: String,
        agents: u32,
        meta_agents: u32,
        parallel: u32,
        scenario: Option<String>,
        caps: RunCaps,
    },
    RunFinished {
        reason: RunFinishReason,
        exit_code: u8,
    },

    // Termination (1) — precedes the forced `run_finished{CapHit(cap), 2}`.
    CapHit {
        cap: CapKind,
        limit: u64,
        observed: u64,
    },

    // Task (6) — ADR 0010; `task_completed` is the canonical name.
    TaskCreated {
        task: TaskId,
        title: String,
        description: String,
        team: Option<TeamId>,
    },
    TaskClaimed {
        task: TaskId,
        team: Option<TeamId>,
    },
    TaskReleased {
        task: TaskId,
        reason: Option<String>,
    },
    TaskUnassigned {
        task: TaskId,
        prev_claimant: AgentId,
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via_directive: Option<DirectiveId>,
    },
    TaskCompleted {
        task: TaskId,
        result: String,
        result_ref: KnowledgeEntryId,
    },
    TaskCancelled {
        task: TaskId,
        reason: String,
    },

    // Messaging & knowledge (3) — ADR 0011/0014.
    /// Source = sender.
    MessageSent {
        message: MessageId,
        address: Address,
        body: String,
        knowledge_ref: KnowledgeEntryId,
    },
    /// Source = recipient; the ids drained into this turn. The
    /// mailbox-pressure fold pairs each id's `message_sent` `EventId`
    /// against this event's `EventId` (ADR 0022).
    MessagesDelivered {
        delivered: Vec<MessageId>,
    },
    /// **Notes only** — a Message-kind or TaskCompletion-kind entry's
    /// `source_event` points at its `message_sent` / `task_completed` event
    /// instead (ADR 0022).
    KnowledgeWritten {
        entry: KnowledgeEntryId,
        text: String,
    },

    // Runtime (6) — ADR 0015/0020. `turn_completed` fires for every turn of
    // every agent; a tick IS an orchestrator `turn_completed`.
    TurnCompleted {
        first_call_seq: u64,
        last_call_seq: u64,
        tool_iters: u32,
        outcome: TurnOutcome,
        malformed: bool,
        usage: TurnUsage,
        on_task: Option<TaskId>,
    },
    /// Deliberate sleep (orchestrator verb / self-sleep / mechanical
    /// directive) — the park is separate.
    AgentSlept {
        agent: AgentId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via_directive: Option<DirectiveId>,
    },
    /// The automatic K=3-consecutive-malformed park (source = the malformed
    /// agent, ADR 0015).
    AgentParked {
        agent: AgentId,
        count: u32,
    },
    AgentWoke {
        agent: AgentId,
        restored: RestoredState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via_directive: Option<DirectiveId>,
    },
    /// The `set_parallelism` effect, clamped to `[1, --parallel]` (ADR 0020).
    ParallelismChanged {
        requested: u32,
        effective: u32,
        via_directive: DirectiveId,
    },
    /// Source = System; the ~500 ms watchdog firing (ADR 0015). Expected
    /// count 0 on the happy path.
    LivenessNudge {
        board_open: u32,
        claimed_by_asleep: u32,
    },

    // Teams (3) — ADR 0009.
    TeamFormed {
        team: TeamId,
        members: Vec<AgentId>,
    },
    /// Declarative full-set replace with computed deltas — the join/leave
    /// record.
    TeamMembersSet {
        team: TeamId,
        members: Vec<AgentId>,
        added: Vec<AgentId>,
        removed: Vec<AgentId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via_directive: Option<DirectiveId>,
    },
    TeamDissolved {
        team: TeamId,
    },

    // Specialty (1) — ADR 0003.
    AgentRespecialized {
        agent: AgentId,
        from: SpecialtySlug,
        to: SpecialtySlug,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via_directive: Option<DirectiveId>,
    },

    // Directives (3) — ADR 0005/0020. `directive_issued` fires for both
    // tiers; a mechanical one fires only on successful application, so
    // mechanical-issued ⟹ applied (ADR 0022).
    DirectiveIssued {
        directive: DirectiveId,
        tier: DirectiveTier,
        kind: DirectiveKind,
        args: serde_json::Value,
    },
    /// Judgment only — the orchestrator acted with an `in_response_to` cite.
    DirectiveFulfilled {
        directive: DirectiveId,
        by: AgentId,
    },
    DirectiveDeclined {
        directive: DirectiveId,
        kind: DirectiveKind,
        reason: String,
        by: AgentId,
    },

    // Context (1) — emitted only when a section is dropped/truncated under
    // budget pressure; 0 on the happy path (ADR 0016/0022).
    ContextDegraded {
        agent: AgentId,
        sections: Vec<DegradedSection>,
    },
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    /// The complete `events.jsonl` of the canonical dry run — the 34 literal
    /// lines of docs/prototypes/dry-run-transcript.md §8, the schema golden.
    pub(crate) const TRANSCRIPT_JSONL: &str = r#"{"id":0,"at":"2026-07-17T00:00:00Z","source":"system","kind":"run_started","data":{"run_id":"0192f1a0-7e3c-7abc-9def-000000000000","seed":42,"goal":"Write a short onboarding guide for new contributors","agents":3,"meta_agents":1,"parallel":3,"scenario":null,"caps":{}}}
{"id":1,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"team_formed","data":{"team":"t1","members":["agent-1","agent-2","agent-3"]}}
{"id":2,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"task_created","data":{"task":1,"title":"Draft the setup section","description":"Install + build/test steps for a new contributor.","team":"t1"}}
{"id":3,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"task_created","data":{"task":2,"title":"Draft the architecture overview","description":"One-paragraph crate map.","team":"t1"}}
{"id":4,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"turn_completed","data":{"first_call_seq":0,"last_call_seq":1,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":1301,"completion":94,"total":1395},"on_task":null}}
{"id":5,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"task_claimed","data":{"task":1,"team":"t1"}}
{"id":6,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"turn_completed","data":{"first_call_seq":0,"last_call_seq":1,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":812,"completion":24,"total":836},"on_task":1}}
{"id":7,"at":"2026-07-17T00:00:00Z","source":"agent-3","kind":"turn_completed","data":{"first_call_seq":0,"last_call_seq":1,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":872,"completion":26,"total":898},"on_task":null}}
{"id":8,"at":"2026-07-17T00:00:00Z","source":"agent-2","kind":"task_claimed","data":{"task":2,"team":"t1"}}
{"id":9,"at":"2026-07-17T00:00:00Z","source":"agent-2","kind":"turn_completed","data":{"first_call_seq":0,"last_call_seq":1,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":815,"completion":24,"total":839},"on_task":2}}
{"id":10,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"message_sent","data":{"message":1,"address":{"Direct":{"to":"agent-1"}},"body":"Prioritize the setup section; the guide leads with it.","knowledge_ref":1}}
{"id":11,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"turn_completed","data":{"first_call_seq":2,"last_call_seq":3,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":1180,"completion":40,"total":1220},"on_task":null}}
{"id":12,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"messages_delivered","data":{"delivered":[1]}}
{"id":13,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"knowledge_written","data":{"entry":2,"text":"Setup: install mise; then `mise run build` / `test`. Rust 1.94 via mise."}}
{"id":14,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"turn_completed","data":{"first_call_seq":2,"last_call_seq":3,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":905,"completion":38,"total":943},"on_task":1}}
{"id":15,"at":"2026-07-17T00:00:00Z","source":"agent-2","kind":"turn_completed","data":{"first_call_seq":2,"last_call_seq":3,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":898,"completion":33,"total":931},"on_task":2}}
{"id":16,"at":"2026-07-17T00:00:00Z","source":"meta-1","kind":"directive_issued","data":{"directive":1,"tier":"Judgment","kind":"propose_respecialize","args":{"agent":"agent-3","specialty":"doc-reviewer"}}}
{"id":17,"at":"2026-07-17T00:00:00Z","source":"meta-1","kind":"turn_completed","data":{"first_call_seq":0,"last_call_seq":1,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":1128,"completion":36,"total":1164},"on_task":null}}
{"id":18,"at":"2026-07-17T00:00:00Z","source":"agent-2","kind":"task_completed","data":{"task":2,"result":"Architecture overview: openteam (bin) → core + mock + leaf wire; mock depends on wire only.","result_ref":3}}
{"id":19,"at":"2026-07-17T00:00:00Z","source":"agent-2","kind":"turn_completed","data":{"first_call_seq":4,"last_call_seq":5,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":902,"completion":41,"total":943},"on_task":2}}
{"id":20,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"agent_respecialized","data":{"agent":"agent-3","from":"generalist","to":"doc-reviewer","via_directive":1}}
{"id":21,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"directive_fulfilled","data":{"directive":1,"by":"orchestrator"}}
{"id":22,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"message_sent","data":{"message":2,"address":"Broadcast","body":"Team: once setup lands, the guide is complete — no further tasks planned.","knowledge_ref":4}}
{"id":23,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"turn_completed","data":{"first_call_seq":4,"last_call_seq":5,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":1495,"completion":72,"total":1567},"on_task":null}}
{"id":24,"at":"2026-07-17T00:00:00Z","source":"meta-1","kind":"directive_issued","data":{"directive":2,"tier":"Mechanical","kind":"set_parallelism","args":{"target":2}}}
{"id":25,"at":"2026-07-17T00:00:00Z","source":"meta-1","kind":"parallelism_changed","data":{"requested":2,"effective":2,"via_directive":2}}
{"id":26,"at":"2026-07-17T00:00:00Z","source":"meta-1","kind":"turn_completed","data":{"first_call_seq":2,"last_call_seq":3,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":588,"completion":15,"total":603},"on_task":null}}
{"id":27,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"messages_delivered","data":{"delivered":[2]}}
{"id":28,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"message_sent","data":{"message":3,"address":{"Team":{"team":"t1"}},"body":"Setup section drafted; see knowledge notes.","knowledge_ref":5}}
{"id":29,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"turn_completed","data":{"first_call_seq":4,"last_call_seq":5,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":940,"completion":37,"total":977},"on_task":1}}
{"id":30,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"task_completed","data":{"task":1,"result":"Setup section: install mise; `mise run build/test/lint/fmt`; Rust 1.94 via mise.","result_ref":6}}
{"id":31,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"turn_completed","data":{"first_call_seq":6,"last_call_seq":7,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":951,"completion":44,"total":995},"on_task":1}}
{"id":32,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"turn_completed","data":{"first_call_seq":6,"last_call_seq":6,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":1210,"completion":180,"total":1390},"on_task":null}}
{"id":33,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"run_finished","data":{"reason":"CleanFinish","exit_code":0}}"#;
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::test_fixtures::TRANSCRIPT_JSONL;
    use super::*;

    fn at() -> Timestamp {
        "2026-07-17T00:00:00Z".parse().unwrap()
    }

    /// The schema golden (mandated by the #22 gate): every line of the
    /// transcript's `events.jsonl` deserializes into `Event` and
    /// re-serializes to the identical JSON value.
    #[test]
    fn transcript_section_8_round_trips_line_by_line() {
        let lines: Vec<&str> = TRANSCRIPT_JSONL.lines().collect();
        assert_eq!(lines.len(), 34, "transcript §8 has 34 events");
        for (i, line) in lines.iter().enumerate() {
            let event: Event = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line {i} failed to deserialize: {e}"));
            assert_eq!(event.id, EventId::new(i as u64), "EventIds are contiguous");
            let reserialized = serde_json::to_value(&event)
                .unwrap_or_else(|e| panic!("line {i} failed to re-serialize: {e}"));
            let original: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(reserialized, original, "line {i} drifted from the golden");
        }
    }

    #[test]
    fn source_serializes_as_bare_handle_or_system() {
        assert_eq!(
            serde_json::to_value(EventSource::Agent(AgentId::team(2))).unwrap(),
            json!("agent-2")
        );
        assert_eq!(
            serde_json::to_value(EventSource::System).unwrap(),
            json!("system")
        );
        let system: EventSource = serde_json::from_str("\"system\"").unwrap();
        assert!(system.is_system());
        let agent: EventSource = serde_json::from_str("\"meta-1\"").unwrap();
        assert_eq!(agent.agent(), Some(&AgentId::meta(1)));
        assert!(serde_json::from_str::<EventSource>("\"nobody\"").is_err());
    }

    fn event_json(source: EventSource, kind: EventKind) -> serde_json::Value {
        serde_json::to_value(Event::new(EventId::new(40), at(), source, kind)).unwrap()
    }

    /// Pins the serde shapes of the kinds the transcript run did not fire
    /// (its "not exercised by this particular run" list plus the
    /// happy-path-zero kinds).
    #[test]
    fn unexercised_kinds_serialize_to_their_pinned_shapes() {
        let orch = EventSource::Agent(AgentId::orchestrator());

        assert_eq!(
            event_json(
                orch.clone(),
                EventKind::TaskReleased {
                    task: TaskId::new(1),
                    reason: None,
                }
            ),
            json!({"id":40,"at":"2026-07-17T00:00:00Z","source":"orchestrator",
                   "kind":"task_released","data":{"task":1,"reason":null}})
        );

        assert_eq!(
            event_json(
                orch.clone(),
                EventKind::TaskUnassigned {
                    task: TaskId::new(1),
                    prev_claimant: AgentId::team(2),
                    reason: Some("stalled".into()),
                    via_directive: None,
                }
            ),
            json!({"id":40,"at":"2026-07-17T00:00:00Z","source":"orchestrator",
                   "kind":"task_unassigned",
                   "data":{"task":1,"prev_claimant":"agent-2","reason":"stalled"}})
        );

        assert_eq!(
            event_json(
                orch.clone(),
                EventKind::TaskCancelled {
                    task: TaskId::new(2),
                    reason: "obsolete".into(),
                }
            ),
            json!({"id":40,"at":"2026-07-17T00:00:00Z","source":"orchestrator",
                   "kind":"task_cancelled","data":{"task":2,"reason":"obsolete"}})
        );

        assert_eq!(
            event_json(
                EventSource::Agent(AgentId::meta(1)),
                EventKind::AgentSlept {
                    agent: AgentId::team(2),
                    via_directive: Some(DirectiveId::new(3)),
                }
            ),
            json!({"id":40,"at":"2026-07-17T00:00:00Z","source":"meta-1",
                   "kind":"agent_slept","data":{"agent":"agent-2","via_directive":3}})
        );

        assert_eq!(
            event_json(
                EventSource::Agent(AgentId::team(3)),
                EventKind::AgentParked {
                    agent: AgentId::team(3),
                    count: 3,
                }
            ),
            json!({"id":40,"at":"2026-07-17T00:00:00Z","source":"agent-3",
                   "kind":"agent_parked","data":{"agent":"agent-3","count":3}})
        );

        assert_eq!(
            event_json(
                orch.clone(),
                EventKind::AgentWoke {
                    agent: AgentId::team(3),
                    restored: RestoredState::Working {
                        task: TaskId::new(1),
                    },
                    via_directive: None,
                }
            ),
            json!({"id":40,"at":"2026-07-17T00:00:00Z","source":"orchestrator",
                   "kind":"agent_woke",
                   "data":{"agent":"agent-3","restored":{"Working":{"task":1}}}})
        );
        assert_eq!(
            serde_json::to_value(RestoredState::Idle).unwrap(),
            json!("Idle")
        );

        assert_eq!(
            event_json(
                EventSource::System,
                EventKind::LivenessNudge {
                    board_open: 1,
                    claimed_by_asleep: 1,
                }
            ),
            json!({"id":40,"at":"2026-07-17T00:00:00Z","source":"system",
                   "kind":"liveness_nudge","data":{"board_open":1,"claimed_by_asleep":1}})
        );

        assert_eq!(
            event_json(
                orch.clone(),
                EventKind::TeamMembersSet {
                    team: TeamId::parse("t1").unwrap(),
                    members: vec![AgentId::team(1), AgentId::team(3)],
                    added: vec![AgentId::team(3)],
                    removed: vec![AgentId::team(2)],
                    via_directive: None,
                }
            ),
            json!({"id":40,"at":"2026-07-17T00:00:00Z","source":"orchestrator",
                   "kind":"team_members_set",
                   "data":{"team":"t1","members":["agent-1","agent-3"],
                           "added":["agent-3"],"removed":["agent-2"]}})
        );

        assert_eq!(
            event_json(
                orch.clone(),
                EventKind::TeamDissolved {
                    team: TeamId::parse("t1").unwrap(),
                }
            ),
            json!({"id":40,"at":"2026-07-17T00:00:00Z","source":"orchestrator",
                   "kind":"team_dissolved","data":{"team":"t1"}})
        );

        assert_eq!(
            event_json(
                orch.clone(),
                EventKind::DirectiveDeclined {
                    directive: DirectiveId::new(1),
                    kind: DirectiveKind::ProposeReallocate,
                    reason: "duplicate proposal".into(),
                    by: AgentId::orchestrator(),
                }
            ),
            json!({"id":40,"at":"2026-07-17T00:00:00Z","source":"orchestrator",
                   "kind":"directive_declined",
                   "data":{"directive":1,"kind":"propose_reallocate",
                           "reason":"duplicate proposal","by":"orchestrator"}})
        );

        assert_eq!(
            event_json(
                EventSource::System,
                EventKind::CapHit {
                    cap: CapKind::MaxTicks,
                    limit: 100,
                    observed: 100,
                }
            ),
            json!({"id":40,"at":"2026-07-17T00:00:00Z","source":"system",
                   "kind":"cap_hit","data":{"cap":"MaxTicks","limit":100,"observed":100}})
        );

        assert_eq!(
            event_json(
                EventSource::System,
                EventKind::RunFinished {
                    reason: RunFinishReason::CapHit(CapKind::MaxTicks),
                    exit_code: 2,
                }
            ),
            json!({"id":40,"at":"2026-07-17T00:00:00Z","source":"system",
                   "kind":"run_finished",
                   "data":{"reason":{"CapHit":"MaxTicks"},"exit_code":2}})
        );

        assert_eq!(
            event_json(
                EventSource::Agent(AgentId::team(1)),
                EventKind::ContextDegraded {
                    agent: AgentId::team(1),
                    sections: vec![DegradedSection {
                        kind: "fresh_messages".into(),
                        budget: 800,
                        used: 812,
                        dropped_items: 2,
                    }],
                }
            ),
            json!({"id":40,"at":"2026-07-17T00:00:00Z","source":"agent-1",
                   "kind":"context_degraded",
                   "data":{"agent":"agent-1",
                           "sections":[{"kind":"fresh_messages","budget":800,
                                        "used":812,"dropped_items":2}]}})
        );
    }

    #[test]
    fn run_caps_hold_only_the_set_caps() {
        assert_eq!(serde_json::to_value(RunCaps::default()).unwrap(), json!({}));
        let caps = RunCaps {
            max_ticks: Some(50),
            max_llm_calls: None,
            max_duration_ms: None,
        };
        assert_eq!(serde_json::to_value(caps).unwrap(), json!({"max_ticks":50}));
        let back: RunCaps = serde_json::from_str("{}").unwrap();
        assert_eq!(back, RunCaps::default());
    }
}
