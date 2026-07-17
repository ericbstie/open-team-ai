//! Context assembly: one data-driven policy per role, every role rebuilds
//! (ADR 0016).
//!
//! The seam is data, not a trait: one deep assembler interprets a
//! `ContextPolicy` value — an ordered list of [`SectionSpec`]s plus a total
//! assembly pool — and renders exactly two messages: the static role
//! skeleton (`system`) and the `##`-sectioned `user` message in the pinned
//! line grammars (the prompt-legibility contract, ADR 0016/0021).
//! Presentation order is fixed per policy and deliberately separate from
//! allocation priority (which section gets budget first and degrades last).

use openteam_wire::{Role, SpecialtySlug, TokenCounter};

use crate::event::DegradedSection;

/// A labeled, individually-budgeted block of assembled context
/// (CONTEXT.md: Context section).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SectionKind {
    Goal,
    BoardDigest,
    ClaimedTask,
    RecentActivity,
    KnowledgeRetrievals,
    FreshMessages,
    Directives,
    DirectiveOutcomes,
    MetricsDigest,
    RecentEvents,
}

impl SectionKind {
    /// The `##` header text (pins §2).
    pub fn header(self) -> &'static str {
        match self {
            Self::Goal => "Goal",
            Self::BoardDigest => "Board digest",
            Self::ClaimedTask => "Claimed task",
            Self::RecentActivity => "Recent activity",
            Self::KnowledgeRetrievals => "Knowledge retrievals",
            Self::FreshMessages => "Fresh messages",
            Self::Directives => "Directives",
            Self::DirectiveOutcomes => "Directive outcomes",
            Self::MetricsDigest => "Metrics digest",
            Self::RecentEvents => "Recent events",
        }
    }

    /// The snake_case label used in `context_degraded` (pins §7).
    pub fn label(self) -> &'static str {
        match self {
            Self::Goal => "goal",
            Self::BoardDigest => "board_digest",
            Self::ClaimedTask => "claimed_task",
            Self::RecentActivity => "recent_activity",
            Self::KnowledgeRetrievals => "knowledge_retrievals",
            Self::FreshMessages => "fresh_messages",
            Self::Directives => "directives",
            Self::DirectiveOutcomes => "directive_outcomes",
            Self::MetricsDigest => "metrics_digest",
            Self::RecentEvents => "recent_events",
        }
    }

    /// The empty-section placeholder (pins §2).
    fn placeholder(self) -> &'static str {
        match self {
            Self::BoardDigest => "(empty)",
            Self::DirectiveOutcomes => "(none issued)",
            _ => "(none)",
        }
    }
}

/// How a section sheds content under budget pressure (ADR 0016).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropRule {
    /// Goal, Directives, and the meta's Directive outcomes are never
    /// dropped or truncated (ADR 0016; outcomes pinned by #27).
    Never,
    /// The board digest's terminal tail shrinks first.
    TerminalTailFirst,
    /// Retrievals drop lowest-cosine hits.
    LowestCosine,
    /// Oldest-first delivery under budget with carryover — always delivers
    /// at least its single oldest item (fresh messages).
    OldestFirstCarryover,
    /// A sliding window: oldest-dropped under budget, always ≥1 item
    /// delivered (recent activity).
    OldestDropped,
    /// Plain tail truncation, always ≥1 line (metrics digest).
    TailTruncate,
}

/// A context section's token cap plus its allocation/degradation priority
/// (CONTEXT.md: Section budget). Lower `priority` allocates first and
/// degrades last.
#[derive(Debug, Clone, Copy)]
pub struct SectionSpec {
    pub kind: SectionKind,
    pub budget: usize,
    pub priority: u8,
    pub drop_rule: DropRule,
}

/// The per-role assembly policy — a value, not a trait (ADR 0016): the
/// ordered section list (presentation order) plus an optional total
/// assembly pool (the `OPENTEAM_ASSEMBLY_BUDGET` test knob, pins §6).
#[derive(Debug, Clone)]
pub struct ContextPolicy {
    pub sections: Vec<SectionSpec>,
    pub pool: Option<usize>,
}

