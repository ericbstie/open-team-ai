//! The agent runtime: capped-inner-loop turns, the three-state team-agent
//! lifecycle, and the event-driven scheduler (ADRs 0002/0006/0007/0015),
//! plus verb dispatch onto the single serial write path (ADR 0011/0017) and
//! the run entrypoint that composes everything (ADR 0022/0024).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use openteam_wire::{
    AgentId, ChatCompletionRequest, ChatMessage, MessageContent, Role, SpecialtySlug, ToolCall,
    ToolChoice, ToolChoiceMode,
};
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore};

use crate::artifacts;
use crate::board::{Board, TaskState};
use crate::clock::Clock;
use crate::context::{
    AssembleView, AssembledPrompt, ContextPolicy, SpecialtyProfile, assemble, claimed_task_line,
    recent_event_line, retrieval_line, window_line,
};
use crate::directive::{Directive, DirectiveKind, DirectiveState, DirectiveTier};
use crate::event::{
    CapKind, Event, EventKind, EventSource, RestoredState, RunCaps, RunFinishReason, TurnOutcome,
    TurnUsage,
};
use crate::ids::{DirectiveId, EventId, MessageId, RunId, TaskId, TeamId};
use crate::knowledge::{InMemoryVectorStore, KnowledgeKind, VectorStore};
use crate::llm::{AgentChannel, LlmClient, WireEmbedder};
use crate::message::{Address, Mailboxes, Message};
use crate::metrics::Metrics;
use crate::tools::{self, ToolOutcome, ToolRegistry};

/// The meta coalesced-cadence threshold (pins §5): unobserved events not
/// sourced by the observing meta before a turn fires.
const META_CADENCE_THRESHOLD: u64 = 6;
/// A task's Nth release priority-wakes the metas (pins §5).
const RELEASE_PRIORITY_THRESHOLD: u32 = 3;
/// K consecutive malformed turns park an agent (ADR 0015).
const MALFORMED_PARK_K: u32 = 3;
/// The liveness watchdog period (ADR 0015).
const WATCHDOG_PERIOD: Duration = Duration::from_millis(500);
/// Auto-retrieval top-k (pins §5).
const AUTO_RETRIEVAL_K: usize = 3;
/// How many trailing events feed the meta's recent-events window.
const RECENT_EVENTS_WINDOW: usize = 20;

/// Everything `openteam run` resolves before handing the core an
/// already-pure input set (ADR 0024).
#[derive(Debug, Clone)]
pub struct RunConfig {
    pub goal: String,
    pub agents: usize,
    pub meta_agents: usize,
    /// Resolved: defaults to `agents`, validated `<= agents` in the bin.
    pub parallel: usize,
    /// Resolved in the bin — random per run unless `--seed` (ADR 0024).
    pub seed: u64,
    pub max_ticks: Option<u64>,
    pub max_llm_calls: Option<u64>,
    pub max_duration: Option<Duration>,
    pub max_tool_iters: u32,
    pub model: String,
    pub embedding_model: String,
    /// Embed locally (feature hashing) instead of calling `/embeddings` — for
    /// endpoints without an OpenAI embeddings route, e.g. Open WebUI (ADR 0001).
    pub local_embeddings: bool,
    /// `--out-dir` override; default `.openteam/runs/<run-id>/`.
    pub out_dir: Option<PathBuf>,
    /// The `--scenario` path recorded in `run_started` (ADR 0022).
    pub scenario: Option<String>,
    /// The `OPENTEAM_ASSEMBLY_BUDGET` test knob (pins §6).
    pub assembly_budget: Option<usize>,
    /// Serialize the reactor to at most one in-flight turn, so the event
    /// order (and therefore the whole run) is a pure function of the seed and
    /// goal — byte-identical across invocations. Set by `--mock`, where
    /// reproducibility is the point; the real path leaves it off to keep
    /// `--parallel`'s completion overlap (pins §5, determinism note). The
    /// deterministic mock responses are already pure functions of their
    /// request (ADR 0021); this removes the only remaining nondeterminism —
    /// the order concurrent turns win the single write-path lock (ADR 0011).
    pub serial_dispatch: bool,
}

impl RunConfig {
    /// A minimal config for tests and embedding callers.
    pub fn new(goal: impl Into<String>) -> Self {
        Self {
            goal: goal.into(),
            agents: 4,
            meta_agents: 1,
            parallel: 4,
            seed: 0,
            max_ticks: None,
            max_llm_calls: None,
            max_duration: None,
            max_tool_iters: 8,
            model: "openteam-mock".into(),
            embedding_model: "openteam-mock".into(),
            local_embeddings: false,
            out_dir: None,
            scenario: None,
            assembly_budget: None,
            serial_dispatch: false,
        }
    }
}

/// What a finished run hands the bin: the ADR 0006 exit code, the rendered
/// report (byte-identical to `report.md`, ADR 0024), and where the
/// artifacts live.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub exit_code: u8,
    pub report: String,
    pub run_dir: PathBuf,
    pub run_id: RunId,
}

/// A true harness fault before the run could start (artifact IO). Faults
/// after `run_started` finish the run with exit 1 instead (ADR 0006).
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("failed to create run artifacts: {0}")]
    Artifacts(#[from] std::io::Error),
}

/// A team agent's lifecycle position (ADR 0015). The malformed park enters
/// the same `Asleep`, distinguished only by its entry event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Working { task: TaskId },
    Asleep,
}

/// Per-team-agent runtime bookkeeping.
struct AgentSlot {
    state: AgentState,
    in_flight: bool,
    consecutive_malformed: u32,
    profile: SpecialtyProfile,
    /// The recent-activity sliding window lines (ADR 0016) — cleared on a
    /// successful claim (the assignment boundary), wiped on respecialize.
    window: Vec<String>,
    turn_index: u64,
    /// Event-log length at this agent's last turn end (edge-trigger, pins §5).
    watermark: usize,
    channel: Arc<AgentChannel>,
}

/// Control-plane bookkeeping (orchestrator, meta-agents) — no lifecycle
/// state (ADR 0015), plus the meta cadence counters (ADR 0020).
struct ControlSlot {
    in_flight: bool,
    turn_index: u64,
    watermark: usize,
    channel: Arc<AgentChannel>,
    /// Meta only: events not sourced by this meta since its last turn.
    unobserved: u64,
    /// Meta only: an immediate priority wake is pending.
    priority: bool,
}

impl ControlSlot {
    fn new(channel: Arc<AgentChannel>) -> Self {
        Self {
            in_flight: false,
            turn_index: 0,
            watermark: 0,
            channel,
            unobserved: 0,
            priority: false,
        }
    }
}

/// The run world behind the single serialized write path (ADR 0011): one
/// `tokio::sync::Mutex` all mutations flow through, so the four contiguous
/// id counters, the event append, the store ingest, and the mailbox pushes
/// commit atomically per step.
pub(crate) struct World {
    goal: String,
    seed: u64,
    board: Board,
    messages: BTreeMap<MessageId, Message>,
    mailboxes: Mailboxes,
    store: InMemoryVectorStore<WireEmbedder>,
    directives: Vec<Directive>,
    next_task: u64,
    next_message: u64,
    next_directive: u64,
    next_event: u64,
    events: Vec<Event>,
    metrics: Metrics,
    clock: Arc<dyn Clock>,
    events_writer: Option<artifacts::EventsWriter>,
    io_error: Option<String>,
    team: BTreeMap<AgentId, AgentSlot>,
    orchestrator: ControlSlot,
    metas: BTreeMap<AgentId, ControlSlot>,
    release_counts: HashMap<TaskId, u32>,
    report: Option<String>,
    finish_requested: bool,
    finishing: Option<(RunFinishReason, u8)>,
    harness_error: Option<String>,
    first_tick_fired: bool,
    forced_tick: bool,
    ticks: u64,
    llm_calls: u64,
    turns_in_flight: usize,
    effective_parallelism: usize,
    /// The CLI `--parallel` ceiling `set_parallelism` clamps to (ADR 0020).
    configured_parallelism: usize,
    permit_debt: usize,
    semaphore: Arc<Semaphore>,
}

impl World {
    /// Append one event on the serial write path: allocate the contiguous
    /// 0-based `EventId`, stamp the `Clock`, fold metrics, stream to
    /// `events.jsonl`, update the meta cadence counters (ADR 0011/0020/0022).
    fn append_event(&mut self, source: EventSource, kind: EventKind) -> EventId {
        let id = EventId::new(self.next_event);
        self.next_event += 1;
        let event = Event::new(id, self.clock.now(), source, kind);
        self.metrics.fold(&event);
        if let Some(writer) = &mut self.events_writer
            && let Err(error) = writer.append(&event)
            && self.io_error.is_none()
        {
            self.io_error = Some(error.to_string());
        }

        // Meta cadence (ADR 0020, pins §5): count events not sourced by the
        // observing meta; priority-wake on the high-signal kinds.
        let priority = match &event.kind {
            EventKind::AgentParked { .. }
            | EventKind::DirectiveDeclined { .. }
            | EventKind::LivenessNudge { .. } => true,
            EventKind::TaskReleased { task, .. } => {
                let count = self.release_counts.entry(*task).or_insert(0);
                *count += 1;
                *count >= RELEASE_PRIORITY_THRESHOLD
            }
            _ => false,
        };
        for (meta, slot) in &mut self.metas {
            if event.source.agent() != Some(meta) {
                slot.unobserved += 1;
                if priority {
                    slot.priority = true;
                }
            }
        }

        self.events.push(event);
        id
    }

    /// The `EventId` the next `append_event` will mint — for the up-front
    /// cross-reference allocation of one write-path step (ADR 0011/0014).
    fn planned_event(&self) -> EventId {
        EventId::new(self.next_event)
    }

    fn team_agent_ids(&self) -> Vec<AgentId> {
        self.team.keys().cloned().collect()
    }

    fn is_team_agent(&self, agent: &AgentId) -> bool {
        self.team.contains_key(agent)
    }

    /// Message recipients at acceptance time, sender excluded (ADR 0011,
    /// pins §1): broadcast = orchestrator + team agents (meta-agents
    /// observe via events).
    fn recipients_for(&self, sender: &AgentId, address: &Address) -> Vec<AgentId> {
        let mut recipients = match address {
            Address::Direct { to } => vec![to.clone()],
            Address::Team { team } => self
                .board
                .team(team)
                .map(|t| t.members.clone())
                .unwrap_or_default(),
            Address::Broadcast => {
                let mut all = vec![AgentId::orchestrator()];
                all.extend(self.team_agent_ids());
                all
            }
        };
        recipients.retain(|r| r != sender);
        recipients
    }

    /// The quiescent-unfinished liveness predicate (ADR 0015, as amended
    /// by the #28 ruling: a pending judgment directive does not suppress
    /// the watchdog).
    fn liveness_predicate(&self) -> Option<(u32, u32)> {
        if self.finishing.is_some() || self.forced_tick || self.turns_in_flight > 0 {
            return None;
        }
        if self.orchestrator.in_flight {
            return None;
        }
        let all_idle_or_asleep = self
            .team
            .values()
            .all(|slot| matches!(slot.state, AgentState::Idle | AgentState::Asleep));
        if !all_idle_or_asleep {
            return None;
        }
        let board_open = self
            .board
            .tasks()
            .filter(|t| t.state == TaskState::Open)
            .count() as u32;
        let claimed_by_asleep = self
            .board
            .tasks()
            .filter(|t| match &t.state {
                TaskState::Claimed { by } => self
                    .team
                    .get(by)
                    .is_some_and(|s| s.state == AgentState::Asleep),
                _ => false,
            })
            .count() as u32;
        if board_open == 0 && claimed_by_asleep == 0 {
            return None;
        }
        // "No pending input" counts undelivered mailbox items (the next
        // tick drains them) but deliberately NOT a pending judgment
        // directive (#28, ADR 0015 amendment): an orchestrator that keeps
        // yielding on one would otherwise suppress the watchdog forever,
        // hiding a textbook deadlock behind the caps — the forced tick is
        // exactly the directive's resolve-or-decline chance.
        let orchestrator_quiet = self.mailboxes.depth(&AgentId::orchestrator()) == 0;
        if !orchestrator_quiet {
            return None;
        }
        Some((board_open, claimed_by_asleep))
    }

    /// Would the scheduler dispatch anything right now? A non-mutating
    /// mirror of [`plan_dispatches`]'s conditions (permits ignored — with no
    /// turn in flight at least one permit exists). The watchdog uses this to
    /// distinguish a genuine deadlock from a nudge the scheduler simply has
    /// not reacted to yet: "quiescent" means *nothing dispatchable*, not
    /// merely *nothing running* (ADR 0015).
    fn would_dispatch(&self) -> bool {
        if !self.orchestrator.in_flight {
            let orchestrator = AgentId::orchestrator();
            let queued_mail = self.mailboxes.depth(&orchestrator) > 0;
            if !self.first_tick_fired
                || self.forced_tick
                || queued_mail
                || self.has_new_events(self.orchestrator.watermark, &orchestrator)
            {
                return true;
            }
        }
        if self.metas.values().any(|slot| {
            !slot.in_flight && (slot.priority || slot.unobserved >= META_CADENCE_THRESHOLD)
        }) {
            return true;
        }
        self.team.iter().any(|(agent, slot)| {
            if slot.in_flight {
                return false;
            }
            match slot.state {
                AgentState::Working { .. } => true,
                AgentState::Asleep => false,
                AgentState::Idle => {
                    self.mailboxes.depth(agent) > 0
                        || (self.board.tasks().any(|t| {
                            t.state == TaskState::Open && self.board.is_eligible(agent, t)
                        }) && self.has_new_work_events(slot.watermark))
                }
            }
        })
    }

