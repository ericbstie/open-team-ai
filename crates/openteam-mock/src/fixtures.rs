//! Test-only fixtures (ADR 0025): the transcript-§2 tool registries, §1-grammar
//! section texts, synthetic request builders, the fixed seed-sweep list, and
//! the little schema-check oracle asserting the arc never emits an `invalid`
//! call (name ∈ tools, arguments a JSON object, required present, keys ⊆
//! schema properties, shallow type fit).

use openteam_wire::{
    ChatCompletionRequest, ChatMessage, FinishReason, FunctionCall, MessageContent, ToolCall,
    ToolDef, ToolType, WireIdentity,
};
use serde_json::Value;

use crate::behavior::ChatDecision;

/// The three tool registries of dry-run-transcript §2 (pins §1 arg shapes).
pub const REGISTRIES: &str = include_str!("testdata/registries.json");

/// The fixed seed sweep (ADR 0025): 0..1000 plus edge seeds — 0, 1, u64::MAX,
/// and a couple of large primes.
pub fn sweep_seeds() -> Vec<u64> {
    let mut seeds: Vec<u64> = (0..1000).collect();
    seeds.extend([
        0,
        1,
        u64::MAX,
        2_305_843_009_213_693_951,  // 2^61 - 1 (Mersenne prime)
        18_446_744_073_709_551_557, // largest prime below 2^64
    ]);
    seeds
}

pub fn tools(role: &str) -> Vec<ToolDef> {
    let registries: Value = serde_json::from_str(REGISTRIES).expect("registries fixture parses");
    serde_json::from_value(registries[role].clone()).expect("registry role deserializes")
}

pub fn identity(user: &str, call_seq: u64, seed: u64) -> WireIdentity {
    WireIdentity {
        user: user.to_owned(),
        call_seq,
        seed,
    }
}

fn request(sections: &str, role: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: "openteam-mock".into(),
        messages: vec![
            ChatMessage::System {
                content: MessageContent::Text(
                    "(harness-owned skeleton — inert to the mock)".into(),
                ),
                name: None,
            },
            ChatMessage::User {
                content: MessageContent::Text(sections.into()),
                name: None,
            },
        ],
        tools: Some(tools(role)),
        tool_choice: None,
        parallel_tool_calls: Some(role != "meta"),
        user: None,
        safety_identifier: None,
        prompt_cache_key: None,
        stream: None,
        n: None,
    }
}

pub fn orchestrator_request(sections: &str) -> ChatCompletionRequest {
    request(sections, "orchestrator")
}

pub fn team_request(sections: &str) -> ChatCompletionRequest {
    request(sections, "team")
}

pub fn meta_request(sections: &str) -> ChatCompletionRequest {
    request(sections, "meta")
}

/// Append a turn-local assistant call + tool outcome pair (ADR 0015's inner
/// loop shape) to a request.
pub fn push_turn_local(req: &mut ChatCompletionRequest, verb: &str, args: &str, outcome: &str) {
    let call_id = format!("call_fixture_{}", req.messages.len());
    req.messages.push(ChatMessage::Assistant {
        content: None,
        tool_calls: Some(vec![ToolCall {
            id: call_id.clone(),
            kind: ToolType::Function,
            function: FunctionCall {
                name: verb.into(),
                arguments: args.into(),
            },
        }]),
        refusal: None,
        name: None,
    });
    req.messages.push(ChatMessage::Tool {
        content: MessageContent::Text(outcome.into()),
        tool_call_id: call_id,
    });
}

// ---------------------------------------------------------------------------
// The §1-grammar section fixtures (mirroring transcript Pairs A–E).
// ---------------------------------------------------------------------------

pub const EMPTY_BOARD_SECTIONS: &str = "## Goal\n\
Write a short onboarding guide for new contributors.\n\n\
## Board digest\n\
(empty)\n\
run-health: done 0/0 · agents 0W/3I/0S · mailbox depth 0 (max 0) · ticks-since-done 0\n\n\
## Knowledge retrievals\n\
(none)\n\n\
## Fresh messages\n\
(none)\n\n\
## Directives\n\
(none)";