impl ContextPolicy {
    /// Orchestrator: Goal, Board digest, Knowledge retrievals, Fresh
    /// messages, Directives (pins §2); Goal and Directives never dropped.
    pub fn orchestrator() -> Self {
        Self {
            sections: vec![
                spec(SectionKind::Goal, 200, 0, DropRule::Never),
                spec(
                    SectionKind::BoardDigest,
                    800,
                    2,
                    DropRule::TerminalTailFirst,
                ),
                spec(
                    SectionKind::KnowledgeRetrievals,
                    600,
                    4,
                    DropRule::LowestCosine,
                ),
                spec(
                    SectionKind::FreshMessages,
                    800,
                    3,
                    DropRule::OldestFirstCarryover,
                ),
                spec(SectionKind::Directives, 400, 1, DropRule::Never),
            ],
            pool: None,
        }
    }

    /// Team agent: Goal, Board digest, Claimed task, Recent activity,
    /// Knowledge retrievals, Fresh messages (pins §2).
    pub fn team_agent() -> Self {
        Self {
            sections: vec![
                spec(SectionKind::Goal, 200, 0, DropRule::Never),
                spec(
                    SectionKind::BoardDigest,
                    800,
                    3,
                    DropRule::TerminalTailFirst,
                ),
                spec(SectionKind::ClaimedTask, 100, 1, DropRule::Never),
                spec(SectionKind::RecentActivity, 400, 2, DropRule::OldestDropped),
                spec(
                    SectionKind::KnowledgeRetrievals,
                    600,
                    5,
                    DropRule::LowestCosine,
                ),
                spec(
                    SectionKind::FreshMessages,
                    800,
                    4,
                    DropRule::OldestFirstCarryover,
                ),
            ],
            pool: None,
        }
    }

    /// Meta-agent: the four slots — Goal, Metrics digest, Directive
    /// outcomes, Recent events (ADR 0016 amendment). Directive outcomes is
    /// never dropped (#27): it is the meta's stateless already-issued
    /// ≤1-per-tier bound (ADR 0020/0021) — load-bearing state, exactly like
    /// the orchestrator's Directives section.
    pub fn meta_agent() -> Self {
        Self {
            sections: vec![
                spec(SectionKind::Goal, 200, 0, DropRule::Never),
                spec(SectionKind::MetricsDigest, 800, 2, DropRule::TailTruncate),
                spec(SectionKind::DirectiveOutcomes, 400, 1, DropRule::Never),
                spec(SectionKind::RecentEvents, 400, 3, DropRule::TailTruncate),
            ],
            pool: None,
        }
    }

    /// The policy for a role, with the optional global pool override.
    pub fn for_role(role: Role, pool: Option<usize>) -> Self {
        let mut policy = match role {
            Role::Orchestrator => Self::orchestrator(),
            Role::TeamAgent => Self::team_agent(),
            Role::MetaAgent => Self::meta_agent(),
        };
        policy.pool = pool;
        policy
    }
}

fn spec(kind: SectionKind, budget: usize, priority: u8, drop_rule: DropRule) -> SectionSpec {
    SectionSpec {
        kind,
        budget,
        priority,
        drop_rule,
    }
}

/// A team agent's authored specialty (CONTEXT.md: Specialty): slug, roster
/// description, freeform focus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialtyProfile {
    pub slug: SpecialtySlug,
    pub description: String,
    pub focus: String,
}

impl SpecialtyProfile {
    /// The harness-shipped boot specialty (CONTEXT.md: Generalist).
    pub fn generalist() -> Self {
        Self {
            slug: SpecialtySlug::generalist(),
            description: "A flexible generalist ready for any task.".into(),
            focus: "whatever the board needs".into(),
        }
    }
}

/// The world snapshot the runtime builds under the write-path lock for one
/// agent's turn — everything the assembler may render.
#[derive(Debug, Clone, Default)]
pub struct AssembleView {
    pub goal: String,
    /// Board digest task lines (full board, or the claimed-plus-eligible
    /// slice for a team agent).
    pub board_lines: Vec<String>,
    /// The folded run-health line (orchestrator only, ADR 0016/0020).
    pub run_health: Option<String>,
    /// The claimed-task line; present ⟺ Working.
    pub claimed_line: Option<String>,
    /// The agent's recent-activity window lines, oldest first.
    pub window_lines: Vec<String>,
    /// Rendered fresh-message lines for the queued mailbox, oldest first.
    pub queued_messages: Vec<String>,
    /// Retrieval hits: (cosine score, rendered line), highest first.
    pub retrievals: Vec<(f32, String)>,
    /// Pending judgment directive lines (orchestrator, never dropped).
    pub directive_lines: Vec<String>,
    /// Directive-outcome lines (meta).
    pub outcome_lines: Vec<String>,
    /// The rendered metrics digest (meta).
    pub metrics_digest: Option<String>,
    /// Recent event lines (meta), oldest first.
    pub recent_event_lines: Vec<String>,
}

