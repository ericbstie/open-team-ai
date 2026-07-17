//! The legibility-pairing test (ADR 0025): the "two halves of one contract".
//!
//! Renders a known world through the REAL core assembler, feeds the exact
//! rendered request to the REAL mock parser and built-in arc, and asserts
//! the arc reads the intended WORLD STATE (task states, claimed-task
//! presence, work-action count, directive kind+args) — the pairing, never
//! exact generated text. The bin is the composition root where both crates
//! meet, so this lives here.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code

use openteam_core::{
    AssembleView, Board, ContextPolicy, Directive, DirectiveKind, DirectiveState, DirectiveTier,
    EventId, KnowledgeEntryId, Metrics, SpecialtyProfile, TaskId, TeamId, ToolRegistry, assemble,
    claimed_task_line, window_line,
};
use openteam_mock::{BehaviorModel as _, BuiltinArc, RenderedWorld, TaskState};
use openteam_wire::{
    AgentId, CharCountTokenizer, ChatCompletionRequest, ChatMessage, MessageContent, Role,
    ToolChoice, ToolChoiceMode, WireIdentity,
};

const SEED: u64 = 42;

/// Build the exact wire request core's runtime builds from an assembled
/// prompt: two messages plus the role's verbatim tool defs.
fn request(
    role: Role,
    view: &AssembleView,
    specialty: Option<&SpecialtyProfile>,
) -> ChatCompletionRequest {
    let registry = ToolRegistry::new();
    let prompt = assemble(
        &ContextPolicy::for_role(role, None),
        role,
        specialty,
        view,
        &CharCountTokenizer,
    );
    ChatCompletionRequest {
        model: "openteam-mock".into(),
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
        tools: Some(registry.tool_defs(role).to_vec()),
        tool_choice: Some(ToolChoice::Mode(ToolChoiceMode::Auto)),
        parallel_tool_calls: Some(role != Role::MetaAgent),
        user: None,
        safety_identifier: None,
        prompt_cache_key: None,
        stream: None,
        n: None,
    }
}

fn identity(user: &str, call_seq: u64) -> WireIdentity {
    WireIdentity {
        user: user.into(),
        call_seq,
        seed: SEED,
    }
}

/// A board with one Claimed, one Done, one Open task on team t1.
fn known_board() -> Board {
    let mut board = Board::new();
    let team = TeamId::parse("t1").unwrap();
    let members = vec![AgentId::team(1), AgentId::team(2)];
    board.form_team(team.clone(), members).unwrap();
    for (id, title) in [
        (1, "Draft the setup section"),
        (2, "Draft the overview"),
        (3, "Review both"),
    ] {
        board
            .create_task(
                TaskId::new(id),
                title,
                "description",
                AgentId::orchestrator(),
                EventId::new(id),
                Some(team.clone()),
            )
            .unwrap();
    }
    board.claim(&AgentId::team(1), TaskId::new(1)).unwrap();
    board.claim(&AgentId::team(2), TaskId::new(2)).unwrap();
    board
        .complete(&AgentId::team(2), "done", KnowledgeEntryId::new(1))
        .unwrap();
    board
}

#[test]
fn the_mock_reads_the_rendered_board_exactly() {
    let board = known_board();
    let view = AssembleView {
        goal: "Write a short onboarding guide.".into(),
        board_lines: board.digest_lines(None),
        run_health: Some(
            "run-health: done 1/3 · agents 1W/1I/0S · mailbox depth 0 (max 0) · ticks-since-done 0"
                .into(),
        ),
        directive_lines: vec![
            Directive {
                id: openteam_core::DirectiveId::new(1),
                tier: DirectiveTier::Judgment,
                kind: DirectiveKind::ProposeRespecialize,
                args: serde_json::json!({"agent": "agent-2", "specialty": "doc-reviewer"}),
                from: AgentId::meta(1),
                state: DirectiveState::Pending,
            }
            .directives_line(),
        ],
        ..AssembleView::default()
    };
    let req = request(Role::Orchestrator, &view, None);
    let world = RenderedWorld::parse(&req);

    let digest = world.board.expect("the board digest parses");
    assert_eq!(digest.tasks.len(), 3);
    assert_eq!(digest.tasks[0].id, 1);
    assert_eq!(
        digest.tasks[0].state,
        TaskState::Claimed {
            by: "agent-1".into()
        },
        "claimant read from the rendered line"
    );
    assert_eq!(digest.tasks[1].state, TaskState::Done);
    assert_eq!(digest.tasks[2].state, TaskState::Open);
    assert_eq!(digest.tasks[0].team.as_deref(), Some("t1"));
    assert_eq!(digest.tasks[0].title, "Draft the setup section");

    // The Directives line carries id + kind + args (F4a): the arc can act.
    assert_eq!(world.directives.len(), 1);
    let directive = &world.directives[0];
    assert_eq!(directive.id, 1);
    assert!(directive.is_pending());
    assert_eq!(directive.kind, "propose_respecialize");
    assert_eq!(directive.args.get("agent"), Some("agent-2"));
    assert_eq!(directive.args.get("specialty"), Some("doc-reviewer"));
}

