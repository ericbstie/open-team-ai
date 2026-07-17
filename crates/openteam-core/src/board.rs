//! The task board and teams: flat, orchestrator-authored, pull-only claiming
//! (ADR 0010) with teams as runtime entities (ADR 0009).
//!
//! Only the orchestrator creates, unassigns, and cancels tasks; team agents
//! claim open tasks themselves — at most one each, team-eligibility checked
//! at claim time, first claim wins. There are no dependency edges, no
//! push-assignment, and **no Failed state** — a struggling task is released
//! (a meta-visible signal), never failed.
//!
//! Every transition returns `Result<_, BoardRejection>`; a rejection is a
//! well-formed call the domain refused (ADR 0017's `rejected` tool outcome),
//! carrying a snake_case domain code plus a human message.

use std::collections::BTreeMap;

use openteam_wire::AgentId;
use serde::{Deserialize, Serialize};

use crate::ids::{EventId, KnowledgeEntryId, TaskId, TeamId};

/// A task's lifecycle position (ADR 0010). Serializes externally tagged for
/// `board.json` (pins §7): `"Open"` / `{"Claimed":{"by":…}}` /
/// `{"Done":{"result":…,"result_ref":…}}` / `{"Cancelled":{"reason":…}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    Open,
    Claimed {
        by: AgentId,
    },
    /// Inlines both the result text (so `board.json` is self-readable) and
    /// its knowledge-store entry (ADR 0022).
    Done {
        result: String,
        result_ref: KnowledgeEntryId,
    },
    Cancelled {
        reason: String,
    },
}

impl TaskState {
    /// The `<state>` slot of the pinned board-digest line grammar
    /// (ADR 0016): `Open` / `Claimed by <agent>` / `Done` / `Cancelled`.
    pub fn digest_label(&self) -> String {
        match self {
            Self::Open => "Open".into(),
            Self::Claimed { by } => format!("Claimed by {by}"),
            Self::Done { .. } => "Done".into(),
            Self::Cancelled { .. } => "Cancelled".into(),
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done { .. } | Self::Cancelled { .. })
    }
}

/// A unit of work on the task board (ADR 0010/0022).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub title: String,
    pub description: String,
    /// Always the orchestrator in v1 — task authorship is orchestrator-only.
    pub created_by: AgentId,
    /// The `task_created` event.
    pub origin_event: EventId,
    /// Claim-eligibility tag: untagged tasks are claimable by any team agent.
    pub team: Option<TeamId>,
    pub state: TaskState,
}

impl Task {
    /// The pinned board-digest task line (ADR 0016, pins §3):
    /// `- task <id> [<state>] team:<tag|->  "<title>"` — two spaces before
    /// the quoted title.
    pub fn digest_line(&self) -> String {
        let tag = self.team.as_ref().map_or("-", TeamId::as_str);
        format!(
            "- task {} [{}] team:{}  \"{}\"",
            self.id,
            self.state.digest_label(),
            tag,
            self.title
        )
    }
}

/// A runtime team entity: a routable message scope plus a claim-eligibility
/// scope (ADR 0009). Dissolved teams stay recorded for `board.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Team {
    pub id: TeamId,
    pub members: Vec<AgentId>,
    pub dissolved: bool,
}

/// The membership deltas of a declarative `set_team_members` replace — the
/// join/leave record carried on the `team_members_set` event (ADR 0022).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MembershipDelta {
    pub added: Vec<AgentId>,
    pub removed: Vec<AgentId>,
}

/// A well-formed call the board refused: a snake_case domain code (carried
/// into the `rejected` tool outcome, ADR 0017) plus a human message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct BoardRejection {
    pub code: &'static str,
    pub message: String,
}

impl BoardRejection {
    fn new(code: &'static str, message: String) -> Self {
        Self { code, message }
    }
}

/// The shared registry of tasks and teams — the orchestrator's steering
/// surface (ADR 0010). Ids are allocated by the runtime's single serial
/// write path (ADR 0011) and passed in.
#[derive(Debug, Clone, Default)]
pub struct Board {
    tasks: BTreeMap<TaskId, Task>,
    teams: Vec<Team>,
}

impl Board {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- accessors -------------------------------------------------------

