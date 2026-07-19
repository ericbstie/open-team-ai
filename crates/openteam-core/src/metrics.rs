//! Runtime-owned metrics: one accumulator, one fold, three projections
//! (ADR 0020).
//!
//! [`Metrics::fold`] is called for every appended event; the three pure
//! projections are [`Metrics::run_health_line`] (the orchestrator's compact
//! steering line), [`Metrics::digest`] (the meta-agent's full process view),
//! and [`Metrics::summary`] (the report's `## Run summary`). Ownership is
//! load-bearing: the module lives in the runtime, not the meta-agent, so a
//! `--meta-agents 0` run still measures itself.
//!
//! Every time-like value is counted in **EventId deltas / orchestrator
//! ticks, never wall-clock** — except the human duration line of the
//! summary, computed from the first/last event `at` breadcrumbs (ADR 0022).

use jiff::Timestamp;
use openteam_wire::{AgentId, SpecialtySlug};
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};

use crate::directive::DirectiveTier;
use crate::event::{Event, EventKind, RestoredState, RunFinishReason, TurnUsage};
use crate::ids::{EventId, MessageId, TaskId, TeamId};
use crate::message::Address;

/// A team agent's lifecycle position as folded from events (ADR 0015):
/// `task_claimed` → Working, `task_completed`/`released`/`unassigned` →
/// Idle, `agent_slept`/`parked` → Asleep, `agent_woke` → its restored state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MeterState {
    Idle,
    Working(TaskId),
    Asleep,
}

/// Per-team-agent fold state. Agents are enumerated from `run_started`
/// counts; all boot generalist and Idle.
#[derive(Debug, Clone)]
struct AgentMeter {
    id: AgentId,
    state: MeterState,
    specialty: SpecialtySlug,
    /// The EventId of the agent's most recent Idle activity — the base of
    /// the digest's `(idle <k>)` streak, in EventId deltas.
    idle_since: EventId,
    consecutive_malformed: u32,
}