#[test]
fn empty_board_drives_bounded_decomposition() {
    let view = AssembleView {
        goal: "Write a short onboarding guide.".into(),
        run_health: Some(
            "run-health: done 0/0 · agents 0W/3I/0S · mailbox depth 0 (max 0) · ticks-since-done 0"
                .into(),
        ),
        ..AssembleView::default()
    };
    let req = request(Role::Orchestrator, &view, None);
    let decision = BuiltinArc::new().chat(&req, &identity("orchestrator", 0));
    let calls = decision.message.tool_calls.expect("decompose emits calls");
    let creates = calls
        .iter()
        .filter(|c| c.function.name == "create_task")
        .count() as u64;
    assert!(creates >= 1, "n == 0 means decompose");
    assert!(creates <= BuiltinArc::task_budget(SEED), "bounded by T");
    for call in &calls {
        assert!(
            ["create_task", "form_team"].contains(&call.function.name.as_str()),
            "decompose batch only authors work: {}",
            call.function.name
        );
    }
}

#[test]
fn all_terminal_board_drives_finish_run() {
    let mut board = known_board();
    board.unassign(TaskId::new(1)).unwrap();
    board.claim(&AgentId::team(1), TaskId::new(1)).unwrap();
    board
        .complete(&AgentId::team(1), "done", KnowledgeEntryId::new(2))
        .unwrap();
    board.claim(&AgentId::team(1), TaskId::new(3)).unwrap();
    board
        .complete(&AgentId::team(1), "done", KnowledgeEntryId::new(3))
        .unwrap();
    let view = AssembleView {
        goal: "Write a short onboarding guide.".into(),
        board_lines: board.digest_lines(None),
        run_health: Some(
            "run-health: done 3/3 · agents 0W/2I/0S · mailbox depth 0 (max 0) · ticks-since-done 0"
                .into(),
        ),
        ..AssembleView::default()
    };
    let req = request(Role::Orchestrator, &view, None);
    let decision = BuiltinArc::new().chat(&req, &identity("orchestrator", 10));
    let calls = decision.message.tool_calls.expect("finish emits the call");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name, "finish_run");
}

#[test]
fn work_quota_counted_from_the_rendered_window() {
    let agent_user = "team-agent:agent-1:generalist";
    let quota = BuiltinArc::work_quota(SEED, "agent-1", 1);
    assert!((1..=3).contains(&quota), "W in [1..3]");

    let board = known_board();
    let claimed = claimed_task_line(board.task(TaskId::new(1)).unwrap());
    let make_view = |work_actions: u64| {
        let mut window_lines = vec![window_line(1, "claim_task", "task:1", "ok")];
        for turn in 0..work_actions {
            window_lines.push(window_line(turn + 2, "write_knowledge", "\"note…\"", "ok"));
        }
        AssembleView {
            goal: "Write a short onboarding guide.".into(),
            board_lines: board.digest_lines(Some(&AgentId::team(1))),
            claimed_line: Some(claimed.clone()),
            window_lines,
            ..AssembleView::default()
        }
    };

    // Below quota: exactly one more work-action, never complete_task.
    let req = request(
        Role::TeamAgent,
        &make_view(quota - 1),
        Some(&SpecialtyProfile::generalist()),
    );
    let parsed = RenderedWorld::parse(&req);
    assert_eq!(
        parsed.recent_activity.as_ref().unwrap().work_actions as u64,
        quota - 1,
        "work-actions counted from the window, claim excluded"
    );
    let decision = BuiltinArc::new().chat(&req, &identity(agent_user, 20));
    let calls = decision.message.tool_calls.expect("a work action");
    assert_eq!(calls.len(), 1);
    assert!(
        ["write_knowledge", "post_message", "search_knowledge"]
            .contains(&calls[0].function.name.as_str()),
        "one seeded work-action, got {}",
        calls[0].function.name
    );

    // At quota: complete.
    let req = request(
        Role::TeamAgent,
        &make_view(quota),
        Some(&SpecialtyProfile::generalist()),
    );
    let decision = BuiltinArc::new().chat(&req, &identity(agent_user, 22));
    let calls = decision.message.tool_calls.expect("completes at quota");
    assert_eq!(calls[0].function.name, "complete_task");
}