    pub fn task(&self, id: TaskId) -> Option<&Task> {
        self.tasks.get(&id)
    }

    /// All tasks in id (== creation) order.
    pub fn tasks(&self) -> impl Iterator<Item = &Task> {
        self.tasks.values()
    }

    /// The task `agent` currently claims, if any (≤1 by invariant).
    pub fn claimed_by(&self, agent: &AgentId) -> Option<&Task> {
        self.tasks
            .values()
            .find(|t| matches!(&t.state, TaskState::Claimed { by } if by == agent))
    }

    pub fn team(&self, id: &TeamId) -> Option<&Team> {
        self.teams.iter().find(|t| &t.id == id)
    }

    /// All teams in formation order, dissolved included (for `board.json`).
    pub fn teams(&self) -> &[Team] {
        &self.teams
    }

    /// Claim eligibility (ADR 0010): the task is untagged, or the agent is a
    /// member of its team — checked at claim time.
    pub fn is_eligible(&self, agent: &AgentId, task: &Task) -> bool {
        match &task.team {
            None => true,
            Some(team) => self.team(team).is_some_and(|t| t.members.contains(agent)),
        }
    }

    /// The Open/Claimed tasks blocking `finish_run` — which is rejected
    /// while any remain, enumerating them (ADR 0006/0010).
    pub fn finish_blockers(&self) -> Vec<&Task> {
        self.tasks
            .values()
            .filter(|t| !t.state.is_terminal())
            .collect()
    }

    // ---- task transitions ------------------------------------------------

    /// Author a new Open task (orchestrator-only by verb registry). The
    /// `id` comes from the runtime's 1-based TaskId counter.
    pub fn create_task(
        &mut self,
        id: TaskId,
        title: impl Into<String>,
        description: impl Into<String>,
        created_by: AgentId,
        origin_event: EventId,
        team: Option<TeamId>,
    ) -> Result<&Task, BoardRejection> {
        if self.tasks.contains_key(&id) {
            return Err(BoardRejection::new(
                "duplicate_task",
                format!("task {id} already exists"),
            ));
        }
        if let Some(tag) = &team {
            let live = self.team(tag).is_some_and(|t| !t.dissolved);
            if !live {
                return Err(BoardRejection::new(
                    "unknown_team",
                    format!("team {tag} is not a live team"),
                ));
            }
        }
        let task = Task {
            id,
            title: title.into(),
            description: description.into(),
            created_by,
            origin_event,
            team,
            state: TaskState::Open,
        };
        Ok(self.tasks.entry(id).or_insert(task))
    }

    /// A team agent takes exclusive ownership of an Open task — first claim
    /// wins, at most one claimed task per agent, eligibility checked now
    /// (ADR 0010).
    pub fn claim(&mut self, agent: &AgentId, id: TaskId) -> Result<&Task, BoardRejection> {
        let Some(task) = self.tasks.get(&id) else {
            return Err(BoardRejection::new(
                "unknown_task",
                format!("task {id} does not exist"),
            ));
        };
        if task.state != TaskState::Open {
            return Err(BoardRejection::new(
                "task_not_open",
                format!("task {id} is not Open ({})", task.state.digest_label()),
            ));
        }
        if let Some(held) = self.claimed_by(agent) {
            return Err(BoardRejection::new(
                "already_claimed_other",
                format!(
                    "{agent} already claims task {} (at most one claimed task per agent)",
                    held.id
                ),
            ));
        }
        if !self.is_eligible(agent, task) {
            let tag = task.team.as_ref().map_or("-", TeamId::as_str);
            return Err(BoardRejection::new(
                "not_eligible",
                format!("{agent} is not a member of team {tag}, so cannot claim task {id}"),
            ));
        }
        let task = self.tasks.get_mut(&id).ok_or_else(|| {
            BoardRejection::new("unknown_task", format!("task {id} does not exist"))
        })?;
        task.state = TaskState::Claimed { by: agent.clone() };
        Ok(task)
    }