pub const TWO_OPEN_TASKS_SECTIONS: &str = "## Goal\n\
Write a short onboarding guide for new contributors.\n\n\
## Board digest\n\
- task 1 [Open] team:t1  \"Draft the setup section\"\n\
- task 2 [Open] team:t1  \"Draft the architecture overview\"\n\n\
## Claimed task\n\
(none)\n\n\
## Recent activity\n\
(none)\n\n\
## Knowledge retrievals\n\
(none)\n\n\
## Fresh messages\n\
(none)";

pub const WORKING_NO_ACTIVITY_SECTIONS: &str = "## Goal\n\
Write a short onboarding guide for new contributors.\n\n\
## Board digest\n\
- task 2 [Claimed by agent-2] team:t1  \"Draft the architecture overview\"\n\n\
## Claimed task\n\
task 2 — \"Draft the architecture overview\" (team t1)\n\n\
## Recent activity\n\
- [turn 2] claim_task{task:2} -> ok\n\n\
## Knowledge retrievals\n\
(none)\n\n\
## Fresh messages\n\
(none)";

/// A Working team-agent view whose window shows `count` work-actions.
pub fn working_sections_with_actions(count: usize) -> String {
    let mut activity = String::from("- [turn 2] claim_task{task:2} -> ok\n");
    for turn in 0..count {
        activity.push_str(&format!(
            "- [turn {}] write_knowledge{{\"note {}\"}} -> ok\n",
            4 + 2 * turn,
            turn + 1
        ));
    }
    format!(
        "## Goal\n\
         Write a short onboarding guide for new contributors.\n\n\
         ## Board digest\n\
         - task 2 [Claimed by agent-2] team:t1  \"Draft the architecture overview\"\n\n\
         ## Claimed task\n\
         task 2 — \"Draft the architecture overview\" (team t1)\n\n\
         ## Recent activity\n\
         {activity}\n\
         ## Fresh messages\n\
         (none)"
    )
}

const EIGHT_TASK_BOARD: &str = "## Board digest\n\
- task 1 [Claimed by agent-1] team:t1  \"Draft the setup section\"\n\
- task 2 [Done] team:t1  \"Draft the architecture overview\"\n\
- task 3 [Claimed by agent-2] team:t1  \"Review the guide notes\"\n\
- task 4 [Done] team:t1  \"Outline the contributors plan\"\n\
- task 5 [Done] team:t1  \"Research onboarding details\"\n\
- task 6 [Done] team:t1  \"Summarize setup findings\"\n\
- task 7 [Done] team:t1  \"Draft the welcome section\"\n\
- task 8 [Done] team:t1  \"Review the build notes\"\n\
run-health: done 6/8 · agents 2W/1I/0S · mailbox depth 0 (max 1) · ticks-since-done 0";

/// Non-terminal 8-task board (n ≥ MAX_TASKS so decompose can never fire),
/// no pending directive ⇒ the waiting yield/steer phase for every seed.
pub fn non_terminal_sections() -> String {
    format!(
        "## Goal\nWrite a short onboarding guide for new contributors.\n\n\
         {EIGHT_TASK_BOARD}\n\n\
         ## Knowledge retrievals\n(none)\n\n\
         ## Fresh messages\n(none)\n\n\
         ## Directives\n(none)"
    )
}

pub fn pending_directive_sections(directive_line: &str) -> String {
    format!(
        "## Goal\nWrite a short onboarding guide for new contributors.\n\n\
         {EIGHT_TASK_BOARD}\n\n\
         ## Knowledge retrievals\n(none)\n\n\
         ## Fresh messages\n(none)\n\n\
         ## Directives\n{directive_line}"
    )
}