#[test]
fn degraded_window_forces_completion_never_blocks() {
    let board = known_board();
    let view = AssembleView {
        goal: "Write a short onboarding guide.".into(),
        board_lines: board.digest_lines(Some(&AgentId::team(1))),
        claimed_line: Some(claimed_task_line(board.task(TaskId::new(1)).unwrap())),
        // A window degraded below any visible work-action.
        window_lines: vec!["(degraded: 3 dropped)".into()],
        ..AssembleView::default()
    };
    let req = request(
        Role::TeamAgent,
        &view,
        Some(&SpecialtyProfile::generalist()),
    );
    let parsed = RenderedWorld::parse(&req);
    assert!(parsed.recent_activity.as_ref().unwrap().degraded);
    let decision = BuiltinArc::new().chat(&req, &identity("team-agent:agent-1:generalist", 30));
    let calls = decision
        .message
        .tool_calls
        .expect("degradation forces completion");
    assert_eq!(calls[0].function.name, "complete_task");
}

#[test]
fn idle_agent_claims_the_lowest_eligible_open_task() {
    let mut board = Board::new();
    for id in [1, 2] {
        board
            .create_task(
                TaskId::new(id),
                format!("Task {id}"),
                "description",
                AgentId::orchestrator(),
                EventId::new(id),
                None,
            )
            .unwrap();
    }
    let view = AssembleView {
        goal: "Write a short onboarding guide.".into(),
        board_lines: board.digest_lines(Some(&AgentId::team(3))),
        ..AssembleView::default()
    };
    let req = request(
        Role::TeamAgent,
        &view,
        Some(&SpecialtyProfile::generalist()),
    );
    let decision = BuiltinArc::new().chat(&req, &identity("team-agent:agent-3:generalist", 0));
    let calls = decision.message.tool_calls.expect("an idle agent claims");
    assert_eq!(calls[0].function.name, "claim_task");
    let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
    assert_eq!(args["task"], 1, "lowest-id tie-break (F5c)");
}

#[test]
fn meta_reads_outcomes_per_tier_from_the_rendered_slot() {
    // Fold a real run_started so the REAL metrics renderer produces the
    // utilization lines the meta arc reads (state + specialty, F4b).
    let mut metrics = Metrics::new();
    metrics.fold(&openteam_core::Event::new(
        EventId::new(0),
        jiff::Timestamp::UNIX_EPOCH,
        openteam_core::EventSource::System,
        openteam_core::EventKind::RunStarted {
            run_id: uuid_nil(),
            seed: SEED,
            goal: "g".into(),
            agents: 2,
            meta_agents: 1,
            parallel: 2,
            scenario: None,
            caps: openteam_core::RunCaps::default(),
        },
    ));

    // Tier 1: nothing issued yet — the meta emits a judgment directive at
    // an Idle generalist it found in the utilization lines.
    let view = AssembleView {
        goal: "g".into(),
        metrics_digest: Some(metrics.digest()),
        ..AssembleView::default()
    };
    let req = request(Role::MetaAgent, &view, None);
    let decision = BuiltinArc::new().chat(&req, &identity("meta-agent:meta-1", 0));
    let calls = decision.message.tool_calls.expect("first tier fires");
    assert_eq!(calls.len(), 1, "meta emits at most one directive per turn");
    let name = calls[0].function.name.clone();
    assert!(
        name.starts_with("propose_") || name == "set_parallelism",
        "a directive emitter, got {name}"
    );

    // Both tiers used — rendered through the REAL outcomes-line renderer —
    // the meta yields.
    let outcome_lines = vec![
        Directive {
            id: openteam_core::DirectiveId::new(1),
            tier: DirectiveTier::Judgment,
            kind: DirectiveKind::ProposeRespecialize,
            args: serde_json::json!({"agent": "agent-2", "specialty": "doc-reviewer"}),
            from: AgentId::meta(1),
            state: DirectiveState::Fulfilled {
                by: AgentId::orchestrator(),
            },
        }
        .outcomes_line(),
        Directive {
            id: openteam_core::DirectiveId::new(2),
            tier: DirectiveTier::Mechanical,
            kind: DirectiveKind::SetParallelism,
            args: serde_json::json!({"target": 2}),
            from: AgentId::meta(1),
            state: DirectiveState::Fulfilled {
                by: AgentId::meta(1),
            },
        }
        .outcomes_line(),
    ];
    let view = AssembleView {
        goal: "g".into(),
        metrics_digest: Some(metrics.digest()),
        outcome_lines,
        ..AssembleView::default()
    };
    let req = request(Role::MetaAgent, &view, None);
    let parsed = RenderedWorld::parse(&req);
    let outcomes = parsed.directive_outcomes.expect("outcomes parse");
    let decision = BuiltinArc::new().chat(&req, &identity("meta-agent:meta-1", 4));
    assert!(
        decision.message.tool_calls.is_none(),
        "both tiers used ({outcomes:?}) — the meta yields"
    );
}

fn uuid_nil() -> openteam_core::RunId {
    openteam_core::RunId::nil()
}