    /// Any event beyond `watermark` not sourced by `agent` (pins §5).
    fn has_new_events(&self, watermark: usize, agent: &AgentId) -> bool {
        self.events
            .iter()
            .skip(watermark)
            .any(|e| e.source.agent() != Some(agent))
    }

    /// Any event beyond `watermark` that can change an Idle agent's claim
    /// decision — board, team, or specialty shape. Deliberately excludes
    /// `turn_completed`: re-dispatching a decliner on another agent's no-op
    /// yield is the busy-spin ADR 0015 forbids (the stateless arc would
    /// decline again identically), while every real board change still
    /// counts as a fresh nudge.
    fn has_new_work_events(&self, watermark: usize) -> bool {
        self.events.iter().skip(watermark).any(|e| {
            matches!(
                e.kind,
                EventKind::TaskCreated { .. }
                    | EventKind::TaskClaimed { .. }
                    | EventKind::TaskReleased { .. }
                    | EventKind::TaskUnassigned { .. }
                    | EventKind::TaskCompleted { .. }
                    | EventKind::TaskCancelled { .. }
                    | EventKind::TeamFormed { .. }
                    | EventKind::TeamMembersSet { .. }
                    | EventKind::TeamDissolved { .. }
                    | EventKind::AgentRespecialized { .. }
                    | EventKind::AgentWoke { .. }
            )
        })
    }

    /// Force-terminate on a safety cap (ADR 0006/0022).
    fn trigger_cap(&mut self, cap: CapKind, limit: u64, observed: u64, stopping: &AtomicBool) {
        if self.finishing.is_some() {
            return;
        }
        self.append_event(
            EventSource::System,
            EventKind::CapHit {
                cap,
                limit,
                observed,
            },
        );
        self.append_event(
            EventSource::System,
            EventKind::RunFinished {
                reason: RunFinishReason::CapHit(cap),
                exit_code: 2,
            },
        );
        self.finishing = Some((RunFinishReason::CapHit(cap), 2));
        stopping.store(true, Ordering::SeqCst);
    }

    /// Fail the run on a harness fault after `run_started` (exit 1).
    fn trigger_harness_error(&mut self, message: String, stopping: &AtomicBool) {
        if self.finishing.is_some() {
            return;
        }
        tracing::error!(error = %message, "harness error — force-terminating the run");
        self.harness_error = Some(message);
        self.append_event(
            EventSource::System,
            EventKind::RunFinished {
                reason: RunFinishReason::HarnessError,
                exit_code: 1,
            },
        );
        self.finishing = Some((RunFinishReason::HarnessError, 1));
        stopping.store(true, Ordering::SeqCst);
    }
}

/// Immutable run-wide settings and handles shared across tasks.
struct Shared {
    world: Mutex<World>,
    registry: ToolRegistry,
    config: RunConfig,
    notify: Notify,
    /// Set on any termination path: in-flight turns abort at the next
    /// completion boundary.
    stopping: AtomicBool,
}

// ---- verb dispatch (ADR 0017) -------------------------------------------

impl ToolRegistry {
    /// Dispatch-by-name on the serial write path: unknown verb or args that
    /// fail typed deserialization are `invalid`; a domain-guard refusal is
    /// `rejected`; every world mutation emits its ADR 0022 event here.
    pub(crate) async fn dispatch(
        &self,
        role: Role,
        caller: &AgentId,
        call: &ToolCall,
        world: &mut World,
    ) -> ToolOutcome {
        let name = call.function.name.as_str();
        if !self.contains(role, name) {
            return ToolOutcome::unknown_verb(name);
        }
        let args = &call.function.arguments;
        macro_rules! parse {
            ($ty:ty) => {
                match serde_json::from_str::<$ty>(args) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        return ToolOutcome::invalid_arguments(format!(
                            "bad arguments for {name}: {error}"
                        ));
                    }
                }
            };
        }

        match name {
            "claim_task" => handle_claim(caller, parse!(tools::ClaimTaskArgs), world),
            "complete_task" => {
                handle_complete(caller, parse!(tools::CompleteTaskArgs), world).await
            }
            "release_task" => handle_release(caller, parse!(tools::ReleaseTaskArgs), world),
            "post_message" => {
                handle_post_message(caller, parse!(tools::PostMessageArgs), world).await
            }
            "write_knowledge" => {
                handle_write_knowledge(caller, parse!(tools::WriteKnowledgeArgs), world).await
            }
            "search_knowledge" => {
                handle_search_knowledge(parse!(tools::SearchKnowledgeArgs), world).await
            }
            "sleep" => handle_self_sleep(caller, parse!(tools::SleepArgs), world),
            "create_task" => handle_create_task(caller, parse!(tools::CreateTaskArgs), world),
            "cancel_task" => handle_cancel_task(caller, parse!(tools::CancelTaskArgs), world),
            "unassign_task" => handle_unassign(caller, parse!(tools::UnassignTaskArgs), world),
            "form_team" => handle_form_team(caller, parse!(tools::FormTeamArgs), world),
            "dissolve_team" => handle_dissolve_team(caller, parse!(tools::DissolveTeamArgs), world),
            "set_team_members" => {
                handle_set_team_members(caller, parse!(tools::SetTeamMembersArgs), world)
            }
            "respecialize" => handle_respecialize(caller, parse!(tools::RespecializeArgs), world),
            "sleep_agent" => match role {
                Role::MetaAgent => {
                    handle_meta_sleep_agent(caller, parse!(tools::SleepAgentArgs), world)
                }
                _ => handle_sleep_agent(caller, parse!(tools::SleepAgentArgs), world),
            },
            "wake_agent" => match role {
                Role::MetaAgent => {
                    handle_meta_wake_agent(caller, parse!(tools::WakeAgentArgs), world)
                }
                _ => handle_wake_agent(caller, parse!(tools::WakeAgentArgs), world),
            },
            "decline_directive" => {
                handle_decline_directive(caller, parse!(tools::DeclineDirectiveArgs), world)
            }
            "finish_run" => handle_finish_run(parse!(tools::FinishRunArgs), world),
            "set_parallelism" => {
                handle_set_parallelism(caller, parse!(tools::SetParallelismArgs), world)
            }
            "propose_respecialize" => handle_propose(
                caller,
                DirectiveKind::ProposeRespecialize,
                serde_json::from_str::<tools::ProposeRespecializeArgs>(args)
                    .map(|_| args)
                    .map_err(|e| e.to_string()),
                world,
            ),
            "propose_reallocate" => handle_propose(
                caller,
                DirectiveKind::ProposeReallocate,
                serde_json::from_str::<tools::ProposeReallocateArgs>(args)
                    .map(|_| args)
                    .map_err(|e| e.to_string()),
                world,
            ),
            "propose_rebalance" => handle_propose(
                caller,
                DirectiveKind::ProposeRebalance,
                serde_json::from_str::<tools::ProposeRebalanceArgs>(args)
                    .map(|_| args)
                    .map_err(|e| e.to_string()),
                world,
            ),
            _ => ToolOutcome::unknown_verb(name),
        }
    }
}

fn parse_team_agent(world: &World, handle: &str) -> Result<AgentId, ToolOutcome> {
    let agent = AgentId::parse(handle).map_err(|_| {
        ToolOutcome::rejected(
            "unknown_agent",
            format!("{handle:?} is not an agent handle"),
        )
    })?;
    if world.is_team_agent(&agent) {
        Ok(agent)
    } else {
        Err(ToolOutcome::rejected(
            "unknown_agent",
            format!("{agent} is not a pool team agent"),
        ))
    }
}

/// Validate an `in_response_to` cite BEFORE acting: must name a pending
/// judgment directive (ADR 0020).
fn validate_cite(world: &World, cite: Option<u64>) -> Result<Option<DirectiveId>, ToolOutcome> {
    let Some(raw) = cite else { return Ok(None) };
    let id = DirectiveId::new(raw);
    let Some(directive) = world.directives.iter().find(|d| d.id == id) else {
        return Err(ToolOutcome::rejected(
            "unknown_directive",
            format!("directive {id} does not exist"),
        ));
    };
    if directive.tier != DirectiveTier::Judgment {
        return Err(ToolOutcome::rejected(
            "directive_not_pending",
            format!("directive {id} is mechanical — nothing to fulfill"),
        ));
    }
    if !directive.is_pending() {
        return Err(ToolOutcome::rejected(
            "directive_not_pending",
            format!("directive {id} is already resolved"),
        ));
    }
    Ok(Some(id))
}

/// After a cited action succeeded: mark fulfilled and emit
/// `directive_fulfilled` (ADR 0020/0022).
fn fulfill_cite(world: &mut World, cite: Option<DirectiveId>, by: &AgentId) {
    let Some(id) = cite else { return };
    if let Some(directive) = world.directives.iter_mut().find(|d| d.id == id) {
        directive.state = DirectiveState::Fulfilled { by: by.clone() };
    }
    world.append_event(
        EventSource::Agent(by.clone()),
        EventKind::DirectiveFulfilled {
            directive: id,
            by: by.clone(),
        },
    );
}

fn handle_claim(caller: &AgentId, args: tools::ClaimTaskArgs, world: &mut World) -> ToolOutcome {
    let id = TaskId::new(args.task);
    match world.board.claim(caller, id) {
        Ok(task) => {
            let team = task.team.clone();
            if let Some(slot) = world.team.get_mut(caller) {
                slot.state = AgentState::Working { task: id };
                // The assignment boundary: reset the window, then record
                // the claim line itself (pins §5).
                slot.window.clear();
            }
            world.append_event(
                EventSource::Agent(caller.clone()),
                EventKind::TaskClaimed { task: id, team },
            );
            ToolOutcome::ok(serde_json::json!({ "task": id }))
        }
        Err(rejection) => rejection.into(),
    }
}

async fn handle_complete(
    caller: &AgentId,
    args: tools::CompleteTaskArgs,
    world: &mut World,
) -> ToolOutcome {
    // Validate the claim first so a failed complete never orphans a store
    // entry (the 3-id atomic step, ADR 0011/0014).
    let Some(task) = world.board.claimed_by(caller).map(|t| t.id) else {
        return ToolOutcome::rejected(
            "task_not_claimed",
            format!("{caller} has no claimed task to complete"),
        );
    };
    let planned = world.planned_event();
    let entry = match world
        .store
        .insert(
            &args.result,
            caller.clone(),
            planned,
            KnowledgeKind::TaskCompletion,
        )
        .await
    {
        Ok(entry) => entry,
        Err(error) => {
            return ToolOutcome::rejected(
                "internal_error",
                format!("knowledge ingest failed: {error}"),
            );
        }
    };
    match world.board.complete(caller, args.result.clone(), entry) {
        Ok(task_id) => {
            debug_assert_eq!(task, task_id);
            if let Some(slot) = world.team.get_mut(caller) {
                slot.state = AgentState::Idle;
            }
            world.append_event(
                EventSource::Agent(caller.clone()),
                EventKind::TaskCompleted {
                    task: task_id,
                    result: args.result,
                    result_ref: entry,
                },
            );
            ToolOutcome::ok(serde_json::json!({ "task": task_id, "result_ref": entry }))
        }
        Err(rejection) => rejection.into(),
    }
}

fn handle_release(
    caller: &AgentId,
    args: tools::ReleaseTaskArgs,
    world: &mut World,
) -> ToolOutcome {
    match world.board.release(caller) {
        Ok(task) => {
            if let Some(slot) = world.team.get_mut(caller) {
                slot.state = AgentState::Idle;
            }
            world.append_event(
                EventSource::Agent(caller.clone()),
                EventKind::TaskReleased {
                    task,
                    reason: args.reason,
                },
            );
            ToolOutcome::ok(serde_json::json!({ "task": task }))
        }
        Err(rejection) => rejection.into(),
    }
}

async fn handle_post_message(
    caller: &AgentId,
    args: tools::PostMessageArgs,
    world: &mut World,
) -> ToolOutcome {
    // Exactly one address form (pins §1).
    let broadcast = args.broadcast.unwrap_or(false);
    let forms =
        usize::from(args.to.is_some()) + usize::from(args.team.is_some()) + usize::from(broadcast);
    if forms != 1 {
        return ToolOutcome::rejected(
            "invalid_address",
            "exactly one of to / team / broadcast:true must be set",
        );
    }
    let address = if let Some(to) = &args.to {
        let Ok(agent) = AgentId::parse(to) else {
            return ToolOutcome::rejected(
                "unknown_agent",
                format!("{to:?} is not an agent handle"),
            );
        };
        if agent.role() == Role::MetaAgent {
            return ToolOutcome::rejected(
                "invalid_address",
                "meta-agents observe via events and receive no messages",
            );
        }
        if agent.role() == Role::TeamAgent && !world.is_team_agent(&agent) {
            return ToolOutcome::rejected(
                "unknown_agent",
                format!("{agent} is not a pool team agent"),
            );
        }
        Address::Direct { to: agent }
    } else if let Some(team) = &args.team {
        let Ok(team) = TeamId::parse(team) else {
            return ToolOutcome::rejected("unknown_team", format!("{team:?} is not a team id"));
        };
        let live = world.board.team(&team).is_some_and(|t| !t.dissolved);
        if !live {
            return ToolOutcome::rejected(
                "unknown_team",
                format!("team {team} is not a live team"),
            );
        }
        Address::Team { team }
    } else {
        Address::Broadcast
    };

    let recipients = world.recipients_for(caller, &address);
    let id = MessageId::new(world.next_message);
    let planned = world.planned_event();
    let entry = match world
        .store
        .insert(&args.body, caller.clone(), planned, KnowledgeKind::Message)
        .await
    {
        Ok(entry) => entry,
        Err(error) => {
            return ToolOutcome::rejected(
                "internal_error",
                format!("knowledge ingest failed: {error}"),
            );
        }
    };
    world.next_message += 1;
    let message = Message {
        id,
        sender: caller.clone(),
        address: address.clone(),
        body: args.body.clone(),
        knowledge_ref: entry,
    };
    world.messages.insert(id, message);
    world.append_event(
        EventSource::Agent(caller.clone()),
        EventKind::MessageSent {
            message: id,
            address,
            body: args.body,
            knowledge_ref: entry,
        },
    );
    world.mailboxes.push_for_recipients(recipients, id);
    ToolOutcome::ok(serde_json::json!({ "message": id }))
}