pub fn all_terminal_sections() -> String {
    "## Goal\nWrite a short onboarding guide for new contributors.\n\n\
     ## Board digest\n\
     - task 1 [Done] team:t1  \"Draft the setup section\"\n\
     - task 2 [Done] team:t1  \"Draft the architecture overview\"\n\
     - task 3 [Done] team:t1  \"Review the guide notes\"\n\
     - task 4 [Done] team:t1  \"Outline the contributors plan\"\n\
     - task 5 [Cancelled] team:t1  \"Research onboarding details\"\n\
     - task 6 [Done] team:t1  \"Summarize setup findings\"\n\
     - task 7 [Done] team:t1  \"Draft the welcome section\"\n\
     - task 8 [Done] team:t1  \"Review the build notes\"\n\
     run-health: done 7/8 · agents 0W/3I/0S · mailbox depth 2 (max 2) · ticks-since-done 0\n\n\
     ## Knowledge retrievals\n(none)\n\n\
     ## Fresh messages\n(none)\n\n\
     ## Directives\n(none)"
        .to_owned()
}

pub const META_FRESH_SECTIONS: &str = "## Goal\n\
Write a short onboarding guide for new contributors.\n\n\
## Metrics digest\n\
throughput: 0 task_completed / 15 EventIds · latency: work n/a\n\
utilization:\n\
\u{20}\u{20}- agent-1: Working (task 1), generalist\n\
\u{20}\u{20}- agent-2: Working (task 2), generalist\n\
\u{20}\u{20}- agent-3: Idle, generalist (idle 8)\n\
mailbox: depth 0, max 1, oldest-pending-age 0\n\
tokens: run 2.6k · faults: parks 0, malformed[a1:0 a2:0 a3:0] · directives: issued 0/ful 0/dec 0\n\n\
## Directive outcomes\n\
(none issued)\n\n\
## Recent events\n\
- event 5 task_claimed (agent-1)";

pub const META_JUDGMENT_USED_SECTIONS: &str = "## Goal\n\
Write a short onboarding guide for new contributors.\n\n\
## Metrics digest\n\
throughput: 1 task_completed / 23 EventIds · latency: work median 6 EventIds\n\
utilization:\n\
\u{20}\u{20}- agent-1: Working (task 1), generalist\n\
\u{20}\u{20}- agent-2: Idle, generalist (idle 4)\n\
\u{20}\u{20}- agent-3: Idle, doc-reviewer (idle 3)\n\
mailbox: depth 0, max 1, oldest-pending-age 0\n\
tokens: run 3.4k · faults: parks 0 · directives: issued 1/ful 1/dec 0\n\n\
## Directive outcomes\n\
- directive 1 [judgment] propose_respecialize{agent:agent-3, specialty:doc-reviewer} — fulfilled by orchestrator\n\n\
## Recent events\n\
- event 21 directive_fulfilled (orchestrator)";

pub const META_BOTH_USED_SECTIONS: &str = "## Goal\n\
Write a short onboarding guide for new contributors.\n\n\
## Metrics digest\n\
throughput: 2 task_completed / 31 EventIds · latency: work median 6 EventIds\n\
utilization:\n\
\u{20}\u{20}- agent-1: Idle, generalist (idle 1)\n\
\u{20}\u{20}- agent-2: Idle, generalist (idle 6)\n\
\u{20}\u{20}- agent-3: Idle, doc-reviewer (idle 5)\n\
mailbox: depth 2, max 2, oldest-pending-age 4\n\
tokens: run 4.1k · faults: parks 0 · directives: issued 2/ful 2/dec 0\n\n\
## Directive outcomes\n\
- directive 1 [judgment] propose_respecialize{agent:agent-3, specialty:doc-reviewer} — fulfilled by orchestrator\n\
- directive 2 [mechanical] set_parallelism{target:2} — fulfilled by runtime\n\n\
## Recent events\n\
- event 25 parallelism_changed (meta-1)";