/// What `assemble` returns: the two wire messages, how many queued
/// messages were drained into this prompt (the runtime pops exactly those
/// and emits `messages_delivered`), and any degradation records.
#[derive(Debug, Clone)]
pub struct AssembledPrompt {
    pub system: String,
    pub user: String,
    pub drained: usize,
    pub degraded: Vec<DegradedSection>,
}

/// The static role skeleton (ADR 0012, pins §8) — inert to the mock.
pub fn skeleton(role: Role, specialty: Option<&SpecialtyProfile>) -> String {
    match role {
        Role::Orchestrator => "You are the orchestrator of an offline agentic team. You decompose \
                              the goal into board tasks, form teams, steer via messages and \
                              directives, and alone end the run with finish_run."
            .into(),
        Role::TeamAgent => {
            let profile = specialty
                .cloned()
                .unwrap_or_else(SpecialtyProfile::generalist);
            format!(
                "You are a team agent. Specialty: {} — {} Focus: {} Claim eligible work, do it \
                 over one or more turns, then complete_task.",
                profile.slug, profile.description, profile.focus
            )
        }
        Role::MetaAgent => "You are a meta-agent. You observe metrics and improve the process \
                            through directives only."
            .into(),
    }
}

struct RenderedSection {
    kind: SectionKind,
    body: String,
    used: usize,
    dropped: usize,
    drained: Option<usize>,
}

/// The one real assembler (ADR 0016): interpret the policy against the
/// view, budgeting in priority order and degrading per drop rule; render
/// the sections in presentation order.
pub fn assemble(
    policy: &ContextPolicy,
    role: Role,
    specialty: Option<&SpecialtyProfile>,
    view: &AssembleView,
    counter: &dyn TokenCounter,
) -> AssembledPrompt {
    // Allocation pass in priority order (stable by list position).
    let mut order: Vec<usize> = (0..policy.sections.len()).collect();
    order.sort_by_key(|&i| (policy.sections[i].priority, i));

    let mut remaining = policy.pool;
    let mut rendered: Vec<Option<RenderedSection>> = policy.sections.iter().map(|_| None).collect();
    for index in order {
        let section_spec = &policy.sections[index];
        let allowed = match remaining {
            Some(pool) => section_spec.budget.min(pool),
            None => section_spec.budget,
        };
        let section = render_section(section_spec, allowed, view, counter);
        if let Some(pool) = remaining.as_mut() {
            *pool = pool.saturating_sub(section.used);
        }
        rendered[index] = Some(section);
    }

    // Presentation pass in policy order.
    let mut blocks = Vec::with_capacity(policy.sections.len());
    let mut degraded = Vec::new();
    let mut drained = 0;
    for section in rendered.into_iter().flatten() {
        blocks.push(format!("## {}\n{}", section.kind.header(), section.body));
        if let Some(count) = section.drained {
            drained = count;
        }
        if section.dropped > 0 {
            degraded.push(DegradedSection {
                kind: section.kind.label().into(),
                budget: policy.sections.iter().fold(0, |acc, s| {
                    if s.kind == section.kind {
                        s.budget as u32
                    } else {
                        acc
                    }
                }),
                used: section.used as u32,
                dropped_items: section.dropped as u32,
            });
        }
    }

    AssembledPrompt {
        system: skeleton(role, specialty),
        user: blocks.join("\n\n"),
        drained,
        degraded,
    }
}

fn count_lines(lines: &[String], counter: &dyn TokenCounter) -> usize {
    lines.iter().map(|line| counter.count(line)).sum()
}