async fn handle_write_knowledge(
    caller: &AgentId,
    args: tools::WriteKnowledgeArgs,
    world: &mut World,
) -> ToolOutcome {
    let planned = world.planned_event();
    let entry = match world
        .store
        .insert(&args.text, caller.clone(), planned, KnowledgeKind::Note)
        .await
    {
        Ok(entry) => entry,
        Err(error) => {
            return ToolOutcome::rejected(
                "internal_error",
                format!("knowledge ingest failed: {error}"),
            );
        }
    };
    world.append_event(
        EventSource::Agent(caller.clone()),
        EventKind::KnowledgeWritten {
            entry,
            text: args.text,
        },
    );
    ToolOutcome::ok(serde_json::json!({ "entry": entry }))
}

async fn handle_search_knowledge(
    args: tools::SearchKnowledgeArgs,
    world: &mut World,
) -> ToolOutcome {
    let k = args.k.unwrap_or(3).clamp(1, 10) as usize;
    match world.store.search(&args.query, k).await {
        Ok(hits) => {
            let hits: Vec<serde_json::Value> = hits
                .iter()
                .map(|hit| {
                    serde_json::json!({
                        "entry": hit.entry.id,
                        "score": (f64::from(hit.score) * 100.0).round() / 100.0,
                        "kind": hit.entry.kind,
                        "author": hit.entry.author,
                        "text": hit.entry.text,
                    })
                })
                .collect();
            ToolOutcome::ok(serde_json::json!({ "hits": hits }))
        }
        Err(error) => ToolOutcome::rejected("internal_error", format!("search failed: {error}")),
    }
}

fn handle_self_sleep(caller: &AgentId, _args: tools::SleepArgs, world: &mut World) -> ToolOutcome {
    let Some(slot) = world.team.get_mut(caller) else {
        return ToolOutcome::rejected("unknown_agent", format!("{caller} is not a team agent"));
    };
    if slot.state != AgentState::Idle {
        return ToolOutcome::rejected(
            "not_idle",
            format!("{caller} is not Idle — sleep is legal only from Idle (ADR 0015)"),
        );
    }
    slot.state = AgentState::Asleep;
    world.append_event(
        EventSource::Agent(caller.clone()),
        EventKind::AgentSlept {
            agent: caller.clone(),
            via_directive: None,
        },
    );
    ToolOutcome::ok(serde_json::json!({ "agent": caller, "state": "Asleep" }))
}

fn handle_create_task(
    caller: &AgentId,
    args: tools::CreateTaskArgs,
    world: &mut World,
) -> ToolOutcome {
    let team = match &args.team {
        Some(tag) => match TeamId::parse(tag) {
            Ok(team) => Some(team),
            Err(_) => {
                return ToolOutcome::rejected("unknown_team", format!("{tag:?} is not a team id"));
            }
        },
        None => None,
    };
    let id = TaskId::new(world.next_task);
    let planned = world.planned_event();
    match world.board.create_task(
        id,
        args.title.clone(),
        args.description.clone(),
        caller.clone(),
        planned,
        team.clone(),
    ) {
        Ok(_) => {
            world.next_task += 1;
            world.append_event(
                EventSource::Agent(caller.clone()),
                EventKind::TaskCreated {
                    task: id,
                    title: args.title,
                    description: args.description,
                    team,
                },
            );
            ToolOutcome::ok(serde_json::json!({ "task": id }))
        }
        Err(rejection) => rejection.into(),
    }
}

fn handle_cancel_task(
    caller: &AgentId,
    args: tools::CancelTaskArgs,
    world: &mut World,
) -> ToolOutcome {
    let id = TaskId::new(args.task);
    match world.board.cancel(id, args.reason.clone()) {
        Ok(()) => {
            world.append_event(
                EventSource::Agent(caller.clone()),
                EventKind::TaskCancelled {
                    task: id,
                    reason: args.reason,
                },
            );
            ToolOutcome::ok(serde_json::json!({ "task": id }))
        }
        Err(rejection) => rejection.into(),
    }
}

fn handle_unassign(
    caller: &AgentId,
    args: tools::UnassignTaskArgs,
    world: &mut World,
) -> ToolOutcome {
    let cite = match validate_cite(world, args.in_response_to) {
        Ok(cite) => cite,
        Err(outcome) => return outcome,
    };
    let id = TaskId::new(args.task);
    match world.board.unassign(id) {
        Ok(prev_claimant) => {
            if let Some(slot) = world.team.get_mut(&prev_claimant)
                && slot.state == (AgentState::Working { task: id })
            {
                slot.state = AgentState::Idle;
            }
            world.append_event(
                EventSource::Agent(caller.clone()),
                EventKind::TaskUnassigned {
                    task: id,
                    prev_claimant: prev_claimant.clone(),
                    reason: args.reason,
                    via_directive: cite,
                },
            );
            fulfill_cite(world, cite, caller);
            ToolOutcome::ok(serde_json::json!({ "task": id, "prev_claimant": prev_claimant }))
        }
        Err(rejection) => rejection.into(),
    }
}

fn handle_form_team(caller: &AgentId, args: tools::FormTeamArgs, world: &mut World) -> ToolOutcome {
    let Ok(team) = TeamId::parse(&args.team) else {
        return ToolOutcome::rejected("unknown_team", format!("{:?} is not a team id", args.team));
    };
    let mut members = Vec::with_capacity(args.members.len());
    for handle in &args.members {
        match parse_team_agent(world, handle) {
            Ok(agent) => members.push(agent),
            Err(outcome) => return outcome,
        }
    }
    match world.board.form_team(team.clone(), members.clone()) {
        Ok(_) => {
            world.append_event(
                EventSource::Agent(caller.clone()),
                EventKind::TeamFormed {
                    team: team.clone(),
                    members,
                },
            );
            ToolOutcome::ok(serde_json::json!({ "team": team }))
        }
        Err(rejection) => rejection.into(),
    }
}

fn handle_dissolve_team(
    caller: &AgentId,
    args: tools::DissolveTeamArgs,
    world: &mut World,
) -> ToolOutcome {
    let Ok(team) = TeamId::parse(&args.team) else {
        return ToolOutcome::rejected("unknown_team", format!("{:?} is not a team id", args.team));
    };
    match world.board.dissolve_team(&team) {
        Ok(()) => {
            world.append_event(
                EventSource::Agent(caller.clone()),
                EventKind::TeamDissolved { team: team.clone() },
            );
            ToolOutcome::ok(serde_json::json!({ "team": team }))
        }
        Err(rejection) => rejection.into(),
    }
}

fn handle_set_team_members(
    caller: &AgentId,
    args: tools::SetTeamMembersArgs,
    world: &mut World,
) -> ToolOutcome {
    let cite = match validate_cite(world, args.in_response_to) {
        Ok(cite) => cite,
        Err(outcome) => return outcome,
    };
    let Ok(team) = TeamId::parse(&args.team) else {
        return ToolOutcome::rejected("unknown_team", format!("{:?} is not a team id", args.team));
    };
    let mut members = Vec::with_capacity(args.members.len());
    for handle in &args.members {
        match parse_team_agent(world, handle) {
            Ok(agent) => members.push(agent),
            Err(outcome) => return outcome,
        }
    }
    match world.board.set_team_members(&team, members.clone()) {
        Ok(delta) => {
            world.append_event(
                EventSource::Agent(caller.clone()),
                EventKind::TeamMembersSet {
                    team: team.clone(),
                    members,
                    added: delta.added,
                    removed: delta.removed,
                    via_directive: cite,
                },
            );
            fulfill_cite(world, cite, caller);
            ToolOutcome::ok(serde_json::json!({ "team": team }))
        }
        Err(rejection) => rejection.into(),
    }
}

fn handle_respecialize(
    caller: &AgentId,
    args: tools::RespecializeArgs,
    world: &mut World,
) -> ToolOutcome {
    let cite = match validate_cite(world, args.in_response_to) {
        Ok(cite) => cite,
        Err(outcome) => return outcome,
    };
    let agent = match parse_team_agent(world, &args.agent) {
        Ok(agent) => agent,
        Err(outcome) => return outcome,
    };
    let Ok(slug) = SpecialtySlug::parse(&args.specialty.name) else {
        return ToolOutcome::rejected(
            "invalid_slug",
            format!("{:?} is not a valid specialty slug", args.specialty.name),
        );
    };
    let Some(slot) = world.team.get_mut(&agent) else {
        return ToolOutcome::rejected("unknown_agent", format!("{agent} is not a team agent"));
    };
    // Respecializing a non-idle or in-flight agent is illegal (ADR 0003).
    if slot.state != AgentState::Idle || slot.in_flight {
        return ToolOutcome::rejected(
            "not_idle",
            format!("{agent} is not Idle and settled — unassign or park first (ADR 0003)"),
        );
    }
    let from = slot.profile.slug.clone();
    slot.profile = SpecialtyProfile {
        slug: slug.clone(),
        description: args.specialty.description,
        focus: args.specialty.focus,
    };
    slot.window.clear();
    slot.channel.set_specialty(slug.clone());
    world.append_event(
        EventSource::Agent(caller.clone()),
        EventKind::AgentRespecialized {
            agent: agent.clone(),
            from: from.clone(),
            to: slug.clone(),
            via_directive: cite,
        },
    );
    fulfill_cite(world, cite, caller);
    ToolOutcome::ok(serde_json::json!({ "agent": agent, "from": from, "to": slug }))
}

/// The shared sleep guard: legal only from Idle and not mid-turn (ADR 0015).
fn sleep_guard(world: &mut World, target: &AgentId) -> Result<(), ToolOutcome> {
    let Some(slot) = world.team.get(target) else {
        return Err(ToolOutcome::rejected(
            "unknown_agent",
            format!("{target} is not a team agent"),
        ));
    };
    if slot.state != AgentState::Idle || slot.in_flight {
        return Err(ToolOutcome::rejected(
            "not_idle",
            format!("{target} is not Idle — sleep is legal only from Idle (ADR 0015)"),
        ));
    }
    Ok(())
}

fn handle_sleep_agent(
    caller: &AgentId,
    args: tools::SleepAgentArgs,
    world: &mut World,
) -> ToolOutcome {
    let target = match parse_team_agent(world, &args.agent) {
        Ok(agent) => agent,
        Err(outcome) => return outcome,
    };
    if let Err(outcome) = sleep_guard(world, &target) {
        return outcome;
    }
    if let Some(slot) = world.team.get_mut(&target) {
        slot.state = AgentState::Asleep;
    }
    world.append_event(
        EventSource::Agent(caller.clone()),
        EventKind::AgentSlept {
            agent: target.clone(),
            via_directive: None,
        },
    );
    ToolOutcome::ok(serde_json::json!({ "agent": target, "state": "Asleep" }))
}

/// The shared wake transition: Asleep → Working with its still-claimed
/// task, else Idle; the consecutive-malformed counter resets (recovery).
fn wake_transition(world: &mut World, target: &AgentId) -> Result<RestoredState, ToolOutcome> {
    let restored = {
        let Some(slot) = world.team.get(target) else {
            return Err(ToolOutcome::rejected(
                "unknown_agent",
                format!("{target} is not a team agent"),
            ));
        };
        if slot.state != AgentState::Asleep {
            return Err(ToolOutcome::rejected(
                "not_asleep",
                format!("{target} is not Asleep — wake is legal only from Asleep (ADR 0015)"),
            ));
        }
        match world.board.claimed_by(target) {
            Some(task) => RestoredState::Working { task: task.id },
            None => RestoredState::Idle,
        }
    };
    if let Some(slot) = world.team.get_mut(target) {
        slot.state = match restored {
            RestoredState::Working { task } => AgentState::Working { task },
            RestoredState::Idle => AgentState::Idle,
        };
        slot.consecutive_malformed = 0;
    }
    Ok(restored)
}

fn handle_wake_agent(
    caller: &AgentId,
    args: tools::WakeAgentArgs,
    world: &mut World,
) -> ToolOutcome {
    let target = match parse_team_agent(world, &args.agent) {
        Ok(agent) => agent,
        Err(outcome) => return outcome,
    };
    let restored = match wake_transition(world, &target) {
        Ok(restored) => restored,
        Err(outcome) => return outcome,
    };
    world.append_event(
        EventSource::Agent(caller.clone()),
        EventKind::AgentWoke {
            agent: target.clone(),
            restored,
            via_directive: None,
        },
    );
    ToolOutcome::ok(serde_json::json!({ "agent": target, "restored": restored }))
}

/// Issue a directive record + `directive_issued` event (both tiers,
/// ADR 0020/0022).
fn issue_directive(
    world: &mut World,
    from: &AgentId,
    tier: DirectiveTier,
    kind: DirectiveKind,
    args: serde_json::Value,
    state: DirectiveState,
) -> DirectiveId {
    let id = DirectiveId::new(world.next_directive);
    world.next_directive += 1;
    world.directives.push(Directive {
        id,
        tier,
        kind,
        args: args.clone(),
        from: from.clone(),
        state,
    });
    world.append_event(
        EventSource::Agent(from.clone()),
        EventKind::DirectiveIssued {
            directive: id,
            tier,
            kind,
            args,
        },
    );
    id
}