pub const META_ALL_WORKING_SECTIONS: &str = "## Goal\n\
Write a short onboarding guide for new contributors.\n\n\
## Metrics digest\n\
throughput: 0 task_completed / 12 EventIds · latency: work n/a\n\
utilization:\n\
\u{20}\u{20}- agent-1: Working (task 1), generalist\n\
\u{20}\u{20}- agent-2: Working (task 2), doc-reviewer\n\
mailbox: depth 0, max 0, oldest-pending-age 0\n\
tokens: run 1.2k · faults: parks 0 · directives: issued 0/ful 0/dec 0\n\n\
## Directive outcomes\n\
(none issued)\n\n\
## Recent events\n\
- event 5 task_claimed (agent-1)";

pub const PENDING_DIRECTIVE_LINE: &str = "- directive 1 [judgment, pending] propose_respecialize{agent:agent-3, specialty:doc-reviewer} from meta-1";
pub const PENDING_REALLOCATE_LINE: &str = "- directive 2 [judgment, pending] propose_reallocate{task:3, reason:\"stalled claim\"} from meta-1";
pub const PENDING_REBALANCE_LINE: &str = "- directive 3 [judgment, pending] propose_rebalance{team:t1, members:[agent-1 agent-2]} from meta-1";

/// A team-agent view with nothing claimable: Idle and every visible task
/// terminal or claimed by someone else.
pub const NOTHING_ELIGIBLE_SECTIONS: &str = "## Goal\n\
Write a short onboarding guide for new contributors.\n\n\
## Board digest\n\
- task 1 [Claimed by agent-1] team:t1  \"Draft the setup section\"\n\
- task 2 [Done] team:t1  \"Draft the architecture overview\"\n\n\
## Claimed task\n\
(none)\n\n\
## Recent activity\n\
(none)\n\n\
## Fresh messages\n\
(none)";

// ---------------------------------------------------------------------------
// The sweep cases and the schema-check oracle (ADR 0025).
// ---------------------------------------------------------------------------

/// What a sweep case asserts beyond schema validity, for every seed.
#[derive(Clone, Copy)]
pub enum Expect {
    /// Must be the mandatory no-tool-call yield.
    Yield,
    /// Must emit ≥1 call, every verb within the allowed set.
    CallsAmong(&'static [&'static str]),
    /// May yield; any calls must be within the allowed set.
    YieldOrCallsAmong(&'static [&'static str]),
}

pub struct SweepCase {
    pub name: &'static str,
    pub user: String,
    pub call_seq: u64,
    pub request: ChatCompletionRequest,
    pub expect: Expect,
}