fn render_section(
    section_spec: &SectionSpec,
    allowed: usize,
    view: &AssembleView,
    counter: &dyn TokenCounter,
) -> RenderedSection {
    let kind = section_spec.kind;
    let mut dropped = 0usize;
    let mut drained = None;

    let lines: Vec<String> = match kind {
        SectionKind::Goal => vec![view.goal.clone()],
        SectionKind::BoardDigest => {
            let mut lines = view.board_lines.clone();
            // The terminal tail shrinks first (ADR 0016): drop terminal
            // task lines from the end, then non-terminal from the end; the
            // run-health line is always kept.
            let health_cost = view
                .run_health
                .as_ref()
                .map(|h| counter.count(h))
                .unwrap_or(0);
            let over = |lines: &[String]| count_lines(lines, counter) + health_cost > allowed;
            while over(&lines) && !lines.is_empty() {
                let victim = lines
                    .iter()
                    .rposition(|l| l.contains("[Done]") || l.contains("[Cancelled]"))
                    .unwrap_or(lines.len() - 1);
                lines.remove(victim);
                dropped += 1;
            }
            if lines.is_empty() {
                lines.push(kind.placeholder().into());
            }
            if let Some(health) = &view.run_health {
                lines.push(health.clone());
            }
            lines
        }
        SectionKind::ClaimedTask => {
            vec![
                view.claimed_line
                    .clone()
                    .unwrap_or_else(|| kind.placeholder().into()),
            ]
        }
        SectionKind::RecentActivity => {
            let mut lines = view.window_lines.clone();
            while lines.len() > 1 && count_lines(&lines, counter) > allowed {
                lines.remove(0); // oldest-dropped
                dropped += 1;
            }
            with_marker_or_placeholder(lines, dropped, kind)
        }
        SectionKind::KnowledgeRetrievals => {
            let mut hits = view.retrievals.clone();
            let line_of =
                |hits: &[(f32, String)]| hits.iter().map(|(_, l)| counter.count(l)).sum::<usize>();
            while !hits.is_empty() && line_of(&hits) > allowed {
                hits.pop(); // lowest cosine last
                dropped += 1;
            }
            let lines: Vec<String> = hits.into_iter().map(|(_, l)| l).collect();
            if lines.is_empty() {
                vec![kind.placeholder().into()]
            } else {
                lines
            }
        }
        SectionKind::FreshMessages => {
            // Oldest-first under budget with carryover; always deliver at
            // least the single oldest item (the head-of-line mitigation,
            // ADR 0011/0016).
            let mut taken = Vec::new();
            let mut used = 0usize;
            for line in &view.queued_messages {
                let cost = counter.count(line);
                if taken.is_empty() || used + cost <= allowed {
                    used += cost;
                    taken.push(line.clone());
                } else {
                    break;
                }
            }
            let withheld = view.queued_messages.len() - taken.len();
            dropped = withheld;
            drained = Some(taken.len());
            with_marker_or_placeholder(taken, withheld, kind)
        }
        SectionKind::Directives => {
            if view.directive_lines.is_empty() {
                vec![kind.placeholder().into()]
            } else {
                view.directive_lines.clone()
            }
        }
        SectionKind::DirectiveOutcomes => {
            // Never dropped (#27): the meta re-derives its ≤1-per-tier
            // bound from this slot each completion (ADR 0020/0021), so a
            // truncated tail would make it re-issue directives every
            // cadence turn. Small by construction — one line per directive.
            if view.outcome_lines.is_empty() {
                vec![kind.placeholder().into()]
            } else {
                view.outcome_lines.clone()
            }
        }
        SectionKind::MetricsDigest => {
            let digest = view.metrics_digest.clone().unwrap_or_default();
            let mut lines: Vec<String> = digest.lines().map(str::to_owned).collect();
            while lines.len() > 1 && count_lines(&lines, counter) > allowed {
                lines.pop();
                dropped += 1;
            }
            if lines.is_empty() {
                vec![kind.placeholder().into()]
            } else {
                lines
            }
        }
        SectionKind::RecentEvents => {
            // A bounded window by design: fit under budget dropping oldest;
            // not counted as degradation (the runtime already bounds it).
            let mut lines = view.recent_event_lines.clone();
            while lines.len() > 1 && count_lines(&lines, counter) > allowed {
                lines.remove(0);
            }
            if lines.is_empty() {
                vec![kind.placeholder().into()]
            } else {
                lines
            }
        }
    };

    let used = count_lines(&lines, counter);
    RenderedSection {
        kind,
        body: lines.join("\n"),
        used,
        dropped,
        drained,
    }
}