fn args_value(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or(serde_json::Value::Null)
}

/// Meta mechanical `sleep_agent`: guard FIRST — a refused guard emits no
/// directive at all, keeping mechanical-issued ⟹ applied exact (ADR 0022).
fn handle_meta_sleep_agent(
    caller: &AgentId,
    args: tools::SleepAgentArgs,
    world: &mut World,
) -> ToolOutcome {
    let target = match parse_team_agent(world, &args.agent) {
        Ok(agent) => agent,
        Err(outcome) => return outcome,
    };
    if let Err(outcome) = sleep_guard(world, &target) {
        return outcome;
    }
    let directive = issue_directive(
        world,
        caller,
        DirectiveTier::Mechanical,
        DirectiveKind::SleepAgent,
        serde_json::json!({ "agent": target }),
        DirectiveState::Fulfilled { by: caller.clone() },
    );
    if let Some(slot) = world.team.get_mut(&target) {
        slot.state = AgentState::Asleep;
    }
    world.append_event(
        EventSource::Agent(caller.clone()),
        EventKind::AgentSlept {
            agent: target.clone(),
            via_directive: Some(directive),
        },
    );
    ToolOutcome::ok(serde_json::json!({ "applied": true, "agent": target }))
}

fn handle_meta_wake_agent(
    caller: &AgentId,
    args: tools::WakeAgentArgs,
    world: &mut World,
) -> ToolOutcome {
    let target = match parse_team_agent(world, &args.agent) {
        Ok(agent) => agent,
        Err(outcome) => return outcome,
    };
    // Guard first (peek) so a refusal emits no directive.
    {
        let Some(slot) = world.team.get(&target) else {
            return ToolOutcome::rejected("unknown_agent", format!("{target} is unknown"));
        };
        if slot.state != AgentState::Asleep {
            return ToolOutcome::rejected(
                "not_asleep",
                format!("{target} is not Asleep — wake is legal only from Asleep (ADR 0015)"),
            );
        }
    }
    let directive = issue_directive(
        world,
        caller,
        DirectiveTier::Mechanical,
        DirectiveKind::WakeAgent,
        serde_json::json!({ "agent": target }),
        DirectiveState::Fulfilled { by: caller.clone() },
    );
    let restored = match wake_transition(world, &target) {
        Ok(restored) => restored,
        Err(outcome) => return outcome,
    };
    world.append_event(
        EventSource::Agent(caller.clone()),
        EventKind::AgentWoke {
            agent: target.clone(),
            restored,
            via_directive: Some(directive),
        },
    );
    ToolOutcome::ok(serde_json::json!({ "applied": true, "restored": restored }))
}

fn handle_set_parallelism(
    caller: &AgentId,
    args: tools::SetParallelismArgs,
    world: &mut World,
) -> ToolOutcome {
    // Clamp to [1, --parallel] (ADR 0020).
    let requested = args.target;
    let target = (requested as usize).clamp(1, world.configured_parallelism);
    let directive = issue_directive(
        world,
        caller,
        DirectiveTier::Mechanical,
        DirectiveKind::SetParallelism,
        serde_json::json!({ "target": requested }),
        DirectiveState::Fulfilled { by: caller.clone() },
    );
    let current = world.effective_parallelism;
    if target > current {
        world.semaphore.add_permits(target - current);
    } else if target < current {
        let want = current - target;
        let forgotten = world.semaphore.forget_permits(want);
        world.permit_debt += want - forgotten;
    }
    world.effective_parallelism = target;
    world.append_event(
        EventSource::Agent(caller.clone()),
        EventKind::ParallelismChanged {
            requested,
            effective: target as u32,
            via_directive: directive,
        },
    );
    ToolOutcome::ok(serde_json::json!({ "applied": true, "effective": target }))
}

fn handle_propose(
    caller: &AgentId,
    kind: DirectiveKind,
    parsed: Result<&String, String>,
    world: &mut World,
) -> ToolOutcome {
    let raw = match parsed {
        Ok(raw) => raw,
        Err(error) => {
            return ToolOutcome::invalid_arguments(format!("bad arguments for {kind}: {error}"));
        }
    };
    let directive = issue_directive(
        world,
        caller,
        DirectiveTier::Judgment,
        kind,
        args_value(raw),
        DirectiveState::Pending,
    );
    ToolOutcome::ok(serde_json::json!({ "directive_id": directive }))
}

fn handle_decline_directive(
    caller: &AgentId,
    args: tools::DeclineDirectiveArgs,
    world: &mut World,
) -> ToolOutcome {
    let id = DirectiveId::new(args.directive);
    let Some(directive) = world.directives.iter_mut().find(|d| d.id == id) else {
        return ToolOutcome::rejected(
            "unknown_directive",
            format!("directive {id} does not exist"),
        );
    };
    if directive.tier != DirectiveTier::Judgment || !directive.is_pending() {
        return ToolOutcome::rejected(
            "directive_not_pending",
            format!("directive {id} is not a pending judgment directive"),
        );
    }
    directive.state = DirectiveState::Declined {
        by: caller.clone(),
        reason: args.reason.clone(),
    };
    let kind = directive.kind;
    world.append_event(
        EventSource::Agent(caller.clone()),
        EventKind::DirectiveDeclined {
            directive: id,
            kind,
            reason: args.reason,
            by: caller.clone(),
        },
    );
    ToolOutcome::ok(serde_json::json!({ "directive": id }))
}

fn handle_finish_run(args: tools::FinishRunArgs, world: &mut World) -> ToolOutcome {
    let blockers = world.board.finish_blockers();
    if !blockers.is_empty() {
        let list = blockers
            .iter()
            .map(|t| format!("task {} [{}]", t.id, t.state.digest_label()))
            .collect::<Vec<_>>()
            .join(", ");
        return ToolOutcome::rejected(
            "blockers",
            format!("cannot finish: non-terminal tasks remain — {list}"),
        );
    }
    world.report = Some(args.report);
    world.finish_requested = true;
    ToolOutcome::ok(serde_json::json!({ "finishing": true }))
}

// ---- turn execution (ADR 0015) ------------------------------------------

/// A compact `<args-gist>` for the recent-activity line (pins §3) — the
/// mock reads only the verb name and the outcome.
fn call_gist(call: &ToolCall) -> String {
    let args: serde_json::Value =
        serde_json::from_str(&call.function.arguments).unwrap_or(serde_json::Value::Null);
    let quote_prefix = |text: &str| {
        let prefix: String = text.chars().take(24).collect();
        if text.chars().count() > 24 {
            format!("\"{prefix}…\"")
        } else {
            format!("\"{prefix}\"")
        }
    };
    match call.function.name.as_str() {
        "claim_task" => format!(
            "task:{}",
            args.get("task").and_then(|v| v.as_u64()).unwrap_or(0)
        ),
        "complete_task" => args
            .get("result")
            .and_then(|v| v.as_str())
            .map(quote_prefix)
            .unwrap_or_default(),
        "write_knowledge" => args
            .get("text")
            .and_then(|v| v.as_str())
            .map(quote_prefix)
            .unwrap_or_default(),
        "search_knowledge" => args
            .get("query")
            .and_then(|v| v.as_str())
            .map(quote_prefix)
            .unwrap_or_default(),
        "post_message" => {
            if let Some(to) = args.get("to").and_then(|v| v.as_str()) {
                format!("to:{to}")
            } else if let Some(team) = args.get("team").and_then(|v| v.as_str()) {
                format!("team:{team}")
            } else {
                "broadcast".into()
            }
        }
        "release_task" => String::new(),
        _ => String::new(),
    }
}

/// One reasoning episode (CONTEXT.md: Turn): context assembled once, then
/// the capped completion↔tool inner loop, then the locked finalize step.
///
/// The `--parallel` permit is released BEFORE the final scheduler nudge —
/// a nudge fired while the permit was still held could be consumed by a
/// scheduler pass that fails `try_acquire` and then waits forever (the
/// lost-wakeup the dry-stall test caught).
async fn run_turn(
    shared: Arc<Shared>,
    agent: AgentId,
    role: Role,
    permit: Option<OwnedSemaphorePermit>,
) {
    turn_body(shared.clone(), agent, role).await;
    drop(permit);
    shared.notify.notify_one();
}