/// The synthetic fixture set the fixed seed sweep drives through
/// `BehaviorModel::chat` (ADR 0025).
pub fn sweep_cases() -> Vec<SweepCase> {
    let mut cases = vec![SweepCase {
        name: "orch-empty-board",
        user: "orchestrator".into(),
        call_seq: 0,
        request: orchestrator_request(EMPTY_BOARD_SECTIONS),
        expect: Expect::CallsAmong(&["create_task", "form_team"]),
    }];
    cases.push(SweepCase {
        name: "orch-partial-board",
        user: "orchestrator".into(),
        call_seq: 2,
        request: orchestrator_request(
            "## Goal\nWrite a short onboarding guide for new contributors.\n\n\
             ## Board digest\n\
             - task 1 [Open] team:t1  \"Draft the setup section\"\n\
             run-health: done 0/1 · agents 0W/3I/0S · mailbox depth 0 (max 0) · ticks-since-done 1\n\n\
             ## Directives\n(none)",
        ),
        expect: Expect::YieldOrCallsAmong(&["create_task", "post_message"]),
    });
    cases.push(SweepCase {
        name: "orch-pending-respecialize",
        user: "orchestrator".into(),
        call_seq: 4,
        request: orchestrator_request(&pending_directive_sections(PENDING_DIRECTIVE_LINE)),
        expect: Expect::CallsAmong(&["respecialize", "decline_directive"]),
    });
    cases.push(SweepCase {
        name: "orch-pending-reallocate",
        user: "orchestrator".into(),
        call_seq: 4,
        request: orchestrator_request(&pending_directive_sections(PENDING_REALLOCATE_LINE)),
        expect: Expect::CallsAmong(&["unassign_task", "decline_directive"]),
    });
    cases.push(SweepCase {
        name: "orch-pending-rebalance",
        user: "orchestrator".into(),
        call_seq: 4,
        request: orchestrator_request(&pending_directive_sections(PENDING_REBALANCE_LINE)),
        expect: Expect::CallsAmong(&["set_team_members", "decline_directive"]),
    });
    cases.push(SweepCase {
        name: "orch-non-terminal",
        user: "orchestrator".into(),
        call_seq: 2,
        request: orchestrator_request(&non_terminal_sections()),
        expect: Expect::YieldOrCallsAmong(&["post_message"]),
    });
    cases.push(SweepCase {
        name: "orch-all-terminal",
        user: "orchestrator".into(),
        call_seq: 6,
        request: orchestrator_request(&all_terminal_sections()),
        expect: Expect::CallsAmong(&["finish_run"]),
    });
    let mut after_batch = orchestrator_request(EMPTY_BOARD_SECTIONS);
    push_turn_local(
        &mut after_batch,
        "create_task",
        "{\"title\":\"T\",\"description\":\"D\",\"team\":\"t1\"}",
        "{\"status\":\"ok\",\"result\":{\"task\":1}}",
    );
    cases.push(SweepCase {
        name: "orch-after-batch",
        user: "orchestrator".into(),
        call_seq: 1,
        request: after_batch,
        expect: Expect::Yield,
    });

    cases.push(SweepCase {
        name: "team-idle-open",
        user: "team-agent:agent-1:generalist".into(),
        call_seq: 0,
        request: team_request(TWO_OPEN_TASKS_SECTIONS),
        expect: Expect::CallsAmong(&["claim_task"]),
    });
    cases.push(SweepCase {
        name: "team-working-fresh",
        user: "team-agent:agent-2:generalist".into(),
        call_seq: 2,
        request: team_request(WORKING_NO_ACTIVITY_SECTIONS),
        expect: Expect::CallsAmong(&["write_knowledge", "post_message", "search_knowledge"]),
    });
    cases.push(SweepCase {
        name: "team-working-saturated",
        user: "team-agent:agent-2:generalist".into(),
        call_seq: 6,
        request: team_request(&working_sections_with_actions(3)),
        expect: Expect::CallsAmong(&["complete_task"]),
    });
    cases.push(SweepCase {
        name: "team-degraded-window",
        user: "team-agent:agent-2:generalist".into(),
        call_seq: 4,
        request: team_request(
            "## Goal\nWrite a short onboarding guide for new contributors.\n\n\
             ## Board digest\n\
             - task 2 [Claimed by agent-2] team:t1  \"Draft the architecture overview\"\n\n\
             ## Claimed task\ntask 2 — \"Draft the architecture overview\" (team t1)\n\n\
             ## Recent activity\n(degraded: 2 dropped)\n\n\
             ## Fresh messages\n(none)",
        ),
        expect: Expect::CallsAmong(&["complete_task"]),
    });
    let mut lost_claim = team_request(TWO_OPEN_TASKS_SECTIONS);
    push_turn_local(
        &mut lost_claim,
        "claim_task",
        "{\"task\":1}",
        "{\"status\":\"rejected\",\"code\":\"task_not_open\",\"message\":\"task 1 is not Open\"}",
    );
    cases.push(SweepCase {
        name: "team-lost-claim",
        user: "team-agent:agent-3:generalist".into(),
        call_seq: 1,
        request: lost_claim,
        expect: Expect::Yield,
    });
    cases.push(SweepCase {
        name: "team-nothing-eligible",
        user: "team-agent:agent-3:generalist".into(),
        call_seq: 2,
        request: team_request(NOTHING_ELIGIBLE_SECTIONS),
        expect: Expect::Yield,
    });

    cases.push(SweepCase {
        name: "meta-fresh",
        user: "meta-agent:meta-1".into(),
        call_seq: 0,
        request: meta_request(META_FRESH_SECTIONS),
        expect: Expect::CallsAmong(&["propose_respecialize"]),
    });
    cases.push(SweepCase {
        name: "meta-judgment-used",
        user: "meta-agent:meta-1".into(),
        call_seq: 2,
        request: meta_request(META_JUDGMENT_USED_SECTIONS),
        expect: Expect::CallsAmong(&["set_parallelism"]),
    });
    cases.push(SweepCase {
        name: "meta-both-used",
        user: "meta-agent:meta-1".into(),
        call_seq: 4,
        request: meta_request(META_BOTH_USED_SECTIONS),
        expect: Expect::Yield,
    });
    cases.push(SweepCase {
        name: "meta-no-idle-generalist",
        user: "meta-agent:meta-1".into(),
        call_seq: 0,
        request: meta_request(META_ALL_WORKING_SECTIONS),
        expect: Expect::CallsAmong(&["propose_reallocate"]),
    });

    cases.push(SweepCase {
        name: "plain-client",
        user: "some-arbitrary-client".into(),
        call_seq: 0,
        request: orchestrator_request("What is the capital of France?"),
        expect: Expect::Yield,
    });

    cases
}