/// The single runtime-owned accumulator (CONTEXT.md: Metrics).
#[derive(Debug, Clone, Default)]
pub struct Metrics {
    // Run header.
    team_agent_count: u32,
    meta_agent_count: u32,
    initial_parallelism: u32,
    // Log position.
    last_event_id: Option<EventId>,
    first_at: Option<Timestamp>,
    last_at: Option<Timestamp>,
    // Ticks — a tick IS an orchestrator turn_completed (ADR 0022).
    ticks: u64,
    ticks_since_done: u64,
    // Tasks.
    tasks_created: u64,
    tasks_completed: u64,
    tasks_cancelled: u64,
    claimed_at: BTreeMap<TaskId, EventId>,
    /// Work latencies (`claimed → completed`) in EventId deltas.
    work_latencies: Vec<u64>,
    // Team agents.
    team_agents: Vec<AgentMeter>,
    // Messages & mailbox pressure. Depth = sent-not-yet-delivered per
    // recipient, folded by pairing each send against `messages_delivered`
    // (ADR 0022); Team/Broadcast recipients are computed from folded
    // membership at acceptance time.
    messages_total: u64,
    messages_direct: u64,
    messages_team: u64,
    messages_broadcast: u64,
    pending: BTreeMap<AgentId, VecDeque<(MessageId, EventId)>>,
    mailbox_max_depth: u64,
    teams: BTreeMap<TeamId, Vec<AgentId>>,
    // Knowledge (folded from the three causing events, ADR 0022).
    knowledge_messages: u64,
    knowledge_notes: u64,
    knowledge_completions: u64,
    knowledge_bytes: u64,
    // Lifecycle.
    sleeps: u64,
    wakes: u64,
    parks: u64,
    respecializations: Vec<(AgentId, SpecialtySlug, SpecialtySlug)>,
    specialties_used: Vec<SpecialtySlug>,
    parallelism_changes: Vec<u32>,
    // Tokens (informational usage fold, ADR 0020).
    tokens_total: u64,
    tokens_per_agent: BTreeMap<AgentId, u64>,
    // Directives (ADR 0022's fold: fulfilled = directive_fulfilled +
    // mechanical directive_issued).
    directives_issued: u64,
    directives_fulfilled: u64,
    directives_declined: u64,
    liveness_nudges: u64,
    outcome: Option<(RunFinishReason, u8)>,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one appended event — called incrementally for every event on
    /// the serial write path (ADR 0020).
    pub fn fold(&mut self, event: &Event) {
        self.last_event_id = Some(event.id);
        if self.first_at.is_none() {
            self.first_at = Some(event.at);
        }
        self.last_at = Some(event.at);

        match &event.kind {
            EventKind::RunStarted {
                agents,
                meta_agents,
                parallel,
                ..
            } => {
                self.team_agent_count = *agents;
                self.meta_agent_count = *meta_agents;
                self.initial_parallelism = *parallel;
                self.team_agents = (1..=*agents as usize)
                    .map(|n| AgentMeter {
                        id: AgentId::team(n),
                        state: MeterState::Idle,
                        specialty: SpecialtySlug::generalist(),
                        idle_since: event.id,
                        consecutive_malformed: 0,
                    })
                    .collect();
                if *agents > 0 {
                    self.note_specialty(SpecialtySlug::generalist());
                }
            }
            EventKind::RunFinished { reason, exit_code } => {
                self.outcome = Some((*reason, *exit_code));
            }
            EventKind::CapHit { .. } => {}

            EventKind::TaskCreated { .. } => {
                self.tasks_created += 1;
            }
            EventKind::TaskClaimed { task, .. } => {
                self.claimed_at.insert(*task, event.id);
                if let Some(agent) = event.source.agent().cloned() {
                    self.set_state(&agent, MeterState::Working(*task), event.id);
                }
            }
            EventKind::TaskReleased { .. } => {
                if let Some(agent) = event.source.agent().cloned() {
                    self.set_state(&agent, MeterState::Idle, event.id);
                }
            }
            EventKind::TaskUnassigned { prev_claimant, .. } => {
                let prev = prev_claimant.clone();
                self.set_state(&prev, MeterState::Idle, event.id);
            }
            EventKind::TaskCompleted { task, result, .. } => {
                self.tasks_completed += 1;
                self.ticks_since_done = 0;
                if let Some(claimed) = self.claimed_at.remove(task) {
                    self.work_latencies
                        .push(event.id.get().saturating_sub(claimed.get()));
                }
                self.knowledge_completions += 1;
                self.knowledge_bytes += result.len() as u64;
                if let Some(agent) = event.source.agent().cloned() {
                    self.set_state(&agent, MeterState::Idle, event.id);
                }
            }
            EventKind::TaskCancelled { .. } => {
                self.tasks_cancelled += 1;
            }

            EventKind::MessageSent {
                message,
                address,
                body,
                ..
            } => {
                self.messages_total += 1;
                match address {
                    Address::Direct { .. } => self.messages_direct += 1,
                    Address::Team { .. } => self.messages_team += 1,
                    Address::Broadcast => self.messages_broadcast += 1,
                }
                self.knowledge_messages += 1;
                self.knowledge_bytes += body.len() as u64;
                let recipients = self.recipients(event.source.agent(), address);
                for recipient in recipients {
                    let queue = self.pending.entry(recipient).or_default();
                    queue.push_back((*message, event.id));
                    self.mailbox_max_depth = self.mailbox_max_depth.max(queue.len() as u64);
                }
            }
            EventKind::MessagesDelivered { delivered } => {
                if let Some(recipient) = event.source.agent()
                    && let Some(queue) = self.pending.get_mut(recipient)
                {
                    queue.retain(|(id, _)| !delivered.contains(id));
                }
            }
            EventKind::KnowledgeWritten { text, .. } => {
                self.knowledge_notes += 1;
                self.knowledge_bytes += text.len() as u64;
            }

            EventKind::TurnCompleted {
                tool_iters,
                malformed,
                usage,
                ..
            } => {
                self.fold_usage(event.source.agent(), usage);
                if event.source.agent() == Some(&AgentId::orchestrator()) {
                    self.ticks += 1;
                    self.ticks_since_done += 1;
                }
                if let Some(agent) = event.source.agent().cloned()
                    && let Some(meter) = self.meter_mut(&agent)
                {
                    // Only all-`invalid` turns increment; `ok`/`rejected`
                    // reset; zero-tool-call turns do neither (pins §5).
                    if *malformed {
                        meter.consecutive_malformed += 1;
                    } else if *tool_iters > 0 {
                        meter.consecutive_malformed = 0;
                    }
                    if meter.state == MeterState::Idle {
                        meter.idle_since = event.id;
                    }
                }
            }
            EventKind::AgentSlept { agent, .. } => {
                self.sleeps += 1;
                let agent = agent.clone();
                self.set_state(&agent, MeterState::Asleep, event.id);
            }
            EventKind::AgentParked { agent, .. } => {
                self.parks += 1;
                let agent = agent.clone();
                self.set_state(&agent, MeterState::Asleep, event.id);
            }
            EventKind::AgentWoke {
                agent, restored, ..
            } => {
                self.wakes += 1;
                let state = match restored {
                    RestoredState::Working { task } => MeterState::Working(*task),
                    RestoredState::Idle => MeterState::Idle,
                };
                let agent = agent.clone();
                self.set_state(&agent, state, event.id);
            }
            EventKind::ParallelismChanged { effective, .. } => {
                self.parallelism_changes.push(*effective);
            }
            EventKind::LivenessNudge { .. } => {
                self.liveness_nudges += 1;
            }

            EventKind::TeamFormed { team, members } => {
                self.teams.insert(team.clone(), members.clone());
            }
            EventKind::TeamMembersSet { team, members, .. } => {
                self.teams.insert(team.clone(), members.clone());
            }
            EventKind::TeamDissolved { team } => {
                self.teams.remove(team);
            }

            EventKind::AgentRespecialized {
                agent, from, to, ..
            } => {
                self.respecializations
                    .push((agent.clone(), from.clone(), to.clone()));
                self.note_specialty(to.clone());
                let (agent, to) = (agent.clone(), to.clone());
                if let Some(meter) = self.meter_mut(&agent) {
                    meter.specialty = to;
                    // A fresh persona starts a fresh idle streak.
                    if meter.state == MeterState::Idle {
                        meter.idle_since = event.id;
                    }
                }
            }

            EventKind::DirectiveIssued { tier, .. } => {
                self.directives_issued += 1;
                // Mechanical-issued ⟹ applied (ADR 0022).
                if *tier == DirectiveTier::Mechanical {
                    self.directives_fulfilled += 1;
                }
            }
            EventKind::DirectiveFulfilled { .. } => {
                self.directives_fulfilled += 1;
            }
            EventKind::DirectiveDeclined { .. } => {
                self.directives_declined += 1;
            }

            EventKind::ContextDegraded { .. } => {}
        }
    }