async fn turn_body(shared: Arc<Shared>, agent: AgentId, role: Role) {
    let (mut request, channel, first_call_seq, turn_index) = {
        let mut world = shared.world.lock().await;
        if shared.stopping.load(Ordering::SeqCst) || world.finishing.is_some() {
            end_turn_bookkeeping(&mut world, &agent, role);
            shared.notify.notify_one();
            return;
        }

        // Build the view under the write-path lock.
        let view = build_view(&world, &agent, role).await;
        let policy = ContextPolicy::for_role(role, shared.config.assembly_budget);
        let specialty = world.team.get(&agent).map(|s| s.profile.clone());
        let prompt: AssembledPrompt = assemble(
            &policy,
            role,
            specialty.as_ref(),
            &view,
            &openteam_wire::CharCountTokenizer,
        );
        if prompt.drained > 0 {
            let delivered = world.mailboxes.drain(&agent, prompt.drained);
            world.append_event(
                EventSource::Agent(agent.clone()),
                EventKind::MessagesDelivered { delivered },
            );
        }
        if !prompt.degraded.is_empty() {
            world.append_event(
                EventSource::Agent(agent.clone()),
                EventKind::ContextDegraded {
                    agent: agent.clone(),
                    sections: prompt.degraded.clone(),
                },
            );
        }

        let channel = match role {
            Role::Orchestrator => Some(world.orchestrator.channel.clone()),
            Role::MetaAgent => world.metas.get(&agent).map(|s| s.channel.clone()),
            Role::TeamAgent => world.team.get(&agent).map(|s| s.channel.clone()),
        };
        let Some(channel) = channel else {
            end_turn_bookkeeping(&mut world, &agent, role);
            shared.notify.notify_one();
            return;
        };

        let turn_index = match role {
            Role::Orchestrator => {
                world.orchestrator.turn_index += 1;
                world.orchestrator.turn_index
            }
            Role::MetaAgent => world
                .metas
                .get_mut(&agent)
                .map(|s| {
                    s.turn_index += 1;
                    s.turn_index
                })
                .unwrap_or(0),
            Role::TeamAgent => world
                .team
                .get_mut(&agent)
                .map(|s| {
                    s.turn_index += 1;
                    s.turn_index
                })
                .unwrap_or(0),
        };

        // The edge-trigger watermark is taken at ASSEMBLY time, not turn
        // end: an event that lands while this turn is in flight was not in
        // the assembled view, so it must stay fresh for the next dispatch
        // decision — otherwise a task completing during the orchestrator's
        // final yield tick would be swallowed and `finish_run` never fires
        // (the hang the meta-directive fixture caught).
        let assembled_at = world.events.len();
        match role {
            Role::Orchestrator => world.orchestrator.watermark = assembled_at,
            Role::MetaAgent => {
                if let Some(slot) = world.metas.get_mut(&agent) {
                    slot.watermark = assembled_at;
                }
            }
            Role::TeamAgent => {
                if let Some(slot) = world.team.get_mut(&agent) {
                    slot.watermark = assembled_at;
                }
            }
        }

        let request = ChatCompletionRequest {
            model: shared.config.model.clone(),
            messages: vec![
                ChatMessage::System {
                    content: MessageContent::Text(prompt.system),
                    name: None,
                },
                ChatMessage::User {
                    content: MessageContent::Text(prompt.user),
                    name: None,
                },
            ],
            tools: Some(shared.registry.tool_defs(role).to_vec()),
            tool_choice: Some(ToolChoice::Mode(ToolChoiceMode::Auto)),
            parallel_tool_calls: Some(role != Role::MetaAgent),
            user: None,
            safety_identifier: None,
            prompt_cache_key: None,
            stream: None,
            n: None,
        };
        let first = channel.next_call_seq();
        (request, channel, first, turn_index)
    };

    let mut usage = TurnUsage {
        prompt: 0,
        completion: 0,
        total: 0,
    };
    let mut tool_iters: u32 = 0;
    let mut completions: u64 = 0;
    let mut any_success = false;
    let mut all_invalid = true;
    let mut finish_hit = false;

    let outcome = loop {
        if shared.stopping.load(Ordering::SeqCst) {
            let mut world = shared.world.lock().await;
            end_turn_bookkeeping(&mut world, &agent, role);
            shared.notify.notify_one();
            return;
        }
        // The max-llm-calls cap gates each completion (ADR 0006).
        {
            let mut world = shared.world.lock().await;
            if let Some(max) = shared.config.max_llm_calls {
                let observed = world.llm_calls;
                if observed >= max {
                    world.trigger_cap(CapKind::MaxLlmCalls, max, observed, &shared.stopping);
                    end_turn_bookkeeping(&mut world, &agent, role);
                    shared.notify.notify_one();
                    return;
                }
            }
            world.llm_calls += 1;
        }

        let (_call_seq, response) = match channel.complete(&request).await {
            Ok(pair) => pair,
            Err(error) => {
                let mut world = shared.world.lock().await;
                world.trigger_harness_error(
                    format!("llm completion failed for {agent}: {error}"),
                    &shared.stopping,
                );
                end_turn_bookkeeping(&mut world, &agent, role);
                shared.notify.notify_one();
                return;
            }
        };
        completions += 1;
        usage.prompt += response.usage.prompt_tokens;
        usage.completion += response.usage.completion_tokens;
        usage.total += response.usage.total_tokens;

        let Some(choice) = response.choices.into_iter().next() else {
            let mut world = shared.world.lock().await;
            world.trigger_harness_error(
                format!("llm returned no choices for {agent}"),
                &shared.stopping,
            );
            end_turn_bookkeeping(&mut world, &agent, role);
            shared.notify.notify_one();
            return;
        };
        let message = choice.message;
        let calls = message.tool_calls.clone().unwrap_or_default();
        if calls.is_empty() {
            break TurnOutcome::Yielded;
        }
        tool_iters += 1;

        // Dispatch the batch in array order on the serial write path.
        let outcomes: Vec<ToolOutcome> = {
            let mut world = shared.world.lock().await;
            if world.finishing.is_some() {
                end_turn_bookkeeping(&mut world, &agent, role);
                shared.notify.notify_one();
                return;
            }
            let mut outcomes = Vec::with_capacity(calls.len());
            for call in &calls {
                let call_outcome = shared
                    .registry
                    .dispatch(role, &agent, call, &mut world)
                    .await;
                tracing::debug!(
                    agent = %agent,
                    verb = %call.function.name,
                    outcome = call_outcome.word(),
                    "dispatched coordination verb"
                );
                if role == Role::TeamAgent
                    && let Some(slot) = world.team.get_mut(&agent)
                {
                    slot.window.push(window_line(
                        turn_index,
                        &call.function.name,
                        &call_gist(call),
                        call_outcome.word(),
                    ));
                }
                outcomes.push(call_outcome);
            }
            outcomes
        };
        if role == Role::Orchestrator
            && calls.iter().zip(&outcomes).any(|(call, call_outcome)| {
                call.function.name == "finish_run" && matches!(call_outcome, ToolOutcome::Ok { .. })
            })
        {
            finish_hit = true;
        }

        for call_outcome in &outcomes {
            if !call_outcome.is_invalid() {
                any_success = true;
                all_invalid = false;
            }
        }

        // Feed one `role:"tool"` reply per `tool_call_id` (ADR 0015/0013).
        request.messages.push(ChatMessage::Assistant {
            content: message.content.clone().map(MessageContent::Text),
            tool_calls: Some(calls.clone()),
            refusal: None,
            name: None,
        });
        for (call, call_outcome) in calls.iter().zip(&outcomes) {
            request.messages.push(ChatMessage::Tool {
                content: MessageContent::Text(call_outcome.to_content()),
                tool_call_id: call.id.clone(),
            });
        }

        if finish_hit {
            // A successful finish_run short-circuits the inner loop
            // (pins §5): the turn ends without a further completion.
            break TurnOutcome::Yielded;
        }
        if tool_iters >= shared.config.max_tool_iters {
            break TurnOutcome::ToolIterCap;
        }
    };
    let last_call_seq = first_call_seq + completions.saturating_sub(1);

    // Locked finalize: turn_completed, malformed/park bookkeeping, state.
    {
        let mut world = shared.world.lock().await;
        let malformed = tool_iters > 0 && all_invalid;
        if world.finishing.is_none() {
            let on_task = if role == Role::TeamAgent {
                world.board.claimed_by(&agent).map(|t| t.id)
            } else {
                None
            };
            world.append_event(
                EventSource::Agent(agent.clone()),
                EventKind::TurnCompleted {
                    first_call_seq,
                    last_call_seq,
                    tool_iters,
                    outcome,
                    malformed,
                    usage,
                    on_task,
                },
            );
            if role == Role::Orchestrator {
                world.ticks += 1;
                let ticks = world.ticks;
                if let Some(max) = shared.config.max_ticks
                    && ticks >= max
                    && !world.finish_requested
                {
                    world.trigger_cap(CapKind::MaxTicks, max, ticks, &shared.stopping);
                }
            }
            if role == Role::TeamAgent {
                if malformed {
                    let park = {
                        let slot = world.team.get_mut(&agent);
                        slot.map(|slot| {
                            slot.consecutive_malformed += 1;
                            if slot.consecutive_malformed >= MALFORMED_PARK_K {
                                slot.state = AgentState::Asleep;
                                Some(slot.consecutive_malformed)
                            } else {
                                None
                            }
                        })
                    };
                    if let Some(Some(count)) = park {
                        // Park preserves the claimed task (ADR 0015).
                        world.append_event(
                            EventSource::Agent(agent.clone()),
                            EventKind::AgentParked {
                                agent: agent.clone(),
                                count,
                            },
                        );
                    }
                } else if any_success && let Some(slot) = world.team.get_mut(&agent) {
                    slot.consecutive_malformed = 0;
                }
            }
            if finish_hit && world.finish_requested && world.finishing.is_none() {
                world.append_event(
                    EventSource::Agent(AgentId::orchestrator()),
                    EventKind::RunFinished {
                        reason: RunFinishReason::CleanFinish,
                        exit_code: 0,
                    },
                );
                world.finishing = Some((RunFinishReason::CleanFinish, 0));
                shared.stopping.store(true, Ordering::SeqCst);
            }
        }
        if let Some(error) = world.io_error.take() {
            world.trigger_harness_error(
                format!("events.jsonl write failed: {error}"),
                &shared.stopping,
            );
        }
        end_turn_bookkeeping(&mut world, &agent, role);
    }
    shared.notify.notify_one();
}

/// Clear the in-flight flag and decrement the gauge. The watermark is NOT
/// touched here — it was taken at assembly time, so events that landed
/// while the turn was in flight stay fresh for the next dispatch decision.
fn end_turn_bookkeeping(world: &mut World, agent: &AgentId, role: Role) {
    match role {
        Role::Orchestrator => world.orchestrator.in_flight = false,
        Role::MetaAgent => {
            if let Some(slot) = world.metas.get_mut(agent) {
                slot.in_flight = false;
            }
        }
        Role::TeamAgent => {
            if let Some(slot) = world.team.get_mut(agent) {
                slot.in_flight = false;
            }
        }
    }
    world.turns_in_flight = world.turns_in_flight.saturating_sub(1);
}

/// Build the per-role [`AssembleView`] under the write-path lock.
async fn build_view(world: &World, agent: &AgentId, role: Role) -> AssembleView {
    let mut view = AssembleView {
        goal: world.goal.clone(),
        ..AssembleView::default()
    };

    match role {
        Role::Orchestrator => {
            view.board_lines = world.board.digest_lines(None);
            view.run_health = Some(world.metrics.run_health_line());
            view.directive_lines = world
                .directives
                .iter()
                .filter(|d| d.is_pending())
                .map(Directive::directives_line)
                .collect();
        }
        Role::TeamAgent => {
            view.board_lines = world.board.digest_lines(Some(agent));
            view.claimed_line = world.board.claimed_by(agent).map(claimed_task_line);
            view.window_lines = world
                .team
                .get(agent)
                .map(|s| s.window.clone())
                .unwrap_or_default();
        }
        Role::MetaAgent => {
            view.metrics_digest = Some(world.metrics.digest());
            view.outcome_lines = world
                .directives
                .iter()
                .filter(|d| &d.from == agent)
                .map(Directive::outcomes_line)
                .collect();
            view.recent_event_lines = world
                .events
                .iter()
                .rev()
                .take(RECENT_EVENTS_WINDOW)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(recent_event_line)
                .collect();
        }
    }

    // Fresh messages: the queued mailbox rendered oldest-first; the
    // assembler decides the drained prefix (carryover, ADR 0011).
    if role != Role::MetaAgent {
        view.queued_messages = world
            .mailboxes
            .peek(agent)
            .filter_map(|id| world.messages.get(&id).map(Message::fresh_line))
            .collect();
    }

    // Auto-retrieval (ADR 0016, pins §5): goal + claimed-task title,
    // skipped while the store is empty; meta has no retrievals slot.
    if role != Role::MetaAgent && world.store.entry_count().await > 0 {
        let mut query = world.goal.clone();
        if let Some(task) = world.board.claimed_by(agent) {
            query.push(' ');
            query.push_str(&task.title);
        }
        if let Ok(hits) = world.store.search(&query, AUTO_RETRIEVAL_K).await {
            view.retrievals = hits
                .iter()
                .map(|hit| (hit.score, retrieval_line(hit)))
                .collect();
        }
    }

    view
}

// ---- the scheduler (ADR 0007/0015) --------------------------------------

struct TurnPlan {
    agent: AgentId,
    role: Role,
    permit: Option<OwnedSemaphorePermit>,
}

/// One dispatch evaluation under the lock (edge-triggered, pins §5). The plan
/// order is fixed — orchestrator, then metas and team agents in `BTreeMap`
/// (handle) order — so under `serial_dispatch` the reactor drives the batch to
/// completion in that order (see [`scheduler`]).
fn plan_dispatches(world: &mut World) -> Vec<TurnPlan> {
    let mut plans = Vec::new();
    if world.finishing.is_some() {
        return plans;
    }

    // Settle any deferred permit-forget debt as permits return.
    if world.permit_debt > 0 {
        let forgotten = world.semaphore.forget_permits(world.permit_debt);
        world.permit_debt -= forgotten;
    }

    // Orchestrator tick (ADR 0007; the pinned edge-trigger reading).
    // Pending input is edge-triggered end to end (#28): a queued mailbox
    // item self-clears on the next tick's drain, and a pending judgment
    // directive counts through its `directive_issued` event, fresh beyond
    // the watermark until a tick has rendered it. A directive the
    // orchestrator has seen and left pending generates no further ticks —
    // otherwise an orchestrator that keeps yielding on it busy-spins to
    // the caps; the liveness watchdog's forced tick is the retry path.
    let orchestrator_id = AgentId::orchestrator();
    if !world.orchestrator.in_flight {
        let queued_mail = world.mailboxes.depth(&orchestrator_id) > 0;
        let fresh = world.has_new_events(world.orchestrator.watermark, &orchestrator_id);
        if !world.first_tick_fired || world.forced_tick || queued_mail || fresh {
            world.first_tick_fired = true;
            world.forced_tick = false;
            world.orchestrator.in_flight = true;
            world.turns_in_flight += 1;
            plans.push(TurnPlan {
                agent: orchestrator_id,
                role: Role::Orchestrator,
                permit: None,
            });
        }
    }

    // Meta cadence: coalesced threshold or priority wake (ADR 0020).
    let meta_ids: Vec<AgentId> = world.metas.keys().cloned().collect();
    for meta in meta_ids {
        let Some(slot) = world.metas.get_mut(&meta) else {
            continue;
        };
        if slot.in_flight {
            continue;
        }
        if slot.priority || slot.unobserved >= META_CADENCE_THRESHOLD {
            slot.priority = false;
            slot.unobserved = 0;
            slot.in_flight = true;
            world.turns_in_flight += 1;
            plans.push(TurnPlan {
                agent: meta,
                role: Role::MetaAgent,
                permit: None,
            });
        }
    }

    // Team agents: Working back-to-back; Idle on eligible work or queued
    // mail with fresh events; Asleep never. One `--parallel` permit each.
    let team_ids: Vec<AgentId> = world.team.keys().cloned().collect();
    for agent in team_ids {
        let Some(slot) = world.team.get(&agent) else {
            continue;
        };
        if slot.in_flight || slot.state == AgentState::Asleep {
            continue;
        }
        let wants_turn = match slot.state {
            AgentState::Working { .. } => true,
            AgentState::Idle => {
                let queued_mail = world.mailboxes.depth(&agent) > 0;
                let eligible_open = world
                    .board
                    .tasks()
                    .any(|t| t.state == TaskState::Open && world.board.is_eligible(&agent, t));
                queued_mail || (eligible_open && world.has_new_work_events(slot.watermark))
            }
            AgentState::Asleep => false,
        };
        if !wants_turn {
            continue;
        }
        let Ok(permit) = world.semaphore.clone().try_acquire_owned() else {
            continue;
        };
        if let Some(slot) = world.team.get_mut(&agent) {
            slot.in_flight = true;
        }
        world.turns_in_flight += 1;
        plans.push(TurnPlan {
            agent,
            role: Role::TeamAgent,
            permit: Some(permit),
        });
    }

    plans
}

/// The event-driven reactor: on each nudge, re-evaluate dispatch; ends when
/// a termination path has been taken and all turns settled (ADR 0007/0015).
///
/// Under `serial_dispatch` (the `--mock` reproducibility mode) the planned
/// batch is driven to completion **in plan order, one turn in flight at a
/// time** — the LLM-call overlap `--parallel` buys is traded for a write-path
/// commit order that is a pure function of seed + goal, so the run is
/// byte-identical across invocations (pins §5). `--parallel` still shapes the
/// run: it caps how many team agents a single planning pass admits, so work
/// still spreads across the pool. Off (the real path), the batch is spawned
/// concurrently as before.
async fn scheduler(shared: Arc<Shared>) {
    let serial = shared.config.serial_dispatch;
    loop {
        let plans = {
            let mut world = shared.world.lock().await;
            if let Some(error) = world.io_error.take() {
                world.trigger_harness_error(
                    format!("events.jsonl write failed: {error}"),
                    &shared.stopping,
                );
            }
            if world.finishing.is_some() {
                if world.turns_in_flight == 0 {
                    return;
                }
                Vec::new()
            } else {
                plan_dispatches(&mut world)
            }
        };
        if serial {
            let dispatched = !plans.is_empty();
            for plan in plans {
                run_turn(shared.clone(), plan.agent, plan.role, plan.permit).await;
            }
            // A non-empty batch changed the world — re-plan at once. An empty
            // pass has nothing to do but await an external nudge (a watchdog
            // forced tick or a duration cap).
            if !dispatched {
                shared.notify.notified().await;
            }
        } else {
            for plan in plans {
                let shared = shared.clone();
                tokio::spawn(run_turn(shared, plan.agent, plan.role, plan.permit));
            }
            shared.notify.notified().await;
        }
    }
}