/// Prepend the pinned `(degraded: <n> dropped)` marker when an oldest-first
/// section dropped or withheld items (pins §3); placeholder when empty.
fn with_marker_or_placeholder(
    mut lines: Vec<String>,
    dropped: usize,
    kind: SectionKind,
) -> Vec<String> {
    if dropped > 0 {
        lines.insert(0, format!("(degraded: {dropped} dropped)"));
        lines
    } else if lines.is_empty() {
        vec![kind.placeholder().into()]
    } else {
        lines
    }
}

/// The claimed-task line grammar (pins §3):
/// `task <id> — "<title>" (team <t>)`, `(team -)` when untagged.
pub fn claimed_task_line(task: &crate::board::Task) -> String {
    let team = task
        .team
        .as_ref()
        .map(|t| t.to_string())
        .unwrap_or_else(|| "-".into());
    format!("task {} — \"{}\" (team {})", task.id, task.title, team)
}

/// The knowledge-retrievals line grammar (pins §3):
/// `- entry <id> (<kind> by <author>, cos <score>): "<text>"`.
pub fn retrieval_line(hit: &crate::knowledge::ScoredEntry) -> String {
    format!(
        "- entry {} ({} by {}, cos {:.2}): \"{}\"",
        hit.entry.id, hit.entry.kind, hit.entry.author, hit.score, hit.entry.text
    )
}

/// The recent-events line grammar (pins §3): `- event <id> <kind> (<source>)`.
pub fn recent_event_line(event: &crate::event::Event) -> String {
    let kind = serde_json::to_value(event)
        .ok()
        .and_then(|v| v.get("kind").and_then(|k| k.as_str().map(str::to_owned)))
        .unwrap_or_else(|| "unknown".into());
    format!("- event {} {} ({})", event.id, kind, event.source)
}

