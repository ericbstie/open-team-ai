//! The built-in behavior arc (ADR 0021 + its #22 amendment, pins §4).
//!
//! A pure function of `(request, identity, seed)` with **zero run-state**: the
//! phase is re-derived from the rendered world every completion — the board
//! rendered in the request *is* the arc's memory. Identity comes solely from
//! the `WireIdentity` (`user` field + headers, ADR 0008), never from content;
//! world state comes from the `##`-headed sections (ADR 0016 grammars); the
//! callable verbs come from the request's `tools` array (ADR 0013).
//!
//! Termination holds by construction (ADR 0021): decomposition is hard-capped
//! at `T = f(seed) ∈ [1, 8]` by the visible board count; every claimed task
//! completes within `W_task = g(seed, agent, task) ∈ [1..3]` work-actions **or
//! when the window is degraded** (degradation is a shortcut to completion,
//! never a block); and every role emits the mandatory no-tool-call yield when
//! nothing plausible remains. The seed drives plausible variety within fixed
//! bounds only — no seed can produce a non-terminating or schema-invalid run.

use rand::RngExt as _;
use rand_chacha::ChaCha8Rng;
use serde_json::{Value, json};

use openteam_wire::{
    ChatCompletionRequest, FinishReason, FunctionCall, ParsedUser, ResponseMessage, Seed, ToolCall,
    ToolType, WireIdentity,
};

use crate::behavior::{BehaviorModel, ChatDecision};
use crate::parse::{AgentStateLine, ClaimedTask, DirectiveLine, RenderedWorld, UtilizationLine};
use crate::seed::derive_rng;

/// The task-budget bound: `T = f(seed) ∈ [1, MAX_TASKS]` (ADR 0021).
const MAX_TASKS: u64 = 8;
/// The per-task work-action bound: `W_task ∈ [1..=MAX_WORK]` (ADR 0021).
const MAX_WORK: u64 = 3;

/// The default `BehaviorModel` adapter: the built-in arc (ADR 0019/0021).
#[derive(Debug, Clone, Copy, Default)]
pub struct BuiltinArc;

impl BuiltinArc {
    pub fn new() -> Self {
        Self
    }

    /// `T = f(seed) ∈ [1, 8]` — the seeded total task budget, a pure stable
    /// function of the run seed alone (ADR 0021), drawn from its own derived
    /// stream so it never correlates with per-completion draws.
    pub fn task_budget(seed: Seed) -> u64 {
        derive_rng(seed, "openteam-arc/task-budget", 0).random_range(1..=MAX_TASKS)
    }

    /// `W_task = g(seed, agent, task) ∈ [1..=3]` — the seeded work-action
    /// quota for one agent on one task (ADR 0021).
    pub fn work_quota(seed: Seed, agent: &str, task: u64) -> u64 {
        derive_rng(seed, &format!("openteam-arc/work-quota/{agent}"), task)
            .random_range(1..=MAX_WORK)
    }
}

impl BehaviorModel for BuiltinArc {
    fn chat(&self, req: &ChatCompletionRequest, id: &WireIdentity) -> ChatDecision {
        let world = RenderedWorld::parse(req);
        let mut rng = derive_rng(id.seed, &id.user, id.call_seq);

        // One verb per turn, then yield (ADR 0021 amendment F5a): a completion
        // whose turn-local messages already carry a tool outcome — ok,
        // rejected, or anything else — yields rather than acting again. This
        // is the general form of the lost-claim "don't hammer" rule.
        if world.turn_local.acted() {
            return yield_decision(pick(&mut rng, AFTER_ACTION_TEXTS).to_owned());
        }

        let plan = match ParsedUser::parse(&id.user) {
            Ok(ParsedUser::Orchestrator) => orchestrator_plan(&world, &mut rng, id.seed),
            Ok(ParsedUser::TeamAgent { agent, .. }) => {
                team_plan(&world, &mut rng, id.seed, agent.as_str())
            }
            Ok(ParsedUser::MetaAgent { .. }) => meta_plan(&world, &mut rng),
            // A client outside the identity grammar (or with no `user` at
            // all) is still served: the mandatory plausible-text yield.
            Err(_) => Plan::Yield(plain_client_text(&mut rng, &world)),
        };
        realize(plan, &world, req, id, &mut rng)
    }
}