/// The ~500 ms liveness watchdog: asserts "quiescent ⟹ board done"; fires
/// only on the quiescent-unfinished predicate, emits `liveness_nudge`, and
/// forces exactly one orchestrator tick — never waking team agents
/// (ADR 0015).
async fn watchdog(shared: Arc<Shared>) {
    let mut interval = tokio::time::interval(WATCHDOG_PERIOD);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        if shared.stopping.load(Ordering::SeqCst) {
            return;
        }
        let mut world = shared.world.lock().await;
        if world.finishing.is_some() {
            return;
        }
        if let Some((board_open, claimed_by_asleep)) = world.liveness_predicate() {
            if world.would_dispatch() {
                // Not a deadlock — a nudge the scheduler has not reacted to
                // yet. Re-nudge instead of firing.
                drop(world);
                shared.notify.notify_one();
                continue;
            }
            tracing::warn!(
                board_open,
                claimed_by_asleep,
                "liveness nudge fired — quiescent but unfinished (this is a scheduling bug surfacing loudly)"
            );
            world.append_event(
                EventSource::System,
                EventKind::LivenessNudge {
                    board_open,
                    claimed_by_asleep,
                },
            );
            world.forced_tick = true;
            drop(world);
            shared.notify.notify_one();
        }
    }
}

// ---- the run entrypoint (ADR 0022/0024) ---------------------------------

