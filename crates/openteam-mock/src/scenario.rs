//! The scenario player: a second `BehaviorModel` adapter driven by a
//! call-seq-indexed JSON fixture (ADR 0023).
//!
//! A scenario exists to make the prior-art failure modes reachable as tests —
//! the pathologies the built-in arc structurally cannot produce. The player is
//! a pure function of `(request, identity)` with zero run-state: the wire's
//! own monotonic `call_seq` doubles as the list cursor, so no per-agent "next
//! response" state exists. Keying is **seed-independent**; only fallthrough
//! (arc) turns consume the seed. Validation is **structural, never semantic**:
//! a scripted `Call` is deliberately unchecked against any tool registry or
//! schema — emitting an unknown verb or bad args is exactly how a scenario
//! drives the `invalid` tool-outcome and the K=3 park path (ADR 0017).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;

use openteam_wire::{
    AgentId, ChatCompletionRequest, FinishReason, FunctionCall, ParsedUser, ResponseMessage, Role,
    ToolCall, ToolType, WireIdentity,
};

use crate::arc::BuiltinArc;
use crate::behavior::{BehaviorModel, ChatDecision};

/// The scenario schema version this build supports (ADR 0023 — the off-wire
/// home of any legibility-contract version marker).
pub const SCENARIO_VERSION: u32 = 1;

/// A typed scenario-loading fault. Any of these aborts the run fail-fast.
#[derive(Debug, thiserror::Error)]
pub enum ScenarioError {
    #[error("failed to read scenario file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("scenario file is not structurally valid: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("unsupported scenario version {found}; this build supports version {SCENARIO_VERSION}")]
    UnsupportedVersion { found: u32 },
}

/// A JSON fixture file overriding the built-in behavior model with scripted
/// chat responses (CONTEXT.md: Scenario). Chat-only — embeddings bypass the
/// seam and are never scenario-overridable (ADR 0019/0023).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// Schema version; v1 == 1.
    pub version: u32,
    /// Doc only.
    #[serde(default)]
    pub description: Option<String>,
    /// Doc only: the pathology / ADR the fixture exercises.
    #[serde(default)]
    pub reproduces: Option<String>,
    pub scripts: Vec<Script>,
}

impl Scenario {
    /// Parse and structurally validate a scenario from JSON text.
    pub fn from_json_str(json: &str) -> Result<Self, ScenarioError> {
        let scenario: Self = serde_json::from_str(json)?;
        if scenario.version != SCENARIO_VERSION {
            return Err(ScenarioError::UnsupportedVersion {
                found: scenario.version,
            });
        }
        Ok(scenario)
    }

    /// Load a scenario from a file path.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ScenarioError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ScenarioError::Io {
            path: path.to_owned(),
            source,
        })?;
        Self::from_json_str(&text)
    }
}

/// One agent-or-role's entry: a selector plus an ordered response list
/// indexed directly by the agent's call sequence (CONTEXT.md: Script).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    /// `"orchestrator"` | an exact handle (`"agent-2"`, `"meta-1"`) |
    /// `"agent-*"` | `"meta-*"`.
    pub agent: Selector,
    /// `responses[call_seq]` (0-based).
    pub responses: Vec<Response>,
    /// Past the list: `repeat[(seq - len) % repeat.len()]`; empty ⇒
    /// fallthrough to the built-in arc.
    #[serde(default)]
    pub repeat: Vec<Response>,
}

/// The script selector grammar (ADR 0023).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub enum Selector {
    /// An exact agent handle — beats a role wildcard.
    Exact(String),
    /// `agent-*`: any team agent.
    TeamWildcard,
    /// `meta-*`: any meta-agent.
    MetaWildcard,
}

impl Selector {
    fn matches(&self, handle: &str, role: Role) -> bool {
        match self {
            Self::Exact(exact) => exact == handle,
            Self::TeamWildcard => role == Role::TeamAgent,
            Self::MetaWildcard => role == Role::MetaAgent,
        }
    }
}

impl TryFrom<String> for Selector {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "agent-*" => Ok(Self::TeamWildcard),
            "meta-*" => Ok(Self::MetaWildcard),
            handle => AgentId::parse(handle)
                .map(|_| Self::Exact(value.clone()))
                .map_err(|_| {
                    format!(
                        "invalid agent selector {value:?}: expected \"orchestrator\", an exact \
                     handle, \"agent-*\", or \"meta-*\""
                    )
                }),
        }
    }
}

/// One scripted response (ADR 0023, serde-untagged): the literal string
/// `"yield"`, the literal string `"fallthrough"`, or a `Say` object.
#[derive(Debug, Clone)]
pub enum Response {
    /// Clean no-tool-call stop.
    Yield,
    /// Delegate THIS completion to the built-in arc.
    Fallthrough,
    Say(Say),
}