    /// The claimant returns its claimed task to Open (CONTEXT.md: Release).
    /// The optional reason rides on the `task_released` event, not the board.
    /// Returns the released task's id.
    pub fn release(&mut self, claimant: &AgentId) -> Result<TaskId, BoardRejection> {
        let id = self.claimed_by(claimant).map(|t| t.id).ok_or_else(|| {
            BoardRejection::new(
                "task_not_claimed",
                format!("{claimant} has no claimed task to release"),
            )
        })?;
        if let Some(task) = self.tasks.get_mut(&id) {
            task.state = TaskState::Open;
        }
        Ok(id)
    }

    /// The orchestrator forcibly returns a claimed task to Open — the
    /// reallocation and pre-respecialization move (CONTEXT.md: Unassign).
    /// Returns the previous claimant (the `task_unassigned` payload).
    pub fn unassign(&mut self, id: TaskId) -> Result<AgentId, BoardRejection> {
        let Some(task) = self.tasks.get_mut(&id) else {
            return Err(BoardRejection::new(
                "unknown_task",
                format!("task {id} does not exist"),
            ));
        };
        let TaskState::Claimed { by } = &task.state else {
            return Err(BoardRejection::new(
                "task_not_claimed",
                format!("task {id} is not Claimed ({})", task.state.digest_label()),
            ));
        };
        let prev_claimant = by.clone();
        task.state = TaskState::Open;
        Ok(prev_claimant)
    }

    /// The claimant marks its claimed task Done, recording the result text
    /// and its knowledge-store entry (`result_ref` from the runtime's
    /// KnowledgeEntryId counter, ADR 0014). Returns the completed task's id.
    pub fn complete(
        &mut self,
        claimant: &AgentId,
        result: impl Into<String>,
        result_ref: KnowledgeEntryId,
    ) -> Result<TaskId, BoardRejection> {
        let id = self.claimed_by(claimant).map(|t| t.id).ok_or_else(|| {
            BoardRejection::new(
                "task_not_claimed",
                format!("{claimant} has no claimed task to complete"),
            )
        })?;
        if let Some(task) = self.tasks.get_mut(&id) {
            task.state = TaskState::Done {
                result: result.into(),
                result_ref,
            };
        }
        Ok(id)
    }

    /// The orchestrator cancels an Open task. A Claimed task must be
    /// unassigned first (the deliberate-transition contract, ADR 0010).
    pub fn cancel(&mut self, id: TaskId, reason: impl Into<String>) -> Result<(), BoardRejection> {
        let Some(task) = self.tasks.get_mut(&id) else {
            return Err(BoardRejection::new(
                "unknown_task",
                format!("task {id} does not exist"),
            ));
        };
        if task.state != TaskState::Open {
            return Err(BoardRejection::new(
                "task_not_open",
                format!("task {id} is not Open ({})", task.state.digest_label()),
            ));
        }
        task.state = TaskState::Cancelled {
            reason: reason.into(),
        };
        Ok(())
    }

    // ---- teams -------------------------------------------------------------

    /// Form a new team (ADR 0009). Team ids are never reused — re-forming a
    /// dissolved team's id is rejected too.
    pub fn form_team(
        &mut self,
        id: TeamId,
        members: Vec<AgentId>,
    ) -> Result<&Team, BoardRejection> {
        if self.team(&id).is_some() {
            return Err(BoardRejection::new(
                "duplicate_team",
                format!("team {id} already exists"),
            ));
        }
        self.teams.push(Team {
            id,
            members,
            dissolved: false,
        });
        self.teams
            .last()
            .ok_or_else(|| BoardRejection::new("unknown_team", "team vanished".into()))
    }

    /// Dissolve a team, releasing both its scopes — rejected while live
    /// (Open/Claimed) team-tagged tasks remain (ADR 0009, #7).
    pub fn dissolve_team(&mut self, id: &TeamId) -> Result<(), BoardRejection> {
        let Some(team) = self.teams.iter().find(|t| &t.id == id) else {
            return Err(BoardRejection::new(
                "unknown_team",
                format!("team {id} does not exist"),
            ));
        };
        if team.dissolved {
            return Err(BoardRejection::new(
                "team_dissolved",
                format!("team {id} is already dissolved"),
            ));
        }
        let live: Vec<TaskId> = self
            .tasks
            .values()
            .filter(|t| t.team.as_ref() == Some(id) && !t.state.is_terminal())
            .map(|t| t.id)
            .collect();
        if !live.is_empty() {
            let list = live
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(BoardRejection::new(
                "team_has_live_tasks",
                format!("team {id} still has live tasks: {list}"),
            ));
        }
        if let Some(team) = self.teams.iter_mut().find(|t| &t.id == id) {
            team.dissolved = true;
        }
        Ok(())
    }