/// Execute one run: compose the world, drive the reactor to a termination
/// path, persist the artifacts, and return the rendered report (byte-
/// identical to `report.md`) with the ADR 0006 exit code.
pub async fn run(
    config: RunConfig,
    transport: Arc<dyn LlmClient>,
    clock: Arc<dyn Clock>,
) -> Result<RunOutcome, RunError> {
    let run_id = uuid::Uuid::now_v7();
    let run_dir = artifacts::create_run_dir(config.out_dir.as_deref(), run_id)?;
    // Hold an exclusive advisory lock on `<run-dir>/run.lock` for the run's
    // lifetime (ADR 0027): the stream server reads its release as the run
    // dying. Bound to `run()`'s stack, so it drops on every return path, and
    // the kernel releases it on process death (`SIGKILL`, panic).
    let _run_lock = artifacts::acquire_run_lock(&run_dir)?;
    let events_writer = artifacts::EventsWriter::create(&run_dir)?;

    let parallel = config.parallel.min(config.agents).max(1);
    let semaphore = Arc::new(Semaphore::new(parallel));

    let mut team = BTreeMap::new();
    for n in 1..=config.agents {
        let agent = AgentId::team(n);
        team.insert(
            agent.clone(),
            AgentSlot {
                state: AgentState::Idle,
                in_flight: false,
                consecutive_malformed: 0,
                profile: SpecialtyProfile::generalist(),
                window: Vec::new(),
                turn_index: 0,
                watermark: 0,
                channel: Arc::new(AgentChannel::new(transport.clone(), agent, config.seed)),
            },
        );
    }
    let mut metas = BTreeMap::new();
    for n in 1..=config.meta_agents {
        let agent = AgentId::meta(n);
        metas.insert(
            agent.clone(),
            ControlSlot::new(Arc::new(AgentChannel::new(
                transport.clone(),
                agent,
                config.seed,
            ))),
        );
    }
    let orchestrator = ControlSlot::new(Arc::new(AgentChannel::new(
        transport.clone(),
        AgentId::orchestrator(),
        config.seed,
    )));

    let embedder = if config.local_embeddings {
        WireEmbedder::local(transport.clone(), config.embedding_model.clone())
    } else {
        WireEmbedder::new(transport.clone(), config.embedding_model.clone())
    };
    let store = InMemoryVectorStore::new(embedder);

    let mut world = World {
        goal: config.goal.clone(),
        seed: config.seed,
        board: Board::new(),
        messages: BTreeMap::new(),
        mailboxes: Mailboxes::new(),
        store,
        directives: Vec::new(),
        next_task: 1,
        next_message: 1,
        next_directive: 1,
        next_event: 0,
        events: Vec::new(),
        metrics: Metrics::new(),
        clock: clock.clone(),
        events_writer: Some(events_writer),
        io_error: None,
        team,
        orchestrator,
        metas,
        release_counts: HashMap::new(),
        report: None,
        finish_requested: false,
        finishing: None,
        harness_error: None,
        first_tick_fired: false,
        forced_tick: false,
        ticks: 0,
        llm_calls: 0,
        turns_in_flight: 0,
        effective_parallelism: parallel,
        permit_debt: 0,
        semaphore,
        configured_parallelism: parallel,
    };

    world.append_event(
        EventSource::System,
        EventKind::RunStarted {
            run_id,
            seed: config.seed,
            goal: config.goal.clone(),
            agents: config.agents as u32,
            meta_agents: config.meta_agents as u32,
            parallel: parallel as u32,
            scenario: config.scenario.clone(),
            caps: RunCaps {
                max_ticks: config.max_ticks,
                max_llm_calls: config.max_llm_calls,
                max_duration_ms: config.max_duration.map(|d| d.as_millis() as u64),
            },
        },
    );

    let shared = Arc::new(Shared {
        world: Mutex::new(world),
        registry: ToolRegistry::new(),
        config: config.clone(),
        notify: Notify::new(),
        stopping: AtomicBool::new(false),
    });

    tracing::info!(%run_id, seed = config.seed, goal = %config.goal, "run started");

    let watchdog_task = tokio::spawn(watchdog(shared.clone()));
    let duration_task = config.max_duration.map(|duration| {
        let shared = shared.clone();
        tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            let mut world = shared.world.lock().await;
            let secs = duration.as_secs();
            world.trigger_cap(CapKind::MaxDuration, secs, secs, &shared.stopping);
            drop(world);
            shared.notify.notify_one();
        })
    });

    shared.notify.notify_one();
    scheduler(shared.clone()).await;

    watchdog_task.abort();
    if let Some(task) = duration_task {
        task.abort();
    }

    // Finalize on every termination path (ADR 0006/0022).
    let world = shared.world.lock().await;
    let (reason, exit_code) = world
        .finishing
        .unwrap_or((RunFinishReason::HarnessError, 1));
    let body = match reason {
        RunFinishReason::CleanFinish => world
            .report
            .clone()
            .unwrap_or_else(|| "terminated: finish_run carried no report".into()),
        RunFinishReason::CapHit(cap) => {
            format!("terminated: {} cap before finish_run", cap.as_str())
        }
        RunFinishReason::HarnessError => {
            let detail = world.harness_error.clone().unwrap_or_default();
            format!("terminated: harness error before finish_run ({detail})")
        }
    };
    let summary = world.metrics.summary().render();
    let report = format!("{body}\n\n---\n\n{summary}\n");

    let snapshot = artifacts::board_snapshot(run_id, &world.goal, world.seed, &world.board);
    let entries = world.store.entries().await;
    artifacts::write_final_snapshots(&run_dir, &snapshot, &entries, &report)?;
    tracing::info!(%run_id, exit_code, dir = %run_dir.display(), "run finished; artifacts persisted");

    Ok(RunOutcome {
        exit_code,
        report,
        run_dir,
        run_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FrozenClock;
    use crate::llm::LlmError;
    use async_trait::async_trait;
    use openteam_wire::{
        ChatCompletionResponse, Choice, EmbeddingData, EmbeddingRequest, EmbeddingResponse,
        EmbeddingUsage, EmbeddingVector, FinishReason, FunctionCall, ParsedUser, ResponseMessage,
        Usage, WireIdentity,
    };
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::AtomicUsize;

    fn yield_response(text: &str) -> ChatCompletionResponse {
        ChatCompletionResponse {
            id: "chatcmpl-test".into(),
            object: "chat.completion".into(),
            created: 0,
            model: "openteam-mock".into(),
            choices: vec![Choice {
                index: 0,
                message: ResponseMessage {
                    role: "assistant".into(),
                    content: Some(text.into()),
                    refusal: None,
                    tool_calls: None,
                },
                logprobs: None,
                finish_reason: FinishReason::Stop,
            }],
            usage: Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
        }
    }

    fn calls_response(calls: Vec<(&str, serde_json::Value)>) -> ChatCompletionResponse {
        let tool_calls: Vec<ToolCall> = calls
            .into_iter()
            .enumerate()
            .map(|(i, (name, args))| ToolCall {
                id: format!("call_{i}"),
                kind: openteam_wire::ToolType::Function,
                function: FunctionCall {
                    name: name.into(),
                    arguments: args.to_string(),
                },
            })
            .collect();
        ChatCompletionResponse {
            id: "chatcmpl-test".into(),
            object: "chat.completion".into(),
            created: 0,
            model: "openteam-mock".into(),
            choices: vec![Choice {
                index: 0,
                message: ResponseMessage {
                    role: "assistant".into(),
                    content: None,
                    refusal: None,
                    tool_calls: Some(tool_calls),
                },
                logprobs: None,
                finish_reason: FinishReason::ToolCalls,
            }],
            usage: Usage {
                prompt_tokens: 20,
                completion_tokens: 10,
                total_tokens: 30,
            },
        }
    }

    fn fake_embedding(text: &str) -> EmbeddingResponse {
        let sum: u32 = text.bytes().map(u32::from).sum();
        EmbeddingResponse {
            object: "list".into(),
            data: vec![EmbeddingData {
                object: "embedding".into(),
                index: 0,
                embedding: EmbeddingVector::Float(vec![
                    1.0,
                    (sum % 7) as f32,
                    (sum % 13) as f32,
                    0.5,
                ]),
            }],
            model: "openteam-mock".into(),
            usage: EmbeddingUsage {
                prompt_tokens: 1,
                total_tokens: 1,
            },
        }
    }

    fn section<'a>(user_msg: &'a str, header: &str) -> Vec<&'a str> {
        let mut in_section = false;
        let mut lines = Vec::new();
        for line in user_msg.lines() {
            if let Some(h) = line.strip_prefix("## ") {
                in_section = h == header;
                continue;
            }
            if in_section && !line.is_empty() {
                lines.push(line);
            }
        }
        lines
    }

    /// A deterministic in-memory stand-in for the mock's built-in arc: reads
    /// the rendered world exactly like the real mock (the legibility
    /// contract) and drives a claim → one-work-action → complete arc with
    /// W = 1. Also counts completions per user for spin assertions.
    struct MiniArc {
        counts: StdMutex<std::collections::HashMap<String, usize>>,
        team_yield_only: bool,
        team_script: Option<fn(u64) -> ChatCompletionResponse>,
        orchestrator_script: Option<fn(u64) -> ChatCompletionResponse>,
        max_concurrent_team: AtomicUsize,
        current_team: AtomicUsize,
    }

    impl MiniArc {
        fn new() -> Self {
            Self {
                counts: StdMutex::new(std::collections::HashMap::new()),
                team_yield_only: false,
                team_script: None,
                orchestrator_script: None,
                max_concurrent_team: AtomicUsize::new(0),
                current_team: AtomicUsize::new(0),
            }
        }

        fn count_of(&self, user_prefix: &str) -> usize {
            self.counts
                .lock()
                .unwrap()
                .iter()
                .filter(|(user, _)| user.starts_with(user_prefix))
                .map(|(_, n)| *n)
                .sum()
        }
    }

    #[async_trait]
    impl LlmClient for MiniArc {
        async fn complete(
            &self,
            id: &WireIdentity,
            req: &ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, LlmError> {
            *self
                .counts
                .lock()
                .unwrap()
                .entry(id.user.clone())
                .or_insert(0) += 1;
            let parsed = ParsedUser::parse(&id.user)
                .map_err(|e| LlmError::Malformed(format!("bad user field: {e}")))?;

            // Turn-local: a trailing tool message means this turn already
            // acted — yield (the one-verb-per-turn rule).
            if matches!(req.messages.last(), Some(ChatMessage::Tool { .. })) {
                return Ok(yield_response("Acted this turn; yielding."));
            }
            let user_msg = req
                .messages
                .iter()
                .find_map(|m| match m {
                    ChatMessage::User { content, .. } => Some(content.rendered_text()),
                    _ => None,
                })
                .unwrap_or_default();

            match parsed {
                ParsedUser::Orchestrator => {
                    if let Some(script) = self.orchestrator_script {
                        return Ok(script(id.call_seq));
                    }
                    let board = section(&user_msg, "Board digest");
                    let tasks: Vec<&str> = board
                        .iter()
                        .filter(|l| l.starts_with("- task "))
                        .copied()
                        .collect();
                    if tasks.is_empty() {
                        return Ok(calls_response(vec![
                            (
                                "create_task",
                                serde_json::json!({"title": "Alpha", "description": "First half."}),
                            ),
                            (
                                "create_task",
                                serde_json::json!({"title": "Beta", "description": "Second half."}),
                            ),
                        ]));
                    }
                    let all_terminal = tasks
                        .iter()
                        .all(|l| l.contains("[Done]") || l.contains("[Cancelled]"));
                    if all_terminal {
                        return Ok(calls_response(vec![(
                            "finish_run",
                            serde_json::json!({"report": "# Result\nBoth halves done."}),
                        )]));
                    }
                    Ok(yield_response("Waiting on the team."))
                }
                ParsedUser::MetaAgent { .. } => Ok(yield_response("Observing.")),
                ParsedUser::TeamAgent { .. } => {
                    self.current_team.fetch_add(1, Ordering::SeqCst);
                    let now = self.current_team.load(Ordering::SeqCst);
                    self.max_concurrent_team.fetch_max(now, Ordering::SeqCst);
                    let response = (|| {
                        if let Some(script) = self.team_script {
                            return script(id.call_seq);
                        }
                        if self.team_yield_only {
                            return yield_response("Declining.");
                        }
                        let claimed = section(&user_msg, "Claimed task");
                        let working = claimed.first().is_some_and(|l| l.starts_with("task "));
                        if working {
                            let window = section(&user_msg, "Recent activity");
                            let work_actions = window
                                .iter()
                                .filter(|l| {
                                    l.contains("write_knowledge{")
                                        || l.contains("post_message{")
                                        || l.contains("search_knowledge{")
                                })
                                .count();
                            if work_actions >= 1 {
                                return calls_response(vec![(
                                    "complete_task",
                                    serde_json::json!({"result": "Half of the guide, drafted."}),
                                )]);
                            }
                            return calls_response(vec![(
                                "write_knowledge",
                                serde_json::json!({"text": "Progress note on my half."}),
                            )]);
                        }
                        let board = section(&user_msg, "Board digest");
                        if let Some(open) = board
                            .iter()
                            .find(|l| l.starts_with("- task ") && l.contains("[Open]"))
                        {
                            let task: u64 = open
                                .trim_start_matches("- task ")
                                .split_whitespace()
                                .next()
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(1);
                            return calls_response(vec![(
                                "claim_task",
                                serde_json::json!({"task": task}),
                            )]);
                        }
                        yield_response("No eligible work.")
                    })();
                    self.current_team.fetch_sub(1, Ordering::SeqCst);
                    Ok(response)
                }
            }
        }

        async fn embed(&self, req: &EmbeddingRequest) -> Result<EmbeddingResponse, LlmError> {
            let text = req
                .input
                .texts()
                .first()
                .copied()
                .unwrap_or_default()
                .to_string();
            Ok(fake_embedding(&text))
        }
    }

    fn test_config(dir: &std::path::Path, agents: usize, meta: usize) -> RunConfig {
        let mut config = RunConfig::new("Write a short onboarding guide for new contributors");
        config.agents = agents;
        config.meta_agents = meta;
        config.parallel = agents;
        config.seed = 7;
        config.out_dir = Some(dir.to_path_buf());
        config.max_duration = Some(Duration::from_secs(20));
        config
    }

    fn read_events(dir: &std::path::Path) -> Vec<Event> {
        std::fs::read_to_string(dir.join("events.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mini_run_converges_clean_with_board_conservation() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(dir.path(), 2, 1);
        let outcome = run(
            config,
            Arc::new(MiniArc::new()),
            Arc::new(FrozenClock::default()),
        )
        .await
        .unwrap();

        assert_eq!(outcome.exit_code, 0);
        assert!(outcome.report.contains("## Run summary"));
        assert!(outcome.report.contains("Both halves done."));

        let events = read_events(dir.path());
        assert!(matches!(events[0].kind, EventKind::RunStarted { .. }));
        let created: Vec<u64> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::TaskCreated { task, .. } => Some(task.get()),
                _ => None,
            })
            .collect();
        let completed: Vec<u64> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::TaskCompleted { task, .. } => Some(task.get()),
                _ => None,
            })
            .collect();
        assert_eq!(created, vec![1, 2], "predictable 1-based TaskIds");
        assert_eq!(
            completed.len(),
            2,
            "board conservation: all created end Done"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.kind, EventKind::LivenessNudge { .. })),
            "liveness must never fire on the happy path"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.kind, EventKind::ContextDegraded { .. })),
            "no degradation on the happy path"
        );
        let last = events.last().unwrap();
        assert!(
            matches!(
                last.kind,
                EventKind::RunFinished {
                    reason: RunFinishReason::CleanFinish,
                    exit_code: 0
                }
            ),
            "run_finished is the final event"
        );
        assert_eq!(last.source, EventSource::Agent(AgentId::orchestrator()));

        // EventIds are contiguous and 0-based (ADR 0011 amendment).
        for (i, event) in events.iter().enumerate() {
            assert_eq!(event.id.get(), i as u64);
        }

        // Artifacts persisted; stdout report == report.md is the bin's
        // contract, here we check the file matches the returned report.
        let report_md = std::fs::read_to_string(dir.path().join("report.md")).unwrap();
        assert_eq!(report_md, outcome.report);
        let board: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("board.json")).unwrap())
                .unwrap();
        let states: Vec<String> = board["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                t["state"]
                    .as_object()
                    .map(|o| o.keys().next().unwrap().clone())
                    .unwrap_or_else(|| t["state"].as_str().unwrap().to_string())
            })
            .collect();
        assert_eq!(states, vec!["Done", "Done"]);
        let knowledge = std::fs::read_to_string(dir.path().join("knowledge.jsonl")).unwrap();
        assert!(knowledge.lines().count() >= 2, "completions ingested");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn k3_malformed_turns_park_preserving_the_claim() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path(), 1, 0);
        config.max_ticks = Some(8);

        let mut arc = MiniArc::new();
        arc.orchestrator_script = Some(|seq| match seq {
            0 => calls_response(vec![(
                "create_task",
                serde_json::json!({"title": "Alpha", "description": "d"}),
            )]),
            _ => yield_response("waiting"),
        });
        arc.team_script = Some(|seq| match seq {
            0 => calls_response(vec![("claim_task", serde_json::json!({"task": 1}))]),
            seq if seq % 2 == 0 => calls_response(vec![("bogus_verb", serde_json::json!({}))]),
            _ => yield_response("hm"),
        });

        let outcome = run(config, Arc::new(arc), Arc::new(FrozenClock::default()))
            .await
            .unwrap();
        assert_eq!(outcome.exit_code, 2, "stalls out on the tick cap");

        let events = read_events(dir.path());
        let parks: Vec<u32> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::AgentParked { count, .. } => Some(*count),
                _ => None,
            })
            .collect();
        assert_eq!(parks, vec![MALFORMED_PARK_K], "parked exactly once at K=3");
        let malformed_turns = events
            .iter()
            .filter(|e| {
                matches!(
                    &e.kind,
                    EventKind::TurnCompleted {
                        malformed: true,
                        ..
                    }
                ) && e.source == EventSource::Agent(AgentId::team(1))
            })
            .count();
        assert_eq!(malformed_turns, 3, "three consecutive malformed turns");

        let board: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("board.json")).unwrap())
                .unwrap();
        assert!(
            board["tasks"][0]["state"]["Claimed"]["by"]
                .as_str()
                .is_some_and(|by| by == "agent-1"),
            "park preserves the claimed task (ADR 0015)"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn liveness_fires_only_when_quiescent_but_unfinished_and_never_wakes() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path(), 1, 0);
        config.max_ticks = Some(4);

        let mut arc = MiniArc::new();
        arc.orchestrator_script = Some(|seq| match seq {
            0 => calls_response(vec![(
                "create_task",
                serde_json::json!({"title": "Alpha", "description": "d"}),
            )]),
            _ => yield_response("waiting"),
        });
        arc.team_script = Some(|seq| match seq {
            0 => calls_response(vec![("sleep", serde_json::json!({}))]),
            _ => yield_response("zzz"),
        });

        let outcome = run(config, Arc::new(arc), Arc::new(FrozenClock::default()))
            .await
            .unwrap();
        assert_eq!(outcome.exit_code, 2);

        let events = read_events(dir.path());
        let nudges = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::LivenessNudge { .. }))
            .count();
        assert!(nudges >= 1, "the deadlock breaker fired");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.kind, EventKind::AgentWoke { .. })),
            "the nudge never auto-wakes team agents"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e.kind, EventKind::AgentSlept { .. })),
            "the self-sleep was recorded"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn edge_triggered_dispatch_never_busy_spins_a_declining_agent() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(dir.path(), 2, 0);
        config.max_ticks = Some(3);

        let mut arc = MiniArc::new();
        arc.team_yield_only = true;
        arc.orchestrator_script = Some(|seq| match seq {
            0 => calls_response(vec![(
                "create_task",
                serde_json::json!({"title": "Alpha", "description": "d"}),
            )]),
            _ => yield_response("waiting"),
        });
        let arc = Arc::new(arc);

        let outcome = run(config, arc.clone(), Arc::new(FrozenClock::default()))
            .await
            .unwrap();
        assert_eq!(outcome.exit_code, 2);
        let team_completions = arc.count_of("team-agent:");
        assert!(
            team_completions <= 12,
            "declining idle agents must only re-dispatch on fresh nudges, got {team_completions} completions"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn parallel_one_still_converges() {
        let dir = tempfile::tempdir().unwrap();
        // Debug aid: OPENTEAM_TEST_KEEP_DIR persists this test's artifacts.
        let keep = std::env::var("OPENTEAM_TEST_KEEP_DIR")
            .ok()
            .map(PathBuf::from);
        let mut config = test_config(keep.as_deref().unwrap_or(dir.path()), 2, 0);
        config.parallel = 1;
        let arc = Arc::new(MiniArc::new());
        let outcome = run(config, arc.clone(), Arc::new(FrozenClock::default()))
            .await
            .unwrap();
        assert_eq!(outcome.exit_code, 0);
        assert_eq!(
            arc.max_concurrent_team.load(Ordering::SeqCst),
            1,
            "the --parallel semaphore serializes team-agent completions"
        );
    }

    // ---- dispatch-level guard tests (no scheduler) ----------------------

    async fn dispatch_world(agents: usize) -> (Arc<Shared>, ToolRegistry) {
        let transport: Arc<dyn LlmClient> = Arc::new(MiniArc::new());
        let mut team = BTreeMap::new();
        for n in 1..=agents {
            let agent = AgentId::team(n);
            team.insert(
                agent.clone(),
                AgentSlot {
                    state: AgentState::Idle,
                    in_flight: false,
                    consecutive_malformed: 0,
                    profile: SpecialtyProfile::generalist(),
                    window: Vec::new(),
                    turn_index: 0,
                    watermark: 0,
                    channel: Arc::new(AgentChannel::new(transport.clone(), agent, 7)),
                },
            );
        }
        let world = World {
            goal: "g".into(),
            seed: 7,
            board: Board::new(),
            messages: BTreeMap::new(),
            mailboxes: Mailboxes::new(),
            store: InMemoryVectorStore::new(WireEmbedder::new(transport.clone(), "m")),
            directives: Vec::new(),
            next_task: 1,
            next_message: 1,
            next_directive: 1,
            next_event: 0,
            events: Vec::new(),
            metrics: Metrics::new(),
            clock: Arc::new(FrozenClock::default()),
            events_writer: None,
            io_error: None,
            team,
            orchestrator: ControlSlot::new(Arc::new(AgentChannel::new(
                transport.clone(),
                AgentId::orchestrator(),
                7,
            ))),
            metas: BTreeMap::new(),
            release_counts: HashMap::new(),
            report: None,
            finish_requested: false,
            finishing: None,
            harness_error: None,
            first_tick_fired: false,
            forced_tick: false,
            ticks: 0,
            llm_calls: 0,
            turns_in_flight: 0,
            effective_parallelism: 2,
            configured_parallelism: 2,
            permit_debt: 0,
            semaphore: Arc::new(Semaphore::new(2)),
        };
        let shared = Arc::new(Shared {
            world: Mutex::new(world),
            registry: ToolRegistry::new(),
            config: RunConfig::new("g"),
            notify: Notify::new(),
            stopping: AtomicBool::new(false),
        });
        (shared, ToolRegistry::new())
    }

    fn tool_call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call_t".into(),
            kind: openteam_wire::ToolType::Function,
            function: FunctionCall {
                name: name.into(),
                arguments: args.to_string(),
            },
        }
    }

    #[tokio::test]
    async fn dispatch_separates_invalid_from_rejected() {
        let (shared, registry) = dispatch_world(2).await;
        let mut world = shared.world.lock().await;
        let orchestrator = AgentId::orchestrator();
        let agent = AgentId::team(1);

        // Unknown verb and bad args are schema faults: invalid.
        let unknown = registry
            .dispatch(
                Role::TeamAgent,
                &agent,
                &tool_call("bogus", serde_json::json!({})),
                &mut world,
            )
            .await;
        assert!(matches!(unknown, ToolOutcome::Invalid { ref code, .. } if code == "unknown_verb"));
        let bad_args = registry
            .dispatch(
                Role::TeamAgent,
                &agent,
                &tool_call("claim_task", serde_json::json!({"task": 1, "stray": true})),
                &mut world,
            )
            .await;
        assert!(
            matches!(bad_args, ToolOutcome::Invalid { ref code, .. } if code == "invalid_arguments"),
            "deny_unknown_fields makes stray keys invalid"
        );
        // A verb outside the caller's registry is invalid even though it
        // exists for another role (dispatch is per-role).
        let cross_role = registry
            .dispatch(
                Role::TeamAgent,
                &agent,
                &tool_call(
                    "create_task",
                    serde_json::json!({"title": "x", "description": "y"}),
                ),
                &mut world,
            )
            .await;
        assert!(matches!(cross_role, ToolOutcome::Invalid { .. }));

        // A lost claim race is a well-formed domain refusal: rejected.
        let created = registry
            .dispatch(
                Role::Orchestrator,
                &orchestrator,
                &tool_call(
                    "create_task",
                    serde_json::json!({"title": "Alpha", "description": "d"}),
                ),
                &mut world,
            )
            .await;
        assert!(matches!(created, ToolOutcome::Ok { .. }));
        let win = registry
            .dispatch(
                Role::TeamAgent,
                &agent,
                &tool_call("claim_task", serde_json::json!({"task": 1})),
                &mut world,
            )
            .await;
        assert!(matches!(win, ToolOutcome::Ok { .. }));
        let lose = registry
            .dispatch(
                Role::TeamAgent,
                &AgentId::team(2),
                &tool_call("claim_task", serde_json::json!({"task": 1})),
                &mut world,
            )
            .await;
        assert!(
            matches!(lose, ToolOutcome::Rejected { ref code, .. } if code == "task_not_open"),
            "first claim wins; the loser is rejected, never invalid"
        );

        // finish_run with blockers enumerates them.
        let finish = registry
            .dispatch(
                Role::Orchestrator,
                &orchestrator,
                &tool_call("finish_run", serde_json::json!({"report": "r"})),
                &mut world,
            )
            .await;
        match finish {
            ToolOutcome::Rejected { code, message, .. } => {
                assert_eq!(code, "blockers");
                assert!(message.contains("task 1"), "blockers enumerated: {message}");
            }
            other => panic!("expected rejected blockers, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sleep_wake_and_respecialize_guards() {
        let (shared, registry) = dispatch_world(2).await;
        let mut world = shared.world.lock().await;
        let orchestrator = AgentId::orchestrator();
        let agent = AgentId::team(1);

        // Working agents cannot self-sleep (Idle-only, ADR 0015).
        registry
            .dispatch(
                Role::Orchestrator,
                &orchestrator,
                &tool_call(
                    "create_task",
                    serde_json::json!({"title": "A", "description": "d"}),
                ),
                &mut world,
            )
            .await;
        registry
            .dispatch(
                Role::TeamAgent,
                &agent,
                &tool_call("claim_task", serde_json::json!({"task": 1})),
                &mut world,
            )
            .await;
        let sleep_working = registry
            .dispatch(
                Role::TeamAgent,
                &agent,
                &tool_call("sleep", serde_json::json!({})),
                &mut world,
            )
            .await;
        assert!(
            matches!(sleep_working, ToolOutcome::Rejected { ref code, .. } if code == "not_idle")
        );

        // Respecializing a non-Idle agent is illegal (ADR 0003).
        let respec_working = registry
            .dispatch(
                Role::Orchestrator,
                &orchestrator,
                &tool_call(
                    "respecialize",
                    serde_json::json!({"agent": "agent-1", "specialty": {"name": "doc-reviewer", "description": "d", "focus": "f"}}),
                ),
                &mut world,
            )
            .await;
        assert!(
            matches!(respec_working, ToolOutcome::Rejected { ref code, .. } if code == "not_idle")
        );

        // Idle agent-2: orchestrator sleep, wake restores Idle; double wake rejected.
        let slept = registry
            .dispatch(
                Role::Orchestrator,
                &orchestrator,
                &tool_call("sleep_agent", serde_json::json!({"agent": "agent-2"})),
                &mut world,
            )
            .await;
        assert!(matches!(slept, ToolOutcome::Ok { .. }));
        let woke = registry
            .dispatch(
                Role::Orchestrator,
                &orchestrator,
                &tool_call("wake_agent", serde_json::json!({"agent": "agent-2"})),
                &mut world,
            )
            .await;
        assert!(matches!(woke, ToolOutcome::Ok { .. }));
        let woke_again = registry
            .dispatch(
                Role::Orchestrator,
                &orchestrator,
                &tool_call("wake_agent", serde_json::json!({"agent": "agent-2"})),
                &mut world,
            )
            .await;
        assert!(
            matches!(woke_again, ToolOutcome::Rejected { ref code, .. } if code == "not_asleep"),
            "wake is legal only from Asleep"
        );

        // Respecialize the idle agent: wipes the window, emits the event,
        // and the channel keeps its monotonic call-seq.
        if let Some(slot) = world.team.get_mut(&AgentId::team(2)) {
            slot.window
                .push("- [turn 1] write_knowledge{\"x\"} -> ok".into());
        }
        let respec = registry
            .dispatch(
                Role::Orchestrator,
                &orchestrator,
                &tool_call(
                    "respecialize",
                    serde_json::json!({"agent": "agent-2", "specialty": {"name": "doc-reviewer", "description": "Reviews docs.", "focus": "review"}}),
                ),
                &mut world,
            )
            .await;
        assert!(matches!(respec, ToolOutcome::Ok { .. }));
        let slot = world.team.get(&AgentId::team(2)).unwrap();
        assert!(slot.window.is_empty(), "respecialize wipes the window");
        assert_eq!(slot.profile.slug.as_str(), "doc-reviewer");
        assert!(world.events.iter().any(|e| matches!(
            &e.kind,
            EventKind::AgentRespecialized { agent, via_directive: None, .. } if agent == &AgentId::team(2)
        )));

        // A cite of an unknown directive is rejected before acting.
        let bad_cite = registry
            .dispatch(
                Role::Orchestrator,
                &orchestrator,
                &tool_call(
                    "respecialize",
                    serde_json::json!({"agent": "agent-2", "specialty": {"name": "editor", "description": "d", "focus": "f"}, "in_response_to": 9}),
                ),
                &mut world,
            )
            .await;
        assert!(
            matches!(bad_cite, ToolOutcome::Rejected { ref code, .. } if code == "unknown_directive")
        );
    }

    #[tokio::test]
    async fn directive_round_trip_and_addressing() {
        let (shared, registry) = dispatch_world(2).await;
        let mut world = shared.world.lock().await;
        let orchestrator = AgentId::orchestrator();
        // Give the world a meta so the meta verbs have a caller.
        let transport: Arc<dyn LlmClient> = Arc::new(MiniArc::new());
        let meta = AgentId::meta(1);
        world.metas.insert(
            meta.clone(),
            ControlSlot::new(Arc::new(AgentChannel::new(transport, meta.clone(), 7))),
        );

        // Judgment: propose → pending → fulfilled via cited respecialize.
        let proposed = registry
            .dispatch(
                Role::MetaAgent,
                &meta,
                &tool_call(
                    "propose_respecialize",
                    serde_json::json!({"agent": "agent-1", "specialty": "doc-reviewer"}),
                ),
                &mut world,
            )
            .await;
        match &proposed {
            ToolOutcome::Ok { result } => assert_eq!(result["directive_id"], 1),
            other => panic!("expected ok directive_id, got {other:?}"),
        }
        let fulfilled = registry
            .dispatch(
                Role::Orchestrator,
                &orchestrator,
                &tool_call(
                    "respecialize",
                    serde_json::json!({"agent": "agent-1", "specialty": {"name": "doc-reviewer", "description": "d", "focus": "f"}, "in_response_to": 1}),
                ),
                &mut world,
            )
            .await;
        assert!(matches!(fulfilled, ToolOutcome::Ok { .. }));
        assert!(world.events.iter().any(|e| matches!(
            &e.kind,
            EventKind::DirectiveFulfilled { directive, .. } if directive.get() == 1
        )));
        assert!(world.events.iter().any(|e| matches!(
            &e.kind,
            EventKind::AgentRespecialized { via_directive: Some(d), .. } if d.get() == 1
        )));

        // Declining a resolved directive is rejected; a fresh one declines.
        let decline_resolved = registry
            .dispatch(
                Role::Orchestrator,
                &orchestrator,
                &tool_call(
                    "decline_directive",
                    serde_json::json!({"directive": 1, "reason": "already acted"}),
                ),
                &mut world,
            )
            .await;
        assert!(matches!(
            decline_resolved,
            ToolOutcome::Rejected { ref code, .. } if code == "directive_not_pending"
        ));
        registry
            .dispatch(
                Role::MetaAgent,
                &meta,
                &tool_call(
                    "propose_reallocate",
                    serde_json::json!({"task": 1, "reason": "seems stuck"}),
                ),
                &mut world,
            )
            .await;
        let declined = registry
            .dispatch(
                Role::Orchestrator,
                &orchestrator,
                &tool_call(
                    "decline_directive",
                    serde_json::json!({"directive": 2, "reason": "allocation is fine"}),
                ),
                &mut world,
            )
            .await;
        assert!(matches!(declined, ToolOutcome::Ok { .. }));
        assert!(world.events.iter().any(|e| matches!(
            &e.kind,
            EventKind::DirectiveDeclined { directive, .. } if directive.get() == 2
        )));
        // The decline priority-wakes the observing meta (ADR 0020).
        assert!(world.metas.get(&meta).unwrap().priority);

        // Mechanical set_parallelism clamps to [1, --parallel] and records
        // the applied effect.
        let clamped = registry
            .dispatch(
                Role::MetaAgent,
                &meta,
                &tool_call("set_parallelism", serde_json::json!({"target": 99})),
                &mut world,
            )
            .await;
        match &clamped {
            ToolOutcome::Ok { result } => {
                assert_eq!(result["effective"], 2, "clamped to the CLI --parallel");
            }
            other => panic!("expected ok, got {other:?}"),
        }
        assert!(world.events.iter().any(|e| matches!(
            &e.kind,
            EventKind::ParallelismChanged {
                requested: 99,
                effective: 2,
                ..
            }
        )));

        // A guard-failed mechanical emits NO directive_issued (ADR 0022).
        let issued_before = world
            .events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::DirectiveIssued { .. }))
            .count();
        let refused = registry
            .dispatch(
                Role::MetaAgent,
                &meta,
                &tool_call("wake_agent", serde_json::json!({"agent": "agent-2"})),
                &mut world,
            )
            .await;
        assert!(matches!(refused, ToolOutcome::Rejected { .. }));
        let issued_after = world
            .events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::DirectiveIssued { .. }))
            .count();
        assert_eq!(issued_before, issued_after);

        // Addressing: exactly one form; meta targets refused; broadcast
        // excludes the sender and the metas.
        let two_forms = registry
            .dispatch(
                Role::Orchestrator,
                &orchestrator,
                &tool_call(
                    "post_message",
                    serde_json::json!({"to": "agent-1", "broadcast": true, "body": "x"}),
                ),
                &mut world,
            )
            .await;
        assert!(matches!(
            two_forms,
            ToolOutcome::Rejected { ref code, .. } if code == "invalid_address"
        ));
        let to_meta = registry
            .dispatch(
                Role::Orchestrator,
                &orchestrator,
                &tool_call(
                    "post_message",
                    serde_json::json!({"to": "meta-1", "body": "x"}),
                ),
                &mut world,
            )
            .await;
        assert!(matches!(
            to_meta,
            ToolOutcome::Rejected { ref code, .. } if code == "invalid_address"
        ));
        let broadcast = registry
            .dispatch(
                Role::Orchestrator,
                &orchestrator,
                &tool_call(
                    "post_message",
                    serde_json::json!({"broadcast": true, "body": "hello team"}),
                ),
                &mut world,
            )
            .await;
        assert!(matches!(broadcast, ToolOutcome::Ok { .. }));
        assert_eq!(world.mailboxes.depth(&AgentId::team(1)), 1);
        assert_eq!(world.mailboxes.depth(&AgentId::team(2)), 1);
        assert_eq!(world.mailboxes.depth(&orchestrator), 0, "sender excluded");
        assert_eq!(world.mailboxes.depth(&meta), 0, "metas get no broadcasts");
    }

    #[tokio::test]
    async fn watchdog_predicate_fires_despite_a_stale_pending_directive() {
        // #28: a pending judgment directive over a dead pool must not
        // suppress the liveness watchdog — before the fix the predicate's
        // "orchestrator quiet" clause held it false forever, and the stuck
        // run rode the caps. The forced tick is exactly the orchestrator's
        // resolve-or-decline chance.
        let (shared, registry) = dispatch_world(1).await;
        let mut world = shared.world.lock().await;
        let orchestrator = AgentId::orchestrator();
        let agent = AgentId::team(1);
        let transport: Arc<dyn LlmClient> = Arc::new(MiniArc::new());
        let meta = AgentId::meta(1);
        world.metas.insert(
            meta.clone(),
            ControlSlot::new(Arc::new(AgentChannel::new(transport, meta.clone(), 7))),
        );

        // The deadlock shape: one Open task, the whole pool Asleep, plus a
        // judgment directive the orchestrator never resolves.
        registry
            .dispatch(
                Role::Orchestrator,
                &orchestrator,
                &tool_call(
                    "create_task",
                    serde_json::json!({"title": "Orphaned work", "description": "d"}),
                ),
                &mut world,
            )
            .await;
        registry
            .dispatch(
                Role::TeamAgent,
                &agent,
                &tool_call("sleep", serde_json::json!({})),
                &mut world,
            )
            .await;
        registry
            .dispatch(
                Role::MetaAgent,
                &meta,
                &tool_call(
                    "propose_respecialize",
                    serde_json::json!({"agent": "agent-1", "specialty": "doc-reviewer"}),
                ),
                &mut world,
            )
            .await;
        assert!(world.directives.iter().any(Directive::is_pending));
        world.first_tick_fired = true;

        // The enqueue is still a dispatch edge: `directive_issued` sits
        // beyond the orchestrator's watermark, so a tick fires to render it.
        world.orchestrator.watermark = 0;
        assert!(world.would_dispatch(), "the enqueue edge dispatches a tick");

        // Once a tick has rendered the directive (watermark past its event)
        // and the meta is caught up, a still-pending directive generates no
        // further dispatches — and the fire condition holds regardless.
        world.orchestrator.watermark = world.events.len();
        if let Some(slot) = world.metas.get_mut(&meta) {
            slot.unobserved = 0;
        }
        assert!(
            !world.would_dispatch(),
            "a stale pending directive is not dispatchable input"
        );
        assert_eq!(
            world.liveness_predicate(),
            Some((1, 0)),
            "quiescent-unfinished fires despite the pending directive (#28)"
        );
    }
}