impl<'de> Deserialize<'de> for Response {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::String(keyword) => match keyword.as_str() {
                "yield" => Ok(Self::Yield),
                "fallthrough" => Ok(Self::Fallthrough),
                other => Err(D::Error::custom(format!(
                    "unknown response keyword {other:?}: expected \"yield\" or \"fallthrough\""
                ))),
            },
            object @ Value::Object(_) => Say::deserialize(object)
                .map(Self::Say)
                .map_err(D::Error::custom),
            other => Err(D::Error::custom(format!(
                "a response must be \"yield\", \"fallthrough\", or a say object, got {other}"
            ))),
        }
    }
}

/// A scripted assistant message. `text: null` with empty `tool_calls` is
/// legal — a plain yield-ish say. `finish` defaults to `tool_calls` when
/// `tool_calls` is non-empty, else `stop`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Say {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<Call>,
    #[serde(default)]
    pub finish: Option<FinishReason>,
}

/// One scripted tool call. Deliberately NEVER validated against any verb
/// registry or schema — scripting `invalid` calls is the point (ADR 0023).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub name: String,
    /// A JSON value inlined verbatim into the wire `arguments` string.
    pub arguments: Value,
}

/// The second `BehaviorModel` adapter (ADR 0023): serves a script's response
/// for a matched `(agent, call_seq)`, delegating everything else to the
/// built-in arc it owns.
pub struct ScenarioPlayer {
    scenario: Scenario,
    arc: Arc<BuiltinArc>,
}

impl ScenarioPlayer {
    pub fn new(scenario: Scenario) -> Self {
        Self {
            scenario,
            arc: Arc::new(BuiltinArc::new()),
        }
    }

    /// The most specific matching script: an exact-handle selector beats a
    /// role wildcard; ties go to the first script in file order.
    fn select(&self, handle: &str, role: Role) -> Option<&Script> {
        let matching = || {
            self.scenario
                .scripts
                .iter()
                .filter(|script| script.agent.matches(handle, role))
        };
        matching()
            .find(|script| matches!(script.agent, Selector::Exact(_)))
            .or_else(|| matching().next())
    }
}

impl BehaviorModel for ScenarioPlayer {
    fn chat(&self, req: &ChatCompletionRequest, id: &WireIdentity) -> ChatDecision {
        // Selector matching is on the identity channels only (never content,
        // never the seed): the handle/role parsed from the `user` field plus
        // the call-seq header, which IS the list cursor.
        let Ok(parsed) = ParsedUser::parse(&id.user) else {
            return self.arc.chat(req, id);
        };
        let handle = parsed.agent();
        let Some(script) = self.select(handle.as_str(), parsed.role()) else {
            return self.arc.chat(req, id);
        };
        let seq = usize::try_from(id.call_seq).unwrap_or(usize::MAX);
        let response = if seq < script.responses.len() {
            &script.responses[seq]
        } else if !script.repeat.is_empty() {
            &script.repeat[(seq - script.responses.len()) % script.repeat.len()]
        } else {
            return self.arc.chat(req, id);
        };
        match response {
            Response::Fallthrough => self.arc.chat(req, id),
            Response::Yield => ChatDecision {
                message: ResponseMessage {
                    role: "assistant".into(),
                    content: Some("Yielding.".into()),
                    refusal: None,
                    tool_calls: None,
                },
                finish: FinishReason::Stop,
            },
            Response::Say(say) => realize_say(say, id),
        }
    }
}