/// The schema-check oracle (ADR 0025): every emitted call must name a tool
/// from the request's `tools` array with arguments that are a JSON object,
/// carry every required property, introduce no key outside the schema's
/// properties, and match property types shallowly. Also checks tool-call id
/// uniqueness and the finish/`tool_calls` pairing.
pub fn assert_schema_valid(
    req: &ChatCompletionRequest,
    decision: &ChatDecision,
) -> Result<(), String> {
    let calls = decision.message.tool_calls.as_deref().unwrap_or_default();
    match decision.finish {
        FinishReason::ToolCalls if calls.is_empty() => {
            return Err("finish tool_calls with no calls".into());
        }
        FinishReason::Stop if !calls.is_empty() => {
            return Err("finish stop with tool calls".into());
        }
        _ => {}
    }
    let mut seen_ids = std::collections::BTreeSet::new();
    for call in calls {
        if !seen_ids.insert(call.id.as_str()) {
            return Err(format!("duplicate tool-call id {}", call.id));
        }
        let tool = req
            .tools
            .iter()
            .flatten()
            .find(|tool| tool.function.name == call.function.name)
            .ok_or_else(|| format!("verb {} is not in the tools array", call.function.name))?;
        let args: Value = serde_json::from_str(&call.function.arguments)
            .map_err(|fault| format!("{}: arguments not JSON: {fault}", call.function.name))?;
        let object = args
            .as_object()
            .ok_or_else(|| format!("{}: arguments not an object", call.function.name))?;
        let Some(schema) = &tool.function.parameters else {
            if object.is_empty() {
                continue;
            }
            return Err(format!(
                "{}: args for a parameterless tool",
                call.function.name
            ));
        };
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    return Err(format!("{}: missing required {key}", call.function.name));
                }
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, value) in object {
            let Some(properties) = properties else {
                return Err(format!(
                    "{}: key {key} on a property-less schema",
                    call.function.name
                ));
            };
            let Some(property) = properties.get(key) else {
                return Err(format!(
                    "{}: key {key} outside the schema",
                    call.function.name
                ));
            };
            if let Some(expected) = property.get("type")
                && !type_fits(expected, value)
            {
                return Err(format!(
                    "{}: key {key} has type-mismatched value {value}",
                    call.function.name
                ));
            }
        }
    }
    Ok(())
}

fn type_fits(expected: &Value, value: &Value) -> bool {
    match expected {
        Value::String(kind) => single_type_fits(kind, value),
        Value::Array(kinds) => kinds
            .iter()
            .filter_map(Value::as_str)
            .any(|kind| single_type_fits(kind, value)),
        _ => true,
    }
}

fn single_type_fits(kind: &str, value: &Value) -> bool {
    match kind {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    }
}