/// One recent-activity window line (pins §3):
/// `- [turn <n>] <verb>{<gist>} -> <outcome>`.
pub fn window_line(turn: u64, verb: &str, gist: &str, outcome_word: &str) -> String {
    format!("- [turn {turn}] {verb}{{{gist}}} -> {outcome_word}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use openteam_wire::CharCountTokenizer;

    fn agentless_view() -> AssembleView {
        AssembleView {
            goal: "Write a short onboarding guide for new contributors.".into(),
            ..AssembleView::default()
        }
    }

    #[test]
    fn orchestrator_empty_world_matches_transcript_pair_a() {
        let mut view = agentless_view();
        view.run_health = Some(
            "run-health: done 0/0 · agents 0W/3I/0S · mailbox depth 0 (max 0) · ticks-since-done 0"
                .into(),
        );
        let prompt = assemble(
            &ContextPolicy::orchestrator(),
            Role::Orchestrator,
            None,
            &view,
            &CharCountTokenizer,
        );
        let expected = "## Goal\nWrite a short onboarding guide for new contributors.\n\n\
                        ## Board digest\n(empty)\nrun-health: done 0/0 · agents 0W/3I/0S · mailbox depth 0 (max 0) · ticks-since-done 0\n\n\
                        ## Knowledge retrievals\n(none)\n\n\
                        ## Fresh messages\n(none)\n\n\
                        ## Directives\n(none)";
        assert_eq!(prompt.user, expected);
        assert!(prompt.degraded.is_empty());
        assert_eq!(prompt.drained, 0);
        assert!(prompt.system.starts_with("You are the orchestrator"));
    }

    #[test]
    fn team_agent_open_board_matches_transcript_pair_b() {
        let mut view = agentless_view();
        view.goal = "Write a short onboarding guide for new contributors.".into();
        view.board_lines = vec![
            "- task 1 [Open] team:t1  \"Draft the setup section\"".into(),
            "- task 2 [Open] team:t1  \"Draft the architecture overview\"".into(),
        ];
        let prompt = assemble(
            &ContextPolicy::team_agent(),
            Role::TeamAgent,
            Some(&SpecialtyProfile::generalist()),
            &view,
            &CharCountTokenizer,
        );
        let expected = "## Goal\nWrite a short onboarding guide for new contributors.\n\n\
                        ## Board digest\n- task 1 [Open] team:t1  \"Draft the setup section\"\n- task 2 [Open] team:t1  \"Draft the architecture overview\"\n\n\
                        ## Claimed task\n(none)\n\n\
                        ## Recent activity\n(none)\n\n\
                        ## Knowledge retrievals\n(none)\n\n\
                        ## Fresh messages\n(none)";
        assert_eq!(prompt.user, expected);
        assert!(prompt.system.contains("Specialty: generalist"));
    }

    #[test]
    fn meta_agent_renders_its_four_slots_in_order() {
        let mut view = agentless_view();
        view.metrics_digest = Some("throughput: 0 task_completed / 3 EventIds".into());
        let prompt = assemble(
            &ContextPolicy::meta_agent(),
            Role::MetaAgent,
            None,
            &view,
            &CharCountTokenizer,
        );
        let headers: Vec<&str> = prompt
            .user
            .lines()
            .filter(|l| l.starts_with("## "))
            .collect();
        assert_eq!(
            headers,
            vec![
                "## Goal",
                "## Metrics digest",
                "## Directive outcomes",
                "## Recent events"
            ]
        );
        assert!(prompt.user.contains("## Directive outcomes\n(none issued)"));
    }

    #[test]
    fn goal_and_directives_never_drop_under_a_tiny_pool() {
        let mut view = agentless_view();
        view.directive_lines = vec![
            "- directive 1 [judgment, pending] propose_respecialize{agent:agent-3, specialty:doc-reviewer} from meta-1".into(),
        ];
        view.board_lines = vec!["- task 1 [Open] team:t1  \"A task\"".into()];
        let mut policy = ContextPolicy::orchestrator();
        policy.pool = Some(5); // absurdly tiny
        let prompt = assemble(
            &policy,
            Role::Orchestrator,
            None,
            &view,
            &CharCountTokenizer,
        );
        assert!(prompt.user.contains("onboarding guide"), "goal survives");
        assert!(
            prompt.user.contains("propose_respecialize"),
            "directives survive"
        );
    }

    #[test]
    fn meta_directive_outcomes_never_drop_under_a_tiny_pool() {
        // #27: the outcomes slot is the meta's stateless ≤1-per-tier bound
        // (ADR 0020/0021) — truncating it under budget pressure made the
        // meta re-derive "this tier is unused" and re-issue directives.
        let mut view = agentless_view();
        view.metrics_digest = Some(
            "throughput: 0 task_completed / 40 EventIds · latency: work n/a\n\
             utilization:\n  - agent-1: Idle, generalist (idle 12)"
                .into(),
        );
        view.outcome_lines = vec![
            "- directive 1 [mechanical] set_parallelism{target:2} — fulfilled by runtime".into(),
            "- directive 2 [judgment] propose_respecialize{agent:agent-3, specialty:doc-reviewer} — pending".into(),
        ];
        view.recent_event_lines = vec!["- event 7 directive_issued (meta-1)".into()];
        let mut policy = ContextPolicy::meta_agent();
        policy.pool = Some(5); // absurdly tiny (the OPENTEAM_ASSEMBLY_BUDGET knob)
        let prompt = assemble(&policy, Role::MetaAgent, None, &view, &CharCountTokenizer);
        assert!(
            prompt.user.contains("set_parallelism"),
            "mechanical outcome survives"
        );
        assert!(
            prompt.user.contains("propose_respecialize"),
            "judgment outcome survives"
        );
        assert!(
            !prompt
                .degraded
                .iter()
                .any(|d| d.kind == "directive_outcomes"),
            "the outcomes slot never records degradation"
        );
    }

    #[test]
    fn board_digest_sheds_terminal_tail_first() {
        let mut view = agentless_view();
        view.board_lines = vec![
            "- task 1 [Open] team:t1  \"Alpha\"".into(),
            "- task 2 [Done] team:t1  \"Beta\"".into(),
            "- task 3 [Done] team:t1  \"Gamma\"".into(),
        ];
        let mut policy = ContextPolicy::team_agent();
        for section_spec in &mut policy.sections {
            if section_spec.kind == SectionKind::BoardDigest {
                // Room for roughly one line.
                section_spec.budget = 12;
            }
        }
        let prompt = assemble(
            &policy,
            Role::TeamAgent,
            Some(&SpecialtyProfile::generalist()),
            &view,
            &CharCountTokenizer,
        );
        assert!(prompt.user.contains("Alpha"), "open task survives");
        assert!(!prompt.user.contains("Gamma"), "terminal tail shed");
        let record = prompt
            .degraded
            .iter()
            .find(|d| d.kind == "board_digest")
            .expect("degradation recorded");
        assert!(record.dropped_items >= 1);
    }

    #[test]
    fn retrievals_drop_lowest_cosine_first() {
        let mut view = agentless_view();
        view.retrievals = vec![
            (
                0.9,
                "- entry 1 (Note by agent-1, cos 0.90): \"top hit\"".into(),
            ),
            (
                0.2,
                "- entry 2 (Note by agent-2, cos 0.20): \"low hit\"".into(),
            ),
        ];
        let mut policy = ContextPolicy::orchestrator();
        for section_spec in &mut policy.sections {
            if section_spec.kind == SectionKind::KnowledgeRetrievals {
                section_spec.budget = 13;
            }
        }
        let prompt = assemble(
            &policy,
            Role::Orchestrator,
            None,
            &view,
            &CharCountTokenizer,
        );
        assert!(prompt.user.contains("top hit"));
        assert!(!prompt.user.contains("low hit"));
    }

    #[test]
    fn fresh_messages_carry_over_with_marker_and_head_of_line_guarantee() {
        let mut view = agentless_view();
        view.queued_messages = vec![
            "- msg 1 from orchestrator (direct): \"a long oldest message that alone busts the budget\"".into(),
            "- msg 2 from orchestrator (direct): \"second\"".into(),
            "- msg 3 from orchestrator (direct): \"third\"".into(),
        ];
        let mut policy = ContextPolicy::team_agent();
        for section_spec in &mut policy.sections {
            if section_spec.kind == SectionKind::FreshMessages {
                section_spec.budget = 4;
            }
        }
        let prompt = assemble(
            &policy,
            Role::TeamAgent,
            Some(&SpecialtyProfile::generalist()),
            &view,
            &CharCountTokenizer,
        );
        assert_eq!(prompt.drained, 1, "at least the single oldest delivers");
        assert!(prompt.user.contains("(degraded: 2 dropped)"));
        assert!(prompt.user.contains("msg 1"));
        assert!(!prompt.user.contains("msg 3"));
        let record = prompt
            .degraded
            .iter()
            .find(|d| d.kind == "fresh_messages")
            .expect("degradation recorded");
        assert_eq!(record.dropped_items, 2);
    }

    #[test]
    fn recent_activity_drops_oldest_with_marker() {
        let mut view = agentless_view();
        view.claimed_line = Some("task 1 — \"Alpha\" (team t1)".into());
        view.window_lines = vec![
            "- [turn 1] claim_task{task:1} -> ok".into(),
            "- [turn 2] write_knowledge{\"note one\"} -> ok".into(),
            "- [turn 3] write_knowledge{\"note two\"} -> ok".into(),
        ];
        let mut policy = ContextPolicy::team_agent();
        for section_spec in &mut policy.sections {
            if section_spec.kind == SectionKind::RecentActivity {
                section_spec.budget = 12;
            }
        }
        let prompt = assemble(
            &policy,
            Role::TeamAgent,
            Some(&SpecialtyProfile::generalist()),
            &view,
            &CharCountTokenizer,
        );
        assert!(prompt.user.contains("(degraded:"));
        assert!(!prompt.user.contains("[turn 1]"), "oldest dropped");
        assert!(prompt.user.contains("[turn 3]"), "newest kept");
    }

    #[test]
    fn no_degradation_records_on_a_roomy_happy_path() {
        let mut view = agentless_view();
        view.board_lines = vec!["- task 1 [Open] team:t1  \"Alpha\"".into()];
        view.queued_messages = vec!["- msg 1 from orchestrator (direct): \"hello\"".into()];
        let prompt = assemble(
            &ContextPolicy::team_agent(),
            Role::TeamAgent,
            Some(&SpecialtyProfile::generalist()),
            &view,
            &CharCountTokenizer,
        );
        assert!(prompt.degraded.is_empty());
        assert_eq!(prompt.drained, 1);
        assert!(!prompt.user.contains("(degraded"));
    }
}