fn realize_say(say: &Say, id: &WireIdentity) -> ChatDecision {
    let tool_calls = if say.tool_calls.is_empty() {
        None
    } else {
        let handle = crate::arc::handle_ish(&id.user);
        Some(
            say.tool_calls
                .iter()
                .enumerate()
                .map(|(index, call)| ToolCall {
                    id: format!("call_{handle}_{}_{}", id.call_seq, index + 1),
                    kind: ToolType::Function,
                    function: FunctionCall {
                        name: call.name.clone(),
                        arguments: call.arguments.to_string(),
                    },
                })
                .collect(),
        )
    };
    let finish = say.finish.unwrap_or(if tool_calls.is_some() {
        FinishReason::ToolCalls
    } else {
        FinishReason::Stop
    });
    ChatDecision {
        message: ResponseMessage {
            role: "assistant".into(),
            content: say.text.clone(),
            refusal: None,
            tool_calls,
        },
        finish,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    fn player(json: &str) -> ScenarioPlayer {
        ScenarioPlayer::new(Scenario::from_json_str(json).expect("valid scenario"))
    }

    fn chat(player: &ScenarioPlayer, user: &str, call_seq: u64) -> ChatDecision {
        let req = fixtures::team_request(fixtures::TWO_OPEN_TASKS_SECTIONS);
        player.chat(&req, &fixtures::identity(user, call_seq, 42))
    }

    fn text_of(decision: &ChatDecision) -> String {
        decision.message.content.clone().unwrap_or_default()
    }

    #[test]
    fn literals_and_say_parse() {
        let scenario = Scenario::from_json_str(
            r#"{
                "version": 1,
                "description": "doc",
                "reproduces": "stall (prior-art §10)",
                "scripts": [
                    { "agent": "agent-1",
                      "responses": ["yield", "fallthrough",
                        {"text": "hi", "tool_calls": [], "finish": "stop"}],
                      "repeat": [{"tool_calls": [{"name": "nop", "arguments": {}}]}] }
                ]
            }"#,
        )
        .expect("parses");
        assert_eq!(scenario.scripts.len(), 1);
        assert!(matches!(scenario.scripts[0].responses[0], Response::Yield));
        assert!(matches!(
            scenario.scripts[0].responses[1],
            Response::Fallthrough
        ));
        assert!(matches!(scenario.scripts[0].responses[2], Response::Say(_)));
    }

    #[test]
    fn structural_validation_permits_invalid_scripted_calls() {
        // An unknown verb name and junk args are exactly what the K=3 park
        // fixture needs — the loader must NOT reject them (ADR 0023).
        let scripted = player(
            r#"{"version": 1, "scripts": [
                { "agent": "agent-1", "responses": [
                    {"tool_calls": [{"name": "definitely_not_a_verb",
                                     "arguments": {"bogus": [1, 2, {"x": null}]}}]}
                ] }
            ]}"#,
        );
        let decision = chat(&scripted, "team-agent:agent-1:generalist", 0);
        assert_eq!(decision.finish, FinishReason::ToolCalls);
        let calls = decision.message.tool_calls.expect("scripted call");
        assert_eq!(calls[0].function.name, "definitely_not_a_verb");
        let args: Value = serde_json::from_str(&calls[0].function.arguments).expect("json");
        assert_eq!(args["bogus"][2]["x"], Value::Null);
    }

    #[test]
    fn unknown_fields_fail_fast_at_every_level() {
        for json in [
            r#"{"version": 1, "scripts": [], "stray": true}"#,
            r#"{"version": 1, "scripts": [{"agent": "agent-1", "responses": [], "cursor": 0}]}"#,
            r#"{"version": 1, "scripts": [{"agent": "agent-1", "responses": [{"text": "x", "extra": 1}]}]}"#,
            r#"{"version": 1, "scripts": [{"agent": "agent-1", "responses": [{"tool_calls": [{"name": "x", "arguments": {}, "id": "call_1"}]}]}]}"#,
        ] {
            assert!(
                matches!(Scenario::from_json_str(json), Err(ScenarioError::Parse(_))),
                "should fail structurally: {json}"
            );
        }
    }

    #[test]
    fn unknown_version_and_bad_selector_fail_fast() {
        assert!(matches!(
            Scenario::from_json_str(r#"{"version": 2, "scripts": []}"#),
            Err(ScenarioError::UnsupportedVersion { found: 2 })
        ));
        assert!(matches!(
            Scenario::from_json_str(
                r#"{"version": 1, "scripts": [{"agent": "worker-*", "responses": []}]}"#
            ),
            Err(ScenarioError::Parse(_))
        ));
        assert!(matches!(
            Scenario::from_json_str(
                r#"{"version": 1, "scripts": [{"agent": "yield", "responses": []}]}"#
            ),
            Err(ScenarioError::Parse(_))
        ));
    }

    #[test]
    fn unknown_response_keyword_fails_fast() {
        assert!(matches!(
            Scenario::from_json_str(
                r#"{"version": 1, "scripts": [{"agent": "agent-1", "responses": ["stop"]}]}"#
            ),
            Err(ScenarioError::Parse(_))
        ));
    }

    #[test]
    fn exact_handle_beats_role_wildcard() {
        let scripted = player(
            r#"{"version": 1, "scripts": [
                { "agent": "agent-*", "responses": [{"text": "wildcard"}],
                  "repeat": [{"text": "wildcard"}] },
                { "agent": "agent-2", "responses": [{"text": "exact"}],
                  "repeat": [{"text": "exact"}] }
            ]}"#,
        );
        assert_eq!(
            text_of(&chat(&scripted, "team-agent:agent-2:generalist", 0)),
            "exact"
        );
        assert_eq!(
            text_of(&chat(&scripted, "team-agent:agent-1:generalist", 0)),
            "wildcard"
        );
    }

    #[test]
    fn call_seq_is_the_cursor_and_repeat_cycles() {
        let scripted = player(
            r#"{"version": 1, "scripts": [
                { "agent": "agent-1",
                  "responses": [{"text": "r0"}, {"text": "r1"}],
                  "repeat": [{"text": "loop-a"}, {"text": "loop-b"}] }
            ]}"#,
        );
        let user = "team-agent:agent-1:generalist";
        assert_eq!(text_of(&chat(&scripted, user, 0)), "r0");
        assert_eq!(text_of(&chat(&scripted, user, 1)), "r1");
        assert_eq!(text_of(&chat(&scripted, user, 2)), "loop-a");
        assert_eq!(text_of(&chat(&scripted, user, 3)), "loop-b");
        assert_eq!(text_of(&chat(&scripted, user, 4)), "loop-a");
        assert_eq!(text_of(&chat(&scripted, user, 5)), "loop-b");
    }

    #[test]
    fn matching_is_seed_independent() {
        let scripted = player(
            r#"{"version": 1, "scripts": [
                { "agent": "agent-1", "responses": [{"text": "scripted"}] }
            ]}"#,
        );
        let req = fixtures::team_request(fixtures::TWO_OPEN_TASKS_SECTIONS);
        for seed in [0_u64, 42, u64::MAX] {
            let decision = scripted.chat(
                &req,
                &fixtures::identity("team-agent:agent-1:generalist", 0, seed),
            );
            assert_eq!(text_of(&decision), "scripted");
        }
    }

    #[test]
    fn empty_repeat_past_the_list_falls_through_to_the_arc() {
        let scripted = player(
            r#"{"version": 1, "scripts": [
                { "agent": "agent-1", "responses": [{"text": "r0"}] }
            ]}"#,
        );
        // Past the list with no repeat: the arc decides — an idle agent with
        // Open tasks visible claims the lowest id.
        let decision = chat(&scripted, "team-agent:agent-1:generalist", 5);
        let calls = decision.message.tool_calls.expect("arc claim");
        assert_eq!(calls[0].function.name, "claim_task");
    }

    #[test]
    fn explicit_fallthrough_delegates_one_completion() {
        let scripted = player(
            r#"{"version": 1, "scripts": [
                { "agent": "agent-1", "responses": ["fallthrough", {"text": "back"}] }
            ]}"#,
        );
        let decision = chat(&scripted, "team-agent:agent-1:generalist", 0);
        let calls = decision.message.tool_calls.expect("arc claim");
        assert_eq!(calls[0].function.name, "claim_task");
        assert_eq!(
            text_of(&chat(&scripted, "team-agent:agent-1:generalist", 1)),
            "back"
        );
    }

    #[test]
    fn unmatched_agents_and_unparseable_users_fall_through() {
        let scripted = player(
            r#"{"version": 1, "scripts": [
                { "agent": "meta-*", "responses": ["yield"] }
            ]}"#,
        );
        // A team agent matches no script: arc claims.
        let decision = chat(&scripted, "team-agent:agent-1:generalist", 0);
        assert!(decision.message.tool_calls.is_some());
        // An unparseable user falls through to the arc's plain-client yield.
        let decision = chat(&scripted, "just-a-client", 0);
        assert_eq!(decision.finish, FinishReason::Stop);
    }

    #[test]
    fn finish_defaults_and_overrides() {
        let scripted = player(
            r#"{"version": 1, "scripts": [
                { "agent": "orchestrator", "responses": [
                    {"tool_calls": [{"name": "create_task", "arguments": {"title": "T", "description": "D"}}]},
                    {"text": "plain"},
                    {"text": "cut short", "finish": "length"},
                    {}
                ] }
            ]}"#,
        );
        let user = "orchestrator";
        assert_eq!(
            chat(&scripted, user, 0).finish,
            FinishReason::ToolCalls,
            "non-empty tool_calls defaults to tool_calls"
        );
        assert_eq!(chat(&scripted, user, 1).finish, FinishReason::Stop);
        assert_eq!(chat(&scripted, user, 2).finish, FinishReason::Length);
        // text:null + empty tool_calls is legal: a plain yield-ish say.
        let bare = chat(&scripted, user, 3);
        assert_eq!(bare.finish, FinishReason::Stop);
        assert_eq!(bare.message.content, None);
    }

    #[test]
    fn scripted_call_ids_are_deterministic() {
        let scripted = player(
            r#"{"version": 1, "scripts": [
                { "agent": "agent-1", "responses": [
                    {"tool_calls": [{"name": "a", "arguments": {}}, {"name": "b", "arguments": {}}]}
                ] }
            ]}"#,
        );
        let decision = chat(&scripted, "team-agent:agent-1:generalist", 0);
        let calls = decision.message.tool_calls.expect("calls");
        assert_eq!(calls[0].id, "call_agent1_0_1");
        assert_eq!(calls[1].id, "call_agent1_0_2");
    }

    #[test]
    fn from_path_loads_and_reports_io_faults() {
        let missing = Scenario::from_path("/nonexistent/scenario.json");
        assert!(matches!(missing, Err(ScenarioError::Io { .. })));
    }
}