    // ---- fold helpers ----------------------------------------------------

    fn meter_mut(&mut self, agent: &AgentId) -> Option<&mut AgentMeter> {
        self.team_agents.iter_mut().find(|m| &m.id == agent)
    }

    fn set_state(&mut self, agent: &AgentId, state: MeterState, at: EventId) {
        if let Some(meter) = self.meter_mut(agent) {
            meter.state = state;
            if state == MeterState::Idle {
                meter.idle_since = at;
            }
        }
    }

    fn note_specialty(&mut self, specialty: SpecialtySlug) {
        if !self.specialties_used.contains(&specialty) {
            self.specialties_used.push(specialty);
        }
    }

    fn fold_usage(&mut self, agent: Option<&AgentId>, usage: &TurnUsage) {
        self.tokens_total += usage.total;
        if let Some(agent) = agent {
            *self.tokens_per_agent.entry(agent.clone()).or_default() += usage.total;
        }
    }

    /// Recipients of a message at acceptance time (ADR 0011): direct → the
    /// target; team → the team's folded membership; broadcast → the
    /// orchestrator and all team agents, never meta-agents. The sender never
    /// receives its own team/broadcast message.
    fn recipients(&self, sender: Option<&AgentId>, address: &Address) -> Vec<AgentId> {
        match address {
            Address::Direct { to } => vec![to.clone()],
            Address::Team { team } => self
                .teams
                .get(team)
                .map(|members| {
                    members
                        .iter()
                        .filter(|member| Some(*member) != sender)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
            Address::Broadcast => {
                let orchestrator = AgentId::orchestrator();
                std::iter::once(orchestrator)
                    .chain(self.team_agents.iter().map(|m| m.id.clone()))
                    .filter(|recipient| Some(recipient) != sender)
                    .collect()
            }
        }
    }

    // ---- projection helpers ------------------------------------------------

    fn events(&self) -> u64 {
        self.last_event_id.map_or(0, EventId::get)
    }

    /// Current mailbox depth = the deepest per-recipient sent-not-delivered
    /// queue.
    fn mailbox_depth(&self) -> u64 {
        self.pending
            .values()
            .map(|q| q.len() as u64)
            .max()
            .unwrap_or(0)
    }

    /// Oldest pending message age in EventId deltas (0 when none pending).
    fn oldest_pending_age(&self) -> u64 {
        let now = self.events();
        self.pending
            .values()
            .flatten()
            .map(|(_, sent)| now.saturating_sub(sent.get()))
            .max()
            .unwrap_or(0)
    }

    fn state_counts(&self) -> (usize, usize, usize) {
        let mut counts = (0, 0, 0);
        for meter in &self.team_agents {
            match meter.state {
                MeterState::Working(_) => counts.0 += 1,
                MeterState::Idle => counts.1 += 1,
                MeterState::Asleep => counts.2 += 1,
            }
        }
        counts
    }

    fn work_latency_median(&self) -> Option<u64> {
        if self.work_latencies.is_empty() {
            return None;
        }
        let mut sorted = self.work_latencies.clone();
        sorted.sort_unstable();
        let mid = sorted.len() / 2;
        Some(if sorted.len() % 2 == 1 {
            sorted[mid]
        } else {
            u64::midpoint(sorted[mid - 1], sorted[mid])
        })
    }

    // ---- projection 1: the orchestrator's run-health line -------------------

    /// The compact steering line folded into the orchestrator's board digest
    /// (ADR 0016/0020, pins §3):
    /// `run-health: done <d>/<n> · agents <w>W/<i>I/<s>S · mailbox depth <cur> (max <max>) · ticks-since-done <t>`.
    pub fn run_health_line(&self) -> String {
        let (working, idle, asleep) = self.state_counts();
        format!(
            "run-health: done {}/{} · agents {}W/{}I/{}S · mailbox depth {} (max {}) · ticks-since-done {}",
            self.tasks_completed,
            self.tasks_created,
            working,
            idle,
            asleep,
            self.mailbox_depth(),
            self.mailbox_max_depth,
            self.ticks_since_done
        )
    }

    // ---- projection 2: the meta-agent's metrics digest ----------------------

    /// The meta-agent's full process view (`## Metrics digest` content,
    /// pins §3 exact line shapes). Every value is in EventId deltas / tick
    /// counts, never wall-clock, so the digest is golden-testable.
    pub fn digest(&self) -> String {
        let latency = self.work_latency_median().map_or_else(
            || "work n/a".to_string(),
            |median| format!("work median {median} EventIds"),
        );
        let mut lines = vec![
            format!(
                "throughput: {} task_completed / {} EventIds · latency: {}",
                self.tasks_completed,
                self.events(),
                latency
            ),
            "utilization:".to_string(),
        ];
        let now = self.events();
        for meter in &self.team_agents {
            let line = match meter.state {
                MeterState::Working(task) => {
                    format!(
                        "  - {}: Working (task {}), {}",
                        meter.id, task, meter.specialty
                    )
                }
                MeterState::Idle => format!(
                    "  - {}: Idle, {} (idle {})",
                    meter.id,
                    meter.specialty,
                    now.saturating_sub(meter.idle_since.get())
                ),
                MeterState::Asleep => format!("  - {}: Asleep, {}", meter.id, meter.specialty),
            };
            lines.push(line);
        }
        lines.push(format!(
            "mailbox: depth {}, max {}, oldest-pending-age {}",
            self.mailbox_depth(),
            self.mailbox_max_depth,
            self.oldest_pending_age()
        ));
        let malformed = self
            .team_agents
            .iter()
            .map(|m| format!("{}:{}", short_handle(&m.id), m.consecutive_malformed))
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(format!(
            "tokens: run {} · faults: parks {}, malformed[{}] · directives: issued {}/ful {}/dec {}",
            fmt_tokens(self.tokens_total),
            self.parks,
            malformed,
            self.directives_issued,
            self.directives_fulfilled,
            self.directives_declined
        ));
        lines.join("\n")
    }

    // ---- projection 3: the report's run summary ------------------------------

    /// The report's `## Run summary` data (ADR 0020/0022).
    pub fn summary(&self) -> RunSummary {
        let wall_secs = match (self.first_at, self.last_at) {
            (Some(first), Some(last)) => {
                (last.as_millisecond().saturating_sub(first.as_millisecond())) as f64 / 1000.0
            }
            _ => 0.0,
        };
        let mut tokens_per_agent: Vec<(AgentId, u64)> =
            Vec::with_capacity(1 + self.team_agents.len() + self.meta_agent_count as usize);
        let handles = std::iter::once(AgentId::orchestrator())
            .chain((1..=self.team_agent_count as usize).map(AgentId::team))
            .chain((1..=self.meta_agent_count as usize).map(AgentId::meta));
        for handle in handles {
            let total = self.tokens_per_agent.get(&handle).copied().unwrap_or(0);
            tokens_per_agent.push((handle, total));
        }
        RunSummary {
            outcome: self.outcome,
            wall_secs,
            ticks: self.ticks,
            team_agents: self.team_agent_count,
            meta_agents: self.meta_agent_count,
            specialties_used: self.specialties_used.clone(),
            tasks_created: self.tasks_created,
            tasks_completed: self.tasks_completed,
            tasks_cancelled: self.tasks_cancelled,
            messages_total: self.messages_total,
            messages_direct: self.messages_direct,
            messages_team: self.messages_team,
            messages_broadcast: self.messages_broadcast,
            knowledge_entries: self.knowledge_messages
                + self.knowledge_notes
                + self.knowledge_completions,
            knowledge_messages: self.knowledge_messages,
            knowledge_notes: self.knowledge_notes,
            knowledge_completions: self.knowledge_completions,
            knowledge_bytes: self.knowledge_bytes,
            sleeps: self.sleeps,
            wakes: self.wakes,
            parks: self.parks,
            respecializations: self.respecializations.clone(),
            initial_parallelism: self.initial_parallelism,
            parallelism_changes: self.parallelism_changes.clone(),
            tokens_total: self.tokens_total,
            tokens_per_agent,
            directives_issued: self.directives_issued,
            directives_fulfilled: self.directives_fulfilled,
            directives_declined: self.directives_declined,
            liveness_nudges: self.liveness_nudges,
        }
    }
}

/// The report's projection of [`Metrics`] — rendered into `report.md` and
/// printed identically to stdout (ADR 0022, transcript §11).
///
/// The plain `Serialize` derive is the snapshot's `metrics` block (ADR 0030's
/// fourth view of ADR 0020's one-computation-N-views): Rust field names as-is,
/// no renames/skips; `outcome` is `null` or the `[reason, exit_code]` pair,
/// and the tuple fields serialize as arrays (pins §9).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunSummary {
    /// `run_finished`'s reason and exit code; `None` if the run has not
    /// finished (a summary is normally rendered after the bookend).
    pub outcome: Option<(RunFinishReason, u8)>,
    /// The one wall-clock value in metrics: the human duration line,
    /// computed from the first/last event `at` breadcrumbs (ADR 0022).
    pub wall_secs: f64,
    pub ticks: u64,
    pub team_agents: u32,
    pub meta_agents: u32,
    pub specialties_used: Vec<SpecialtySlug>,
    pub tasks_created: u64,
    pub tasks_completed: u64,
    pub tasks_cancelled: u64,
    pub messages_total: u64,
    pub messages_direct: u64,
    pub messages_team: u64,
    pub messages_broadcast: u64,
    pub knowledge_entries: u64,
    pub knowledge_messages: u64,
    pub knowledge_notes: u64,
    pub knowledge_completions: u64,
    pub knowledge_bytes: u64,
    pub sleeps: u64,
    pub wakes: u64,
    pub parks: u64,
    /// Each respecialization as `(agent, from, to)`, in event order.
    pub respecializations: Vec<(AgentId, SpecialtySlug, SpecialtySlug)>,
    pub initial_parallelism: u32,
    /// Each `parallelism_changed.effective`, in event order.
    pub parallelism_changes: Vec<u32>,
    pub tokens_total: u64,
    /// Per-agent token totals in handle order: orchestrator, agent-1..N,
    /// meta-1..M.
    pub tokens_per_agent: Vec<(AgentId, u64)>,
    pub directives_issued: u64,
    pub directives_fulfilled: u64,
    pub directives_declined: u64,
    pub liveness_nudges: u64,
}

impl RunSummary {
    /// Render the `## Run summary` markdown block exactly in the shape of
    /// transcript §11. A `--meta-agents 0` run renders the identical block
    /// minus the meta-interventions line.
    pub fn render(&self) -> String {
        let mut lines = vec!["## Run summary".to_string()];

        let outcome = match self.outcome {
            Some((reason, exit_code)) => {
                let reason = match reason {
                    RunFinishReason::CleanFinish => "CleanFinish".to_string(),
                    RunFinishReason::CapHit(cap) => format!("CapHit({})", cap.as_str()),
                    RunFinishReason::HarnessError => "HarnessError".to_string(),
                };
                format!("{reason} (exit {exit_code})")
            }
            None => "unknown (run not finished)".to_string(),
        };
        lines.push(format!("- Outcome: {outcome}"));
        lines.push(format!(
            "- Duration: {:.2}s wall · {} ticks",
            self.wall_secs, self.ticks
        ));
        let specialties = self
            .specialties_used
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "- Agents: {} team + {} meta · specialties used: {}",
            self.team_agents, self.meta_agents, specialties
        ));
        lines.push(format!(
            "- Tasks: created {} · completed {} · cancelled {}",
            self.tasks_created, self.tasks_completed, self.tasks_cancelled
        ));
        lines.push(format!(
            "- Messages: {} (direct {}, team {}, broadcast {})",
            self.messages_total, self.messages_direct, self.messages_team, self.messages_broadcast
        ));
        lines.push(format!(
            "- Knowledge: {} entries (Message {}, Note {}, TaskCompletion {}) · {} bytes",
            self.knowledge_entries,
            self.knowledge_messages,
            self.knowledge_notes,
            self.knowledge_completions,
            self.knowledge_bytes
        ));
        lines.push(format!(
            "- Sleeps {} · Wakes {} · Parks {}",
            self.sleeps, self.wakes, self.parks
        ));
        if self.respecializations.is_empty() {
            lines.push("- Respecializations: 0".to_string());
        } else {
            let list = self
                .respecializations
                .iter()
                .map(|(agent, from, to)| format!("{agent}: {from} → {to}"))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "- Respecializations: {} ({list})",
                self.respecializations.len()
            ));
        }
        let mut parallelism = self.initial_parallelism.to_string();
        for effective in &self.parallelism_changes {
            parallelism.push_str(&format!(" → {effective}"));
        }
        if !self.parallelism_changes.is_empty() {
            parallelism.push_str(" (meta set_parallelism)");
        }
        lines.push(format!("- Effective parallelism: {parallelism}"));
        let per_agent = self
            .tokens_per_agent
            .iter()
            .map(|(agent, total)| format!("{agent} {}", fmt_tokens(*total)))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "- Tokens: {} total — {per_agent}",
            fmt_tokens(self.tokens_total)
        ));
        if self.meta_agents > 0 {
            lines.push(format!(
                "- Meta interventions: issued {} · fulfilled {} · declined {}",
                self.directives_issued, self.directives_fulfilled, self.directives_declined
            ));
        }
        lines.push(format!("- Liveness nudges: {}", self.liveness_nudges));
        lines.join("\n")
    }
}