    /// Declarative full-membership replace (ADR 0009/0022), returning the
    /// added/removed deltas for the `team_members_set` event.
    pub fn set_team_members(
        &mut self,
        id: &TeamId,
        members: Vec<AgentId>,
    ) -> Result<MembershipDelta, BoardRejection> {
        let Some(team) = self.teams.iter_mut().find(|t| &t.id == id) else {
            return Err(BoardRejection::new(
                "unknown_team",
                format!("team {id} does not exist"),
            ));
        };
        if team.dissolved {
            return Err(BoardRejection::new(
                "team_dissolved",
                format!("team {id} is dissolved"),
            ));
        }
        let added = members
            .iter()
            .filter(|m| !team.members.contains(m))
            .cloned()
            .collect();
        let removed = team
            .members
            .iter()
            .filter(|m| !members.contains(m))
            .cloned()
            .collect();
        team.members = members;
        Ok(MembershipDelta { added, removed })
    }

    // ---- digest -------------------------------------------------------------

    /// The pinned board-digest task lines (ADR 0016), one per task in id
    /// order. `for_agent: None` renders the full board (the orchestrator's
    /// view — the runtime folds the run-health line after these);
    /// `Some(agent)` renders the claimed-plus-eligible slice a team agent
    /// sees: its claimed task plus the Open tasks it is eligible for.
    pub fn digest_lines(&self, for_agent: Option<&AgentId>) -> Vec<String> {
        self.tasks
            .values()
            .filter(|task| match for_agent {
                None => true,
                Some(agent) => match &task.state {
                    TaskState::Claimed { by } => by == agent,
                    TaskState::Open => self.is_eligible(agent, task),
                    _ => false,
                },
            })
            .map(Task::digest_line)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(tag: &str) -> TeamId {
        TeamId::parse(tag).unwrap()
    }

    /// A board with team t1 = {agent-1, agent-2} and two tasks: task 1
    /// tagged t1, task 2 untagged.
    fn seeded_board() -> Board {
        let mut board = Board::new();
        board
            .form_team(t("t1"), vec![AgentId::team(1), AgentId::team(2)])
            .unwrap();
        board
            .create_task(
                TaskId::new(1),
                "Draft the setup section",
                "Install + build/test steps for a new contributor.",
                AgentId::orchestrator(),
                EventId::new(2),
                Some(t("t1")),
            )
            .unwrap();
        board
            .create_task(
                TaskId::new(2),
                "Draft the architecture overview",
                "One-paragraph crate map.",
                AgentId::orchestrator(),
                EventId::new(3),
                None,
            )
            .unwrap();
        board
    }

    #[test]
    fn first_claim_wins_and_the_loser_is_rejected_task_not_open() {
        let mut board = seeded_board();
        board.claim(&AgentId::team(1), TaskId::new(1)).unwrap();

        let lost = board.claim(&AgentId::team(2), TaskId::new(1)).unwrap_err();
        assert_eq!(lost.code, "task_not_open");
        assert_eq!(lost.message, "task 1 is not Open (Claimed by agent-1)");
    }

    #[test]
    fn at_most_one_claimed_task_per_agent() {
        let mut board = seeded_board();
        board.claim(&AgentId::team(1), TaskId::new(1)).unwrap();
        let second = board.claim(&AgentId::team(1), TaskId::new(2)).unwrap_err();
        assert_eq!(second.code, "already_claimed_other");
    }

    #[test]
    fn eligibility_is_team_membership_checked_at_claim_time() {
        let mut board = seeded_board();
        // agent-3 is not in t1: task 1 rejected, untagged task 2 fine.
        let rejected = board.claim(&AgentId::team(3), TaskId::new(1)).unwrap_err();
        assert_eq!(rejected.code, "not_eligible");
        board.claim(&AgentId::team(3), TaskId::new(2)).unwrap();

        // Membership is read at claim time: add agent-3 to t1 and reclaim.
        let mut board = seeded_board();
        board
            .set_team_members(&t("t1"), vec![AgentId::team(1), AgentId::team(3)])
            .unwrap();
        board.claim(&AgentId::team(3), TaskId::new(1)).unwrap();
    }

    #[test]
    fn unknown_task_is_rejected() {
        let mut board = seeded_board();
        let missing = board.claim(&AgentId::team(1), TaskId::new(9)).unwrap_err();
        assert_eq!(missing.code, "unknown_task");
        assert_eq!(
            board.unassign(TaskId::new(9)).unwrap_err().code,
            "unknown_task"
        );
        assert_eq!(
            board.cancel(TaskId::new(9), "x").unwrap_err().code,
            "unknown_task"
        );
    }

    #[test]
    fn release_reopens_and_requires_a_claim() {
        let mut board = seeded_board();
        board.claim(&AgentId::team(1), TaskId::new(1)).unwrap();
        assert_eq!(board.release(&AgentId::team(1)).unwrap(), TaskId::new(1));
        assert_eq!(board.task(TaskId::new(1)).unwrap().state, TaskState::Open);
        // Repeated releases are a signal, not an error — reclaim and re-release.
        board.claim(&AgentId::team(2), TaskId::new(1)).unwrap();
        assert_eq!(board.release(&AgentId::team(2)).unwrap(), TaskId::new(1));
        // No claim → rejected.
        assert_eq!(
            board.release(&AgentId::team(1)).unwrap_err().code,
            "task_not_claimed"
        );
    }

    #[test]
    fn unassign_returns_the_previous_claimant() {
        let mut board = seeded_board();
        board.claim(&AgentId::team(2), TaskId::new(1)).unwrap();
        assert_eq!(board.unassign(TaskId::new(1)).unwrap(), AgentId::team(2));
        assert_eq!(board.task(TaskId::new(1)).unwrap().state, TaskState::Open);
        assert_eq!(
            board.unassign(TaskId::new(1)).unwrap_err().code,
            "task_not_claimed"
        );
    }

    #[test]
    fn complete_records_result_and_result_ref() {
        let mut board = seeded_board();
        board.claim(&AgentId::team(1), TaskId::new(1)).unwrap();
        let done = board
            .complete(
                &AgentId::team(1),
                "Setup section drafted.",
                KnowledgeEntryId::new(6),
            )
            .unwrap();
        assert_eq!(done, TaskId::new(1));
        assert_eq!(
            board.task(TaskId::new(1)).unwrap().state,
            TaskState::Done {
                result: "Setup section drafted.".into(),
                result_ref: KnowledgeEntryId::new(6),
            }
        );
        assert_eq!(
            board
                .complete(&AgentId::team(1), "again", KnowledgeEntryId::new(7))
                .unwrap_err()
                .code,
            "task_not_claimed"
        );
    }

    #[test]
    fn cancel_is_open_only_and_there_is_no_failed_state() {
        let mut board = seeded_board();
        board.cancel(TaskId::new(2), "descoped").unwrap();
        assert_eq!(
            board.task(TaskId::new(2)).unwrap().state,
            TaskState::Cancelled {
                reason: "descoped".into()
            }
        );
        // Claimed tasks must be unassigned first.
        board.claim(&AgentId::team(1), TaskId::new(1)).unwrap();
        assert_eq!(
            board.cancel(TaskId::new(1), "x").unwrap_err().code,
            "task_not_open"
        );
        // Terminal states stay terminal.
        assert_eq!(
            board.cancel(TaskId::new(2), "x").unwrap_err().code,
            "task_not_open"
        );
    }

    #[test]
    fn finish_blockers_enumerates_open_and_claimed_tasks() {
        let mut board = seeded_board();
        board.claim(&AgentId::team(1), TaskId::new(1)).unwrap();
        let blockers: Vec<TaskId> = board.finish_blockers().iter().map(|t| t.id).collect();
        assert_eq!(blockers, vec![TaskId::new(1), TaskId::new(2)]);

        board
            .complete(&AgentId::team(1), "done", KnowledgeEntryId::new(1))
            .unwrap();
        board.cancel(TaskId::new(2), "descoped").unwrap();
        assert!(board.finish_blockers().is_empty());
    }

    #[test]
    fn duplicate_and_unknown_teams_are_rejected() {
        let mut board = seeded_board();
        assert_eq!(
            board.form_team(t("t1"), vec![]).unwrap_err().code,
            "duplicate_team"
        );
        assert_eq!(
            board
                .create_task(
                    TaskId::new(3),
                    "x",
                    "y",
                    AgentId::orchestrator(),
                    EventId::new(9),
                    Some(t("t9")),
                )
                .unwrap_err()
                .code,
            "unknown_team"
        );
        assert_eq!(
            board.dissolve_team(&t("t9")).unwrap_err().code,
            "unknown_team"
        );
        assert_eq!(
            board.set_team_members(&t("t9"), vec![]).unwrap_err().code,
            "unknown_team"
        );
    }

    #[test]
    fn dissolve_is_rejected_while_live_team_tasks_remain() {
        let mut board = seeded_board();
        let live = board.dissolve_team(&t("t1")).unwrap_err();
        assert_eq!(live.code, "team_has_live_tasks");
        assert!(live.message.contains("1"), "enumerates the blockers");

        board.claim(&AgentId::team(1), TaskId::new(1)).unwrap();
        board
            .complete(&AgentId::team(1), "done", KnowledgeEntryId::new(1))
            .unwrap();
        board.dissolve_team(&t("t1")).unwrap();
        assert!(board.team(&t("t1")).unwrap().dissolved);
        assert_eq!(
            board.dissolve_team(&t("t1")).unwrap_err().code,
            "team_dissolved"
        );
        // Ids are never reused.
        assert_eq!(
            board.form_team(t("t1"), vec![]).unwrap_err().code,
            "duplicate_team"
        );
    }

    #[test]
    fn set_team_members_is_a_declarative_replace_with_deltas() {
        let mut board = seeded_board();
        let delta = board
            .set_team_members(&t("t1"), vec![AgentId::team(2), AgentId::team(3)])
            .unwrap();
        assert_eq!(delta.added, vec![AgentId::team(3)]);
        assert_eq!(delta.removed, vec![AgentId::team(1)]);
        assert_eq!(
            board.team(&t("t1")).unwrap().members,
            vec![AgentId::team(2), AgentId::team(3)]
        );
    }

    #[test]
    fn digest_lines_render_the_pinned_grammar() {
        let mut board = seeded_board();
        board.claim(&AgentId::team(1), TaskId::new(1)).unwrap();
        assert_eq!(
            board.digest_lines(None),
            vec![
                "- task 1 [Claimed by agent-1] team:t1  \"Draft the setup section\"",
                "- task 2 [Open] team:-  \"Draft the architecture overview\"",
            ]
        );

        board
            .complete(&AgentId::team(1), "done", KnowledgeEntryId::new(3))
            .unwrap();
        board.cancel(TaskId::new(2), "descoped").unwrap();
        assert_eq!(
            board.digest_lines(None),
            vec![
                "- task 1 [Done] team:t1  \"Draft the setup section\"",
                "- task 2 [Cancelled] team:-  \"Draft the architecture overview\"",
            ]
        );
    }

    #[test]
    fn digest_slice_is_claimed_plus_eligible_for_a_team_agent() {
        let mut board = seeded_board();
        board
            .create_task(
                TaskId::new(3),
                "Team-only follow-up",
                "d",
                AgentId::orchestrator(),
                EventId::new(9),
                Some(t("t1")),
            )
            .unwrap();
        board.claim(&AgentId::team(1), TaskId::new(1)).unwrap();

        // agent-1 (in t1, claiming task 1): its claimed task + open eligible.
        assert_eq!(
            board.digest_lines(Some(&AgentId::team(1))),
            vec![
                "- task 1 [Claimed by agent-1] team:t1  \"Draft the setup section\"",
                "- task 2 [Open] team:-  \"Draft the architecture overview\"",
                "- task 3 [Open] team:t1  \"Team-only follow-up\"",
            ]
        );
        // agent-3 (not in t1): only the untagged open task.
        assert_eq!(
            board.digest_lines(Some(&AgentId::team(3))),
            vec!["- task 2 [Open] team:-  \"Draft the architecture overview\""]
        );
    }
}