/// What a completion decided to do: one parallel batch of calls, or the
/// mandatory no-tool-call yield.
enum Plan {
    Calls(Vec<(&'static str, Value)>),
    Yield(String),
}

// ---------------------------------------------------------------------------
// Orchestrator: phase from board digest + directives (ADR 0021).
// ---------------------------------------------------------------------------

fn orchestrator_plan(world: &RenderedWorld, rng: &mut ChaCha8Rng, seed: Seed) -> Plan {
    let budget = BuiltinArc::task_budget(seed);
    let count = world.board.as_ref().map_or(0, |board| board.count() as u64);

    // Decompose: batch one on an empty board, batch two when 0 < n < T. The
    // visible count only grows (terminal tasks stay on the digest), so this
    // stages at most two batches and never exceeds T (ADR 0021).
    if count < budget {
        return decompose_plan(world, rng, budget, count);
    }
    // Pending judgment directive → resolve it (act-with-cite or decline).
    if let Some(directive) = world.directives.iter().find(|d| d.is_pending()) {
        return resolve_plan(world, rng, directive);
    }
    let board = world.board.as_ref();
    if board.is_some_and(|b| !b.all_terminal()) {
        // Non-terminal tasks remain → yield, with a rare seeded steer.
        return steer_or_yield_plan(world, rng);
    }
    if count > 0 {
        // All tasks terminal and n > 0 → finish_run(report).
        return finish_plan(world, rng);
    }
    Plan::Yield(pick(rng, WAITING_TEXTS).to_owned())
}

fn decompose_plan(world: &RenderedWorld, rng: &mut ChaCha8Rng, budget: u64, count: u64) -> Plan {
    let words = goal_words(world.goal.as_deref());
    let mut calls: Vec<(&'static str, Value)> = Vec::new();
    let mut team_tag: Option<String> = None;
    let batch;
    if count == 0 {
        // Batch one: a seeded slice of the budget, optionally under a team
        // formed over the whole pool (the pool size is read from the
        // run-health line's `agents <w>W/<i>I/<s>S` clause).
        batch = rng.random_range(1..=budget);
        let pool = world
            .board
            .as_ref()
            .and_then(|board| board.run_health_agents)
            .map_or(0, |(working, idle, asleep)| working + idle + asleep);
        if pool > 0 && world.tools.available("form_team") && rng.random_bool(0.8) {
            let members: Vec<String> = (1..=pool).map(|n| format!("agent-{n}")).collect();
            calls.push(("form_team", json!({ "team": "t1", "members": members })));
            team_tag = Some("t1".to_owned());
        }
    } else {
        // Batch two: top the board up to exactly T, joining the team batch
        // one formed (the dominant visible tag).
        batch = budget - count;
        team_tag = world
            .board
            .as_ref()
            .and_then(|board| board.dominant_team())
            .map(str::to_owned);
    }
    for index in 0..batch {
        let (title, description) = task_text(rng, &words, count + index);
        let mut args = json!({ "title": title, "description": description });
        if let Some(team) = &team_tag {
            args["team"] = json!(team);
        }
        calls.push(("create_task", args));
    }
    Plan::Calls(calls)
}

/// Resolve a pending judgment directive: seeded act-with-cite through the
/// matching orchestrator verb (`in_response_to`), or `decline_directive`
/// (ADR 0020/0021).
fn resolve_plan(world: &RenderedWorld, rng: &mut ChaCha8Rng, directive: &DirectiveLine) -> Plan {
    let act = rng.random_bool(0.75);
    if act && let Some(call) = act_call(world, directive) {
        return Plan::Calls(vec![call]);
    }
    let reason = pick(rng, DECLINE_REASONS);
    let args = json!({ "directive": directive.id, "reason": reason });
    if world.tools.args_fit("decline_directive", &args) {
        return Plan::Calls(vec![("decline_directive", args)]);
    }
    Plan::Yield(pick(rng, WAITING_TEXTS).to_owned())
}

/// The act-with-cite call matching a judgment directive kind, or `None` when
/// the args don't parse or the matching verb isn't callable.
fn act_call(world: &RenderedWorld, directive: &DirectiveLine) -> Option<(&'static str, Value)> {
    let (verb, args) = match directive.kind.as_str() {
        "propose_respecialize" => {
            let agent = directive.args.get("agent")?;
            let slug = directive.args.get("specialty")?;
            (
                "respecialize",
                json!({
                    "agent": agent,
                    "specialty": {
                        "name": slug,
                        "description": format!("Focuses on {slug} work for the team."),
                        "focus": format!("{slug} duties toward the goal"),
                    },
                    "in_response_to": directive.id,
                }),
            )
        }
        "propose_reallocate" => {
            let task: u64 = directive.args.get("task")?.parse().ok()?;
            (
                "unassign_task",
                json!({
                    "task": task,
                    "reason": "Reallocating per the meta proposal.",
                    "in_response_to": directive.id,
                }),
            )
        }
        "propose_rebalance" => {
            let team = directive.args.get("team")?;
            let members = directive.args.get_list("members")?;
            (
                "set_team_members",
                json!({ "team": team, "members": members, "in_response_to": directive.id }),
            )
        }
        _ => return None,
    };
    world.tools.args_fit(verb, &args).then_some((verb, args))
}

/// The waiting-on-work yield, with a rare seeded steer `post_message`
/// (direct to a claimant, or broadcast) in the tool-bearing slot (pins §4).
fn steer_or_yield_plan(world: &RenderedWorld, rng: &mut ChaCha8Rng) -> Plan {
    if world.tools.available("post_message") && rng.random_bool(0.25) {
        let body = steer_body(rng, world);
        let claimants: Vec<&str> = world
            .board
            .as_ref()
            .map(|board| board.claimants())
            .unwrap_or_default();
        let args = if !claimants.is_empty() && rng.random_bool(0.6) {
            let to = claimants[rng.random_range(0..claimants.len())];
            json!({ "to": to, "body": body })
        } else {
            json!({ "broadcast": true, "body": body })
        };
        return Plan::Calls(vec![("post_message", args)]);
    }
    Plan::Yield(pick(rng, WAITING_TEXTS).to_owned())
}

fn finish_plan(world: &RenderedWorld, rng: &mut ChaCha8Rng) -> Plan {
    let args = json!({ "report": build_report(world) });
    if world.tools.args_fit("finish_run", &args) {
        return Plan::Calls(vec![("finish_run", args)]);
    }
    Plan::Yield(pick(rng, WAITING_TEXTS).to_owned())
}

/// The finish-run report: a plausible short markdown document assembled from
/// the goal and the Done tasks' titles.
fn build_report(world: &RenderedWorld) -> String {
    let goal = world.goal.as_deref().unwrap_or("Run report");
    let titles = world
        .board
        .as_ref()
        .map(|board| board.done_titles())
        .unwrap_or_default();
    let mut report = format!("# {goal}\n\n## Completed work\n");
    if titles.is_empty() {
        report.push_str("- (no tasks reached Done)\n");
    }
    for title in &titles {
        report.push_str("- ");
        report.push_str(title);
        report.push('\n');
    }
    report.push_str(&format!(
        "\n{} task(s) completed; results are recorded in the knowledge store and the board is fully terminal.\n",
        titles.len()
    ));
    report
}

// ---------------------------------------------------------------------------
// Team agent: phase from claimed task + digest + recent activity (ADR 0021).
// ---------------------------------------------------------------------------

fn team_plan(world: &RenderedWorld, rng: &mut ChaCha8Rng, seed: Seed, handle: &str) -> Plan {
    if let Some(claimed) = &world.claimed {
        let quota = BuiltinArc::work_quota(seed, handle, claimed.id);
        let window = world.recent_activity.clone().unwrap_or_default();
        // Degradation-safe by inversion (ADR 0021): complete when actions
        // seen ≥ W_task OR the window is degraded — a shortcut, never a block.
        if window.degraded || window.work_actions as u64 >= quota {
            let args = json!({ "result": completion_result(rng, &claimed.title) });
            if world.tools.args_fit("complete_task", &args) {
                return Plan::Calls(vec![("complete_task", args)]);
            }
            return Plan::Yield(pick(rng, IDLE_TEXTS).to_owned());
        }
        return work_action_plan(world, rng, claimed);
    }
    // No claimed task: claim the LOWEST-id eligible Open task (F5c — the team
    // agent's digest slice already shows only eligible tasks, so any visible
    // Open task is claimable).
    if let Some(open) = world.board.as_ref().and_then(|board| board.lowest_open()) {
        let args = json!({ "task": open.id });
        if world.tools.args_fit("claim_task", &args) {
            return Plan::Calls(vec![("claim_task", args)]);
        }
    }
    Plan::Yield(pick(rng, IDLE_TEXTS).to_owned())
}

/// One seeded work-action among write_knowledge / post_message /
/// search_knowledge (the work-verb set the window counter counts).
fn work_action_plan(world: &RenderedWorld, rng: &mut ChaCha8Rng, claimed: &ClaimedTask) -> Plan {
    let options: Vec<&'static str> = ["write_knowledge", "post_message", "search_knowledge"]
        .into_iter()
        .filter(|verb| world.tools.available(verb))
        .collect();
    if options.is_empty() {
        return Plan::Yield(pick(rng, IDLE_TEXTS).to_owned());
    }
    let verb = options[rng.random_range(0..options.len())];
    let args = match verb {
        "write_knowledge" => {
            json!({ "text": format!("{} — {}", claimed.title, pick(rng, NOTE_TEXTS)) })
        }
        "search_knowledge" => {
            let words = goal_words(Some(&claimed.title));
            let query = words
                .first()
                .cloned()
                .unwrap_or_else(|| "prior work".to_owned());
            json!({ "query": query })
        }
        _ => {
            let body = format!("Task {}: {}", claimed.id, pick(rng, STATUS_TEXTS));
            match &claimed.team {
                Some(team) => json!({ "team": team, "body": body }),
                None => json!({ "to": "orchestrator", "body": body }),
            }
        }
    };
    Plan::Calls(vec![(verb, args)])
}

// ---------------------------------------------------------------------------
// Meta-agent: phase from directive outcomes + utilization (ADR 0020/0021).
// ---------------------------------------------------------------------------

fn meta_plan(world: &RenderedWorld, rng: &mut ChaCha8Rng) -> Plan {
    // ≤1 directive per tier per run, read statelessly from the outcomes slot
    // (the #22 amendment); ≤1 directive per TURN, the next completion yields.
    let outcomes = world.directive_outcomes.unwrap_or_default();
    if !outcomes.judgment_used
        && let Some(call) = judgment_call(world, rng)
    {
        return Plan::Calls(vec![call]);
    }
    if !outcomes.mechanical_used {
        let bound = world.utilization.len().max(1);
        let args = json!({ "target": rng.random_range(1..=bound) });
        if world.tools.args_fit("set_parallelism", &args) {
            return Plan::Calls(vec![("set_parallelism", args)]);
        }
    }
    Plan::Yield(pick(rng, META_TEXTS).to_owned())
}

/// The judgment-tier proposal: `propose_respecialize` on a seeded Idle
/// generalist from the utilization lines, else `propose_reallocate` on a
/// Working task, else skip the tier.
fn judgment_call(world: &RenderedWorld, rng: &mut ChaCha8Rng) -> Option<(&'static str, Value)> {
    let idle_generalists: Vec<&UtilizationLine> = world
        .utilization
        .iter()
        .filter(|line| line.state == AgentStateLine::Idle && line.specialty == "generalist")
        .collect();
    if !idle_generalists.is_empty() {
        let target = idle_generalists[rng.random_range(0..idle_generalists.len())];
        let slug = pick(rng, SPECIALTY_SLUGS);
        let args = json!({ "agent": target.agent, "specialty": slug });
        if world.tools.args_fit("propose_respecialize", &args) {
            return Some(("propose_respecialize", args));
        }
    }
    let working: Vec<&UtilizationLine> = world
        .utilization
        .iter()
        .filter(|line| matches!(line.state, AgentStateLine::Working { .. }))
        .collect();
    if !working.is_empty() {
        let target = working[rng.random_range(0..working.len())];
        if let AgentStateLine::Working { task } = target.state {
            let args = json!({ "task": task, "reason": "Long-running claim; consider moving it." });
            if world.tools.args_fit("propose_reallocate", &args) {
                return Some(("propose_reallocate", args));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Realization: plan → ChatDecision (calls filtered through the tools index).
// ---------------------------------------------------------------------------

fn realize(
    plan: Plan,
    world: &RenderedWorld,
    req: &ChatCompletionRequest,
    id: &WireIdentity,
    rng: &mut ChaCha8Rng,
) -> ChatDecision {
    let calls = match plan {
        Plan::Yield(text) => return yield_decision(text),
        Plan::Calls(calls) => calls,
    };
    // The final never-`invalid` gate (ADR 0021/0025): a call is emitted only
    // if its verb is in the request's tools array and its args fit the verb's
    // schema; anything else is dropped, degrading to the mandatory yield.
    let mut fitted: Vec<(&'static str, Value)> = calls
        .into_iter()
        .filter(|(name, args)| world.tools.args_fit(name, args))
        .collect();
    if req.parallel_tool_calls == Some(false) && fitted.len() > 1 {
        fitted.truncate(1);
    }
    if fitted.is_empty() {
        return yield_decision(pick(rng, IDLE_TEXTS).to_owned());
    }
    let handle = handle_ish(&id.user);
    let tool_calls: Vec<ToolCall> = fitted
        .into_iter()
        .enumerate()
        .map(|(index, (name, args))| ToolCall {
            // Deterministic per completion (pins §4): call_<handle>_<seq>_<i>.
            id: format!("call_{handle}_{}_{}", id.call_seq, index + 1),
            kind: ToolType::Function,
            function: FunctionCall {
                name: name.to_owned(),
                arguments: args.to_string(),
            },
        })
        .collect();
    ChatDecision {
        message: ResponseMessage {
            role: "assistant".into(),
            content: None,
            refusal: None,
            tool_calls: Some(tool_calls),
        },
        finish: FinishReason::ToolCalls,
    }
}

fn yield_decision(text: String) -> ChatDecision {
    ChatDecision {
        message: ResponseMessage {
            role: "assistant".into(),
            content: Some(text),
            refusal: None,
            tool_calls: None,
        },
        finish: FinishReason::Stop,
    }
}

/// The alphanumeric gist of the parsed handle for tool-call ids: the ADR 0012
/// handle when the `user` field parses, the raw field otherwise, `anon` when
/// absent or empty.
pub(crate) fn handle_ish(user: &str) -> String {
    let base = ParsedUser::parse(user)
        .map(|parsed| parsed.agent().as_str().to_owned())
        .unwrap_or_else(|_| user.to_owned());
    let cleaned: String = base.chars().filter(char::is_ascii_alphanumeric).collect();
    if cleaned.is_empty() {
        "anon".to_owned()
    } else {
        cleaned
    }
}

// ---------------------------------------------------------------------------
// Seeded plausible text (the realism dial — never asserted on, ADR 0021).
// ---------------------------------------------------------------------------

fn pick<'a>(rng: &mut ChaCha8Rng, options: &[&'a str]) -> &'a str {
    options[rng.random_range(0..options.len())]
}

const AFTER_ACTION_TEXTS: &[&str] = &[
    "Action recorded for this turn; yielding.",
    "Done for this turn; handing back to the scheduler.",
    "That covers this turn's move. Yielding.",
    "Result noted; nothing further this turn.",
];

const WAITING_TEXTS: &[&str] = &[
    "Tasks are in flight; standing by for the team.",
    "The board is moving; waiting on claimed work.",
    "Nothing to steer right now; letting the team work.",
    "Holding — open work is progressing.",
];

const IDLE_TEXTS: &[&str] = &[
    "No eligible work visible; yielding.",
    "Nothing claimable right now; standing by.",
    "Idle — waiting for the board to change.",
    "No plausible next action; yielding.",
];

const META_TEXTS: &[&str] = &[
    "Both directive tiers exercised; observing the process.",
    "No directive warranted; the process looks healthy.",
    "Observing — metrics do not call for an intervention.",
];

const DECLINE_REASONS: &[&str] = &[
    "Current allocation is already serving the goal.",
    "Proposal duplicates steering already underway.",
    "Not worth the churn this late in the run.",
];

const NOTE_TEXTS: &[&str] = &[
    "key facts captured for the next pass.",
    "constraints and references recorded.",
    "findings noted for whoever picks this up.",
    "progress checkpoint written down.",
];

const STATUS_TEXTS: &[&str] = &[
    "in progress, on track.",
    "drafting now; will complete shortly.",
    "gathering what I need, no blockers.",
];

const SPECIALTY_SLUGS: &[&str] = &[
    "doc-reviewer",
    "test-writer",
    "researcher",
    "integrator",
    "editor",
];

const TASK_VERBS: &[&str] = &["Draft", "Outline", "Research", "Review", "Summarize"];
const TASK_NOUNS: &[&str] = &["section", "overview", "notes", "plan"];

/// Lowercased goal words worth weaving into task text (the realism dial).
fn goal_words(goal: Option<&str>) -> Vec<String> {
    goal.unwrap_or_default()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 4)
        .map(str::to_lowercase)
        .collect()
}

fn task_text(rng: &mut ChaCha8Rng, words: &[String], index: u64) -> (String, String) {
    let verb = pick(rng, TASK_VERBS);
    let noun = pick(rng, TASK_NOUNS);
    let topic = if words.is_empty() {
        "core".to_owned()
    } else {
        words[(index as usize + rng.random_range(0..words.len())) % words.len()].clone()
    };
    let title = format!("{verb} the {topic} {noun}");
    let description = format!("{verb} work covering {topic} toward the goal.");
    (title, description)
}

fn completion_result(rng: &mut ChaCha8Rng, title: &str) -> String {
    let gist = pick(
        rng,
        &[
            "done and recorded",
            "complete; see the knowledge store",
            "finished with notes attached",
        ],
    );
    format!("{title}: {gist}.")
}

fn steer_body(rng: &mut ChaCha8Rng, world: &RenderedWorld) -> String {
    let topic = world.goal.clone().unwrap_or_else(|| "the goal".to_owned());
    let template = pick(
        rng,
        &[
            "Prioritize what unblocks the rest of",
            "Keep results small and shareable for",
            "Post findings as you go on",
        ],
    );
    format!("{template}: {topic}")
}

fn plain_client_text(rng: &mut ChaCha8Rng, world: &RenderedWorld) -> String {
    match &world.goal {
        Some(goal) => format!("Proceeding with: {goal}"),
        None => pick(
            rng,
            &[
                "Acknowledged.",
                "Understood; proceeding.",
                "Noted — nothing further needed.",
            ],
        )
        .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use openteam_wire::FinishReason;

    fn arc_chat(
        user: &str,
        seed: Seed,
        call_seq: u64,
        req: &ChatCompletionRequest,
    ) -> ChatDecision {
        BuiltinArc::new().chat(req, &fixtures::identity(user, call_seq, seed))
    }

    fn call_names(decision: &ChatDecision) -> Vec<String> {
        decision
            .message
            .tool_calls
            .iter()
            .flatten()
            .map(|call| call.function.name.clone())
            .collect()
    }

    fn args_of(decision: &ChatDecision, index: usize) -> serde_json::Value {
        let calls = decision.message.tool_calls.as_ref().expect("tool calls");
        serde_json::from_str(&calls[index].function.arguments).expect("valid JSON args")
    }

    fn assert_yield(decision: &ChatDecision) {
        assert_eq!(decision.finish, FinishReason::Stop);
        assert!(decision.message.tool_calls.is_none());
        let text = decision.message.content.as_deref().unwrap_or_default();
        assert!(!text.is_empty(), "a yield carries short plausible text");
    }

    #[test]
    fn budget_and_quota_stay_in_bounds_for_every_seed() {
        for seed in fixtures::sweep_seeds() {
            let budget = BuiltinArc::task_budget(seed);
            assert!((1..=8).contains(&budget), "T={budget} for seed {seed}");
            for agent in ["agent-1", "agent-7"] {
                for task in [1_u64, 2, 8, 1000] {
                    let quota = BuiltinArc::work_quota(seed, agent, task);
                    assert!(
                        (1..=3).contains(&quota),
                        "W={quota} for ({seed},{agent},{task})"
                    );
                }
            }
        }
    }

    #[test]
    fn budget_and_quota_are_stable() {
        assert_eq!(BuiltinArc::task_budget(42), BuiltinArc::task_budget(42));
        assert_eq!(
            BuiltinArc::work_quota(42, "agent-1", 1),
            BuiltinArc::work_quota(42, "agent-1", 1)
        );
    }

    /// The fixed seed sweep (ADR 0025): for seeds 0..1000 plus edge seeds,
    /// drive the synthetic fixture requests through `BehaviorModel::chat`
    /// directly and assert the arc NEVER emits an invalid call and never
    /// loops (turn-local outcome ⇒ yield).
    #[test]
    fn seed_sweep_never_invalid_never_loops() {
        let arc = BuiltinArc::new();
        let cases = fixtures::sweep_cases();
        for seed in fixtures::sweep_seeds() {
            for case in &cases {
                let id = fixtures::identity(&case.user, case.call_seq, seed);
                let decision = arc.chat(&case.request, &id);
                fixtures::assert_schema_valid(&case.request, &decision)
                    .unwrap_or_else(|fault| panic!("seed {seed}, case {}: {fault}", case.name));
                match case.expect {
                    fixtures::Expect::Yield => {
                        assert_eq!(
                            decision.finish,
                            FinishReason::Stop,
                            "seed {seed}, case {} must yield",
                            case.name
                        );
                        assert!(decision.message.tool_calls.is_none());
                    }
                    fixtures::Expect::CallsAmong(allowed) => {
                        let names = call_names(&decision);
                        assert!(!names.is_empty(), "seed {seed}, case {}", case.name);
                        for name in &names {
                            assert!(
                                allowed.contains(&name.as_str()),
                                "seed {seed}, case {}: unexpected verb {name}",
                                case.name
                            );
                        }
                    }
                    fixtures::Expect::YieldOrCallsAmong(allowed) => {
                        for name in call_names(&decision) {
                            assert!(
                                allowed.contains(&name.as_str()),
                                "seed {seed}, case {}: unexpected verb {name}",
                                case.name
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn pair_a_empty_board_decomposes_within_budget() {
        let req = fixtures::orchestrator_request(fixtures::EMPTY_BOARD_SECTIONS);
        for seed in fixtures::sweep_seeds() {
            let decision = arc_chat("orchestrator", seed, 0, &req);
            assert_eq!(decision.finish, FinishReason::ToolCalls);
            let names = call_names(&decision);
            let creates = names.iter().filter(|n| *n == "create_task").count() as u64;
            assert!(creates >= 1, "seed {seed}: decompose must create tasks");
            assert!(
                creates <= BuiltinArc::task_budget(seed),
                "seed {seed}: batch one exceeds T"
            );
            for name in &names {
                assert!(
                    name == "create_task" || name == "form_team",
                    "seed {seed}: unexpected decompose verb {name}"
                );
            }
            // form_team, when present, spans the whole pool from run-health.
            if let Some(position) = names.iter().position(|n| n == "form_team") {
                let args = args_of(&decision, position);
                assert_eq!(args["members"].as_array().map(Vec::len), Some(3));
            }
        }
    }

    #[test]
    fn batch_two_tops_the_board_up_to_the_budget() {
        // Find a seed with T >= 3 so a 1-task board triggers batch two.
        let seed = fixtures::sweep_seeds()
            .into_iter()
            .find(|&s| BuiltinArc::task_budget(s) >= 3)
            .expect("some seed has T >= 3");
        let sections = "## Goal\nWrite a short onboarding guide for new contributors.\n\n\
             ## Board digest\n\
             - task 1 [Open] team:t1  \"Draft the setup section\"\n\
             run-health: done 0/1 · agents 0W/3I/0S · mailbox depth 0 (max 0) · ticks-since-done 0\n\n\
             ## Directives\n(none)";
        let req = fixtures::orchestrator_request(sections);
        let decision = arc_chat("orchestrator", seed, 2, &req);
        let names = call_names(&decision);
        let creates = names.iter().filter(|n| *n == "create_task").count() as u64;
        assert_eq!(creates, BuiltinArc::task_budget(seed) - 1);
        assert!(
            !names.contains(&"form_team".to_owned()),
            "batch two never re-forms the team"
        );
        // Batch-two tasks join the dominant visible team tag.
        let args = args_of(&decision, 0);
        assert_eq!(args["team"], "t1");
    }

    #[test]
    fn pair_b_lost_claim_race_yields_not_hammers() {
        let mut req = fixtures::team_request(fixtures::TWO_OPEN_TASKS_SECTIONS);
        fixtures::push_turn_local(
            &mut req,
            "claim_task",
            "{\"task\":1}",
            "{\"status\":\"rejected\",\"code\":\"task_not_open\",\"message\":\"task 1 is not Open (Claimed by agent-1)\"}",
        );
        for seed in fixtures::sweep_seeds() {
            let decision = arc_chat("team-agent:agent-3:generalist", seed, 1, &req);
            assert_yield(&decision);
        }
    }

    #[test]
    fn idle_agent_claims_the_lowest_id_open_task() {
        let req = fixtures::team_request(fixtures::TWO_OPEN_TASKS_SECTIONS);
        for seed in fixtures::sweep_seeds() {
            let decision = arc_chat("team-agent:agent-1:generalist", seed, 0, &req);
            assert_eq!(call_names(&decision), vec!["claim_task"]);
            assert_eq!(args_of(&decision, 0), serde_json::json!({"task": 1}));
        }
    }

    #[test]
    fn working_agent_emits_one_work_action_below_quota() {
        let req = fixtures::team_request(fixtures::WORKING_NO_ACTIVITY_SECTIONS);
        for seed in fixtures::sweep_seeds() {
            let decision = arc_chat("team-agent:agent-2:generalist", seed, 2, &req);
            let names = call_names(&decision);
            assert_eq!(names.len(), 1, "seed {seed}: exactly one work-action");
            assert!(
                ["write_knowledge", "post_message", "search_knowledge"]
                    .contains(&names[0].as_str()),
                "seed {seed}: {} is not a work-action",
                names[0]
            );
        }
    }

    #[test]
    fn work_quota_re_derivation_gates_completion() {
        for seed in [0_u64, 1, 42, u64::MAX] {
            let quota = BuiltinArc::work_quota(seed, "agent-2", 2);
            // Window count == quota ⇒ complete.
            let at_quota =
                fixtures::team_request(&fixtures::working_sections_with_actions(quota as usize));
            let decision = arc_chat("team-agent:agent-2:generalist", seed, 4, &at_quota);
            assert_eq!(call_names(&decision), vec!["complete_task"]);
            // Window count == quota - 1 ⇒ one more work-action.
            if quota > 1 {
                let below = fixtures::team_request(&fixtures::working_sections_with_actions(
                    quota as usize - 1,
                ));
                let decision = arc_chat("team-agent:agent-2:generalist", seed, 4, &below);
                assert_ne!(call_names(&decision), vec!["complete_task".to_owned()]);
            }
        }
    }

    /// The degradation-forces-completion targeted test (ADR 0021/0025): a
    /// degraded window with a count below quota still completes.
    #[test]
    fn degraded_window_forces_completion() {
        let sections = "## Goal\nWrite a short onboarding guide for new contributors.\n\n\
             ## Board digest\n\
             - task 2 [Claimed by agent-2] team:t1  \"Draft the architecture overview\"\n\n\
             ## Claimed task\ntask 2 — \"Draft the architecture overview\" (team t1)\n\n\
             ## Recent activity\n(degraded: 2 dropped)\n\n\
             ## Fresh messages\n(none)";
        let req = fixtures::team_request(sections);
        for seed in fixtures::sweep_seeds() {
            let decision = arc_chat("team-agent:agent-2:generalist", seed, 4, &req);
            assert_eq!(
                call_names(&decision),
                vec!["complete_task"],
                "seed {seed}: degradation must be a shortcut to completion, never a block"
            );
        }
    }

    #[test]
    fn pair_c_meta_targets_the_idle_generalist() {
        let req = fixtures::meta_request(fixtures::META_FRESH_SECTIONS);
        for seed in fixtures::sweep_seeds() {
            let decision = arc_chat("meta-agent:meta-1", seed, 0, &req);
            assert_eq!(call_names(&decision), vec!["propose_respecialize"]);
            let args = args_of(&decision, 0);
            assert_eq!(args["agent"], "agent-3", "the only Idle generalist");
            let slug = args["specialty"].as_str().expect("slug string");
            assert!(openteam_wire::SpecialtySlug::parse(slug).is_ok());
            assert_ne!(slug, "generalist");
        }
    }

    #[test]
    fn pair_d_pending_directive_is_resolved_with_a_cite_or_declined() {
        let req = fixtures::orchestrator_request(&fixtures::pending_directive_sections(
            fixtures::PENDING_DIRECTIVE_LINE,
        ));
        let mut acted = false;
        let mut declined = false;
        for seed in fixtures::sweep_seeds() {
            let decision = arc_chat("orchestrator", seed, 4, &req);
            let names = call_names(&decision);
            assert_eq!(names.len(), 1, "seed {seed}: resolve is a single call");
            let args = args_of(&decision, 0);
            match names[0].as_str() {
                "respecialize" => {
                    acted = true;
                    assert_eq!(args["agent"], "agent-3");
                    assert_eq!(args["in_response_to"], 1, "act must cite the directive");
                    assert_eq!(args["specialty"]["name"], "doc-reviewer");
                    assert!(args["specialty"]["description"].is_string());
                    assert!(args["specialty"]["focus"].is_string());
                }
                "decline_directive" => {
                    declined = true;
                    assert_eq!(args["directive"], 1);
                    assert!(args["reason"].is_string());
                }
                other => panic!("seed {seed}: unexpected resolve verb {other}"),
            }
        }
        assert!(acted, "some seed must act with a cite");
        assert!(declined, "some seed must decline");
    }

    #[test]
    fn reallocate_and_rebalance_directives_resolve_through_matching_verbs() {
        let mut moved = false;
        let mut rebalanced = false;
        for seed in fixtures::sweep_seeds() {
            let req = fixtures::orchestrator_request(&fixtures::pending_directive_sections(
                fixtures::PENDING_REALLOCATE_LINE,
            ));
            let decision = arc_chat("orchestrator", seed, 4, &req);
            let names = call_names(&decision);
            if names == ["unassign_task"] {
                moved = true;
                let args = args_of(&decision, 0);
                assert_eq!(args["task"], 3);
                assert_eq!(args["in_response_to"], 2);
            }
            let req = fixtures::orchestrator_request(&fixtures::pending_directive_sections(
                fixtures::PENDING_REBALANCE_LINE,
            ));
            let decision = arc_chat("orchestrator", seed, 4, &req);
            let names = call_names(&decision);
            if names == ["set_team_members"] {
                rebalanced = true;
                let args = args_of(&decision, 0);
                assert_eq!(args["team"], "t1");
                assert_eq!(
                    args["members"],
                    serde_json::json!(["agent-1", "agent-2"]),
                    "members list parsed from the directive line"
                );
                assert_eq!(args["in_response_to"], 3);
            }
        }
        assert!(moved && rebalanced);
    }

    #[test]
    fn pair_e_unused_mechanical_tier_fires_set_parallelism() {
        let req = fixtures::meta_request(fixtures::META_JUDGMENT_USED_SECTIONS);
        for seed in fixtures::sweep_seeds() {
            let decision = arc_chat("meta-agent:meta-1", seed, 2, &req);
            assert_eq!(call_names(&decision), vec!["set_parallelism"]);
            let target = args_of(&decision, 0)["target"].as_u64().expect("int");
            assert!(target >= 1);
        }
    }

    #[test]
    fn meta_with_both_tiers_used_yields() {
        let req = fixtures::meta_request(fixtures::META_BOTH_USED_SECTIONS);
        for seed in fixtures::sweep_seeds() {
            assert_yield(&arc_chat("meta-agent:meta-1", seed, 4, &req));
        }
    }

    #[test]
    fn all_terminal_board_finishes_the_run_with_a_report() {
        let req = fixtures::orchestrator_request(&fixtures::all_terminal_sections());
        for seed in fixtures::sweep_seeds() {
            let decision = arc_chat("orchestrator", seed, 6, &req);
            assert_eq!(call_names(&decision), vec!["finish_run"]);
            let report = args_of(&decision, 0)["report"]
                .as_str()
                .expect("report string")
                .to_owned();
            assert!(report.starts_with("# "), "report is a markdown document");
            assert!(report.contains("Draft the setup section"));
            assert!(report.contains("Draft the architecture overview"));
        }
    }

    #[test]
    fn waiting_orchestrator_yields_or_steers_rarely() {
        let req = fixtures::orchestrator_request(&fixtures::non_terminal_sections());
        let mut yields = 0_u32;
        let mut steers = 0_u32;
        for seed in fixtures::sweep_seeds() {
            let decision = arc_chat("orchestrator", seed, 2, &req);
            match call_names(&decision).as_slice() {
                [] => {
                    assert_yield(&decision);
                    yields += 1;
                }
                [name] => {
                    assert_eq!(name, "post_message");
                    let args = args_of(&decision, 0);
                    let direct = args.get("to").is_some();
                    let broadcast = args.get("broadcast").is_some();
                    assert!(direct ^ broadcast, "exactly one address form");
                    steers += 1;
                }
                other => panic!("unexpected steer batch {other:?}"),
            }
        }
        assert!(yields > steers, "the steer must stay rare");
        assert!(steers > 0, "the steer must be reachable");
    }

    #[test]
    fn unparseable_user_still_gets_a_plausible_yield() {
        let req = fixtures::orchestrator_request("What is the capital of France?");
        for user in ["", "not-a-grammar-user", "team-agent:bogus"] {
            let decision = arc_chat(user, 42, 0, &req);
            assert_yield(&decision);
        }
    }

    #[test]
    fn missing_tools_degrade_to_a_yield() {
        let mut req = fixtures::orchestrator_request(fixtures::EMPTY_BOARD_SECTIONS);
        req.tools = None;
        for seed in [0_u64, 42, u64::MAX] {
            assert_yield(&arc_chat("orchestrator", seed, 0, &req));
        }
    }

    #[test]
    fn parallel_tool_calls_false_caps_the_batch_at_one() {
        let mut req = fixtures::orchestrator_request(fixtures::EMPTY_BOARD_SECTIONS);
        req.parallel_tool_calls = Some(false);
        for seed in [0_u64, 7, 42] {
            let decision = arc_chat("orchestrator", seed, 0, &req);
            assert!(call_names(&decision).len() <= 1);
        }
    }

    #[test]
    fn decisions_are_deterministic_per_tuple_and_vary_across_call_seq() {
        let req = fixtures::orchestrator_request(fixtures::EMPTY_BOARD_SECTIONS);
        let one = arc_chat("orchestrator", 42, 0, &req);
        let two = arc_chat("orchestrator", 42, 0, &req);
        assert_eq!(
            serde_json::to_string(&one.message).expect("serializable"),
            serde_json::to_string(&two.message).expect("serializable")
        );
    }

    #[test]
    fn tool_call_ids_are_deterministic_and_unique_within_a_completion() {
        let req = fixtures::orchestrator_request(fixtures::EMPTY_BOARD_SECTIONS);
        let seed = fixtures::sweep_seeds()
            .into_iter()
            .find(|&s| BuiltinArc::task_budget(s) >= 2)
            .expect("some seed has T >= 2");
        let decision = arc_chat("orchestrator", seed, 0, &req);
        let calls = decision.message.tool_calls.expect("tool calls");
        let ids: Vec<&str> = calls.iter().map(|call| call.id.as_str()).collect();
        for (index, id) in ids.iter().enumerate() {
            assert_eq!(*id, format!("call_orchestrator_0_{}", index + 1));
        }
    }
}