/// Token counts render as `X.Yk` at ≥1000, bare integer below (pins §3).
#[allow(clippy::cast_precision_loss)]
fn fmt_tokens(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// The digest's compressed team handle (`agent-3` → `a3`, transcript §1.9's
/// `malformed[a1:0 a2:0 a3:0]`).
fn short_handle(agent: &AgentId) -> String {
    agent
        .as_str()
        .strip_prefix("agent-")
        .map_or_else(|| agent.as_str().to_string(), |n| format!("a{n}"))
}

#[cfg(test)]
mod tests {
    use crate::event::test_fixtures::TRANSCRIPT_JSONL;

    use super::*;

    fn folded_transcript() -> Metrics {
        let mut metrics = Metrics::new();
        for line in TRANSCRIPT_JSONL.lines() {
            let event: Event = serde_json::from_str(line).unwrap();
            metrics.fold(&event);
        }
        metrics
    }

    /// The mandated fold test: the 34 literal transcript §8 events produce
    /// exactly the counts of the transcript's run summary (§11).
    #[test]
    fn folding_the_transcript_yields_the_pinned_counts() {
        let metrics = folded_transcript();
        let summary = metrics.summary();

        assert_eq!(summary.tasks_created, 2);
        assert_eq!(summary.tasks_completed, 2);
        assert_eq!(summary.tasks_cancelled, 0);

        assert_eq!(summary.messages_total, 3);
        assert_eq!(summary.messages_direct, 1);
        assert_eq!(summary.messages_team, 1);
        assert_eq!(summary.messages_broadcast, 1);

        assert_eq!(summary.knowledge_entries, 6);
        assert_eq!(summary.knowledge_messages, 3);
        assert_eq!(summary.knowledge_notes, 1, "one knowledge_written Note");
        assert_eq!(summary.knowledge_completions, 2);

        // issued 2; fulfilled = 1 judgment directive_fulfilled + 1
        // mechanical directive_issued (ADR 0022's fold); declined 0.
        assert_eq!(summary.directives_issued, 2);
        assert_eq!(summary.directives_fulfilled, 2);
        assert_eq!(summary.directives_declined, 0);

        assert_eq!(summary.parks, 0);
        assert_eq!(summary.sleeps, 0);
        assert_eq!(summary.wakes, 0);
        assert_eq!(summary.liveness_nudges, 0);

        assert_eq!(summary.respecializations.len(), 1);
        assert_eq!(summary.respecializations[0].0, AgentId::team(3));
        assert_eq!(summary.respecializations[0].1.as_str(), "generalist");
        assert_eq!(summary.respecializations[0].2.as_str(), "doc-reviewer");

        assert_eq!(summary.ticks, 4, "a tick is an orchestrator turn_completed");

        assert_eq!(summary.outcome, Some((RunFinishReason::CleanFinish, 0)));
        assert_eq!(summary.initial_parallelism, 3);
        assert_eq!(summary.parallelism_changes, vec![2]);
    }

    #[test]
    fn final_utilization_is_all_idle_with_agent_3_respecialized() {
        let metrics = folded_transcript();
        let (working, idle, asleep) = metrics.state_counts();
        assert_eq!((working, idle, asleep), (0, 3, 0));

        let digest = metrics.digest();
        assert!(digest.contains("- agent-1: Idle, generalist"), "{digest}");
        assert!(digest.contains("- agent-2: Idle, generalist"), "{digest}");
        assert!(digest.contains("- agent-3: Idle, doc-reviewer"), "{digest}");
    }

    #[test]
    fn the_three_projections_render() {
        let metrics = folded_transcript();

        // The transcript's tick-4 line was rendered before tick 4's own
        // turn_completed; the full fold includes it, so exactly one tick
        // (event 32) has elapsed since the last task_completed (event 30).
        assert_eq!(
            metrics.run_health_line(),
            "run-health: done 2/2 · agents 0W/3I/0S · mailbox depth 2 (max 2) · ticks-since-done 1"
        );

        let digest = metrics.digest();
        assert!(
            digest
                .starts_with("throughput: 2 task_completed / 33 EventIds · latency: work median "),
            "{digest}"
        );
        assert!(digest.contains("\nutilization:\n"), "{digest}");
        // msg 2 (broadcast) is still pending on agent-2/agent-3 since event 22.
        assert!(
            digest.contains("mailbox: depth 2, max 2, oldest-pending-age 11"),
            "{digest}"
        );
        assert!(
            digest.contains("faults: parks 0, malformed[a1:0 a2:0 a3:0]"),
            "{digest}"
        );
        assert!(
            digest.contains("directives: issued 2/ful 2/dec 0"),
            "{digest}"
        );

        let rendered = metrics.summary().render();
        assert!(rendered.starts_with("## Run summary\n"), "{rendered}");
        assert!(
            rendered.contains("- Outcome: CleanFinish (exit 0)"),
            "{rendered}"
        );
        assert!(rendered.contains("· 4 ticks"), "{rendered}");
        assert!(
            rendered
                .contains("- Agents: 3 team + 1 meta · specialties used: generalist, doc-reviewer"),
            "{rendered}"
        );
        assert!(
            rendered.contains("- Tasks: created 2 · completed 2 · cancelled 0"),
            "{rendered}"
        );
        assert!(
            rendered.contains("- Messages: 3 (direct 1, team 1, broadcast 1)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("- Knowledge: 6 entries (Message 3, Note 1, TaskCompletion 2)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("- Sleeps 0 · Wakes 0 · Parks 0"),
            "{rendered}"
        );
        assert!(
            rendered.contains("- Respecializations: 1 (agent-3: generalist → doc-reviewer)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("- Effective parallelism: 3 → 2 (meta set_parallelism)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("- Meta interventions: issued 2 · fulfilled 2 · declined 0"),
            "{rendered}"
        );
        assert!(rendered.contains("- Liveness nudges: 0"), "{rendered}");
        // Per-agent token order: orchestrator, agent-1..3, meta-1. Totals
        // are the exact usage fold of the §8 events (§11's numbers are
        // illustrative; the structure is the pin).
        let tokens_line = rendered
            .lines()
            .find(|l| l.starts_with("- Tokens: "))
            .unwrap();
        assert_eq!(
            tokens_line,
            "- Tokens: 14.7k total — orchestrator 5.6k, agent-1 3.8k, agent-2 2.7k, agent-3 898, meta-1 1.8k"
        );
    }

    /// The snapshot's `metrics` block (ADR 0030, pins §9): a plain
    /// `Serialize` derive — Rust field names as-is, `outcome` the
    /// `[reason, exit_code]` pair (reason per §7), tuple fields as arrays.
    #[test]
    fn run_summary_serializes_to_the_pinned_snapshot_shape() {
        let value = serde_json::to_value(folded_transcript().summary()).unwrap();

        // outcome is the [reason, exit_code] pair, reason per §7.
        assert_eq!(value["outcome"], serde_json::json!(["CleanFinish", 0]));
        // respecializations: [agent, from, to] tuples serialize as arrays.
        assert_eq!(
            value["respecializations"],
            serde_json::json!([["agent-3", "generalist", "doc-reviewer"]])
        );
        // tokens_per_agent: [handle, n] tuples serialize as arrays, in the
        // pinned handle order.
        let tokens = value["tokens_per_agent"].as_array().unwrap();
        assert_eq!(tokens[0][0], "orchestrator");
        assert_eq!(tokens[1][0], "agent-1");
        assert!(tokens.iter().all(|pair| pair[1].is_u64()));
        // Field names are as-is, no renames.
        for key in [
            "wall_secs",
            "ticks",
            "team_agents",
            "meta_agents",
            "tasks_completed",
            "tokens_total",
            "directives_issued",
            "liveness_nudges",
        ] {
            assert!(value.get(key).is_some(), "missing field {key}");
        }

        // A not-yet-finished summary serializes `outcome` as null.
        let mut pending = folded_transcript().summary();
        pending.outcome = None;
        assert_eq!(
            serde_json::to_value(pending).unwrap()["outcome"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn work_latency_median_is_in_event_id_deltas() {
        let metrics = folded_transcript();
        // task 2: claimed @8, completed @18 → 10; task 1: claimed @5,
        // completed @30 → 25; even count → midpoint 17.
        assert_eq!(metrics.work_latency_median(), Some(17));
    }

    #[test]
    fn token_formatting_matches_the_pinned_shapes() {
        assert_eq!(fmt_tokens(950), "950");
        assert_eq!(fmt_tokens(1000), "1.0k");
        assert_eq!(fmt_tokens(2600), "2.6k");
        assert_eq!(fmt_tokens(16549), "16.5k");
    }

    #[test]
    fn meta_agents_zero_renders_the_block_minus_the_meta_line() {
        let mut summary = folded_transcript().summary();
        summary.meta_agents = 0;
        let rendered = summary.render();
        assert!(!rendered.contains("- Meta interventions:"), "{rendered}");
        assert!(rendered.contains("- Liveness nudges: 0"), "{rendered}");
    }
}
