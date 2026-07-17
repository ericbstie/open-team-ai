//! The coordination-verb registry and the three-way tool-outcome envelope
//! (ADR 0017).
//!
//! One fixed registry per role — team-agent (7), orchestrator (14),
//! meta-agent (6) — dispatch-by-name to plain async handlers (in
//! `runtime`), no per-verb trait. Tool `parameters` are schemars 1.x JSON
//! Schema (draft 2020-12) rendered once at startup from each verb's typed
//! args struct; `deny_unknown_fields` → `additionalProperties: false` is
//! what makes a bad-args call `invalid` (ADR 0017).

use openteam_wire::{FunctionDef, Role, ToolDef, ToolType};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::board::BoardRejection;

/// The three-way tool-outcome envelope (ADR 0017), serialized as the string
/// content of the wire's one `role:"tool"` reply per `tool_call_id`:
/// `ok` (executed) / `rejected` (a well-formed call the domain refused) /
/// `invalid` (a schema/parse fault — the only kind that feeds the
/// malformed-park counter).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ToolOutcome {
    Ok {
        result: serde_json::Value,
    },
    Rejected {
        code: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
    },
    Invalid {
        code: String,
        message: String,
    },
}

impl ToolOutcome {
    pub fn ok(result: serde_json::Value) -> Self {
        Self::Ok { result }
    }

    pub fn rejected(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Rejected {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    pub fn unknown_verb(name: &str) -> Self {
        Self::Invalid {
            code: "unknown_verb".into(),
            message: format!("no verb named {name:?} in this registry"),
        }
    }

    pub fn invalid_arguments(message: impl Into<String>) -> Self {
        Self::Invalid {
            code: "invalid_arguments".into(),
            message: message.into(),
        }
    }

    /// `ok` / `rejected` / `invalid` — the recent-activity line suffix.
    pub fn word(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "ok",
            Self::Rejected { .. } => "rejected",
            Self::Invalid { .. } => "invalid",
        }
    }

    pub fn is_invalid(&self) -> bool {
        matches!(self, Self::Invalid { .. })
    }

    /// The serialized envelope — the `role:"tool"` message content.
    pub fn to_content(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            // Serialization of this closed enum over Value cannot fail; keep
            // a defensive envelope rather than panicking in the lib.
            r#"{"status":"invalid","code":"invalid_arguments","message":"unserializable outcome"}"#
                .to_string()
        })
    }
}

impl From<BoardRejection> for ToolOutcome {
    fn from(rejection: BoardRejection) -> Self {
        Self::Rejected {
            code: rejection.code.into(),
            message: rejection.message,
            details: None,
        }
    }
}

// ---- typed argument structs (pins §1) ----------------------------------
//
// Every struct derives `Deserialize` + `JsonSchema` with
// `deny_unknown_fields`, keeping each verb's schema and its arg
// deserializer in lockstep from one type (ADR 0017).

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimTaskArgs {
    /// TaskId to claim.
    pub task: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompleteTaskArgs {
    pub result: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReleaseTaskArgs {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PostMessageArgs {
    /// Direct address: a single agent handle.
    #[serde(default)]
    pub to: Option<String>,
    /// Team address: a live team id.
    #[serde(default)]
    pub team: Option<String>,
    /// Broadcast to the orchestrator and all team agents.
    #[serde(default)]
    pub broadcast: Option<bool>,
    pub body: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteKnowledgeArgs {
    pub text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchKnowledgeArgs {
    pub query: String,
    /// Top-k (default 3).
    #[serde(default)]
    pub k: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SleepArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTaskArgs {
    pub title: String,
    pub description: String,
    /// TeamId tag, or null for untagged.
    #[serde(default)]
    pub team: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelTaskArgs {
    pub task: u64,
    pub reason: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnassignTaskArgs {
    pub task: u64,
    #[serde(default)]
    pub reason: Option<String>,
    /// DirectiveId being fulfilled.
    #[serde(default)]
    pub in_response_to: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FormTeamArgs {
    pub team: String,
    pub members: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DissolveTeamArgs {
    pub team: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetTeamMembersArgs {
    pub team: String,
    pub members: Vec<String>,
    /// DirectiveId being fulfilled.
    #[serde(default)]
    pub in_response_to: Option<u64>,
}

/// The orchestrator-authored 3-field specialty (CONTEXT.md: Specialty).
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpecialtyArgs {
    /// Slug.
    pub name: String,
    pub description: String,
    pub focus: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RespecializeArgs {
    pub agent: String,
    pub specialty: SpecialtyArgs,
    /// DirectiveId being fulfilled.
    #[serde(default)]
    pub in_response_to: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SleepAgentArgs {
    pub agent: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WakeAgentArgs {
    pub agent: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeclineDirectiveArgs {
    pub directive: u64,
    pub reason: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FinishRunArgs {
    pub report: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetParallelismArgs {
    pub target: u32,
}

// The three judgment-proposal arg structs exist for schema generation and
// arg validation only: the runtime stores the raw args object on the
// Directive (the orchestrator interprets it), so the fields are never read
// directly after the typed parse.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct ProposeRespecializeArgs {
    pub agent: String,
    /// Proposed slug/hint; the orchestrator authors the full 3-field
    /// specialty.
    pub specialty: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct ProposeReallocateArgs {
    pub task: u64,
    pub reason: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct ProposeRebalanceArgs {
    pub team: String,
    pub members: Vec<String>,
}

// ---- the registry -------------------------------------------------------

/// Render one `ToolDef` from a typed args struct: schemars draft 2020-12,
/// top-level `$schema` stripped, `strict: false` (ADR 0017).
fn def<T: JsonSchema>(name: &str, description: &str) -> ToolDef {
    let schema = schemars::schema_for!(T);
    let mut parameters = serde_json::to_value(schema).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = parameters.as_object_mut() {
        obj.remove("$schema");
        obj.remove("title");
    }
    ToolDef {
        kind: ToolType::Function,
        function: FunctionDef {
            name: name.into(),
            description: Some(description.into()),
            parameters: Some(parameters),
            strict: Some(false),
        },
    }
}

/// The fixed per-role tool registries (ADR 0017), schemas built once at
/// startup and cached, rendered verbatim into every request's `tools` array
/// (ADR 0013).
#[derive(Debug)]
pub struct ToolRegistry {
    team: Vec<ToolDef>,
    orchestrator: Vec<ToolDef>,
    meta: Vec<ToolDef>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        let claim = def::<ClaimTaskArgs>(
            "claim_task",
            "Take exclusive ownership of an Open task your team is eligible for. First claim wins; a lost race returns a rejected outcome.",
        );
        let complete = def::<CompleteTaskArgs>(
            "complete_task",
            "Mark your claimed task Done and record its result into the knowledge store.",
        );
        let release = def::<ReleaseTaskArgs>(
            "release_task",
            "Return your claimed task to Open, optionally with a reason.",
        );
        let post = def::<PostMessageArgs>(
            "post_message",
            "Send a realtime message: exactly one of `to` (direct to an agent), `team` (a live team), or `broadcast: true`.",
        );
        let write = def::<WriteKnowledgeArgs>(
            "write_knowledge",
            "Record a Note into the shared knowledge store — no delivery; discoverable by later search.",
        );
        let search = def::<SearchKnowledgeArgs>(
            "search_knowledge",
            "Top-k cosine search of the shared knowledge store.",
        );

        let team = vec![
            claim,
            complete,
            release,
            post.clone(),
            write.clone(),
            search.clone(),
            def::<SleepArgs>("sleep", "Go Asleep from Idle until explicitly woken."),
        ];

        let orchestrator = vec![
            def::<CreateTaskArgs>(
                "create_task",
                "Author a new Open task on the board, optionally tagged to a team for claim-eligibility.",
            ),
            def::<CancelTaskArgs>(
                "cancel_task",
                "Cancel an Open task with a reason so the run can converge deliberately.",
            ),
            def::<UnassignTaskArgs>(
                "unassign_task",
                "Forcibly return a Claimed task to Open — the reallocation and pre-respecialization move. Cite a directive with in_response_to to fulfill it.",
            ),
            def::<FormTeamArgs>(
                "form_team",
                "Form a team over pool agents: a routable message scope plus a task claim-eligibility scope.",
            ),
            def::<DissolveTeamArgs>(
                "dissolve_team",
                "Dissolve a team, releasing both scopes. Rejected while live team-tagged tasks remain.",
            ),
            def::<SetTeamMembersArgs>(
                "set_team_members",
                "Declaratively replace a team's member set. Cite a directive with in_response_to to fulfill it.",
            ),
            def::<RespecializeArgs>(
                "respecialize",
                "Swap an Idle agent's specialty and system prompt, wiping its recent-activity window; identity preserved. Cite a directive with in_response_to to fulfill it.",
            ),
            def::<SleepAgentArgs>("sleep_agent", "Put an Idle team agent to sleep."),
            def::<WakeAgentArgs>(
                "wake_agent",
                "Wake a sleeping or parked agent; restores Working with its still-claimed task, else Idle.",
            ),
            def::<DeclineDirectiveArgs>(
                "decline_directive",
                "Decline a pending judgment directive with a logged reason.",
            ),
            post,
            write,
            search,
            def::<FinishRunArgs>(
                "finish_run",
                "End the run. Rejected (enumerating blockers) if any task is Open or Claimed.",
            ),
        ];

        let meta = vec![
            def::<SetParallelismArgs>(
                "set_parallelism",
                "Mechanical directive: retune the effective team-agent permit count within [1, --parallel]. Applied by the runtime.",
            ),
            def::<SleepAgentArgs>(
                "sleep_agent",
                "Mechanical directive: put an Idle team agent to sleep. Applied by the runtime.",
            ),
            def::<WakeAgentArgs>(
                "wake_agent",
                "Mechanical directive: wake a sleeping or parked agent. Applied by the runtime.",
            ),
            def::<ProposeRespecializeArgs>(
                "propose_respecialize",
                "Judgment directive: propose the orchestrator respecialize an agent. Returns a directive id.",
            ),
            def::<ProposeReallocateArgs>(
                "propose_reallocate",
                "Judgment directive: propose the orchestrator unassign and reallocate a claimed task. Returns a directive id.",
            ),
            def::<ProposeRebalanceArgs>(
                "propose_rebalance",
                "Judgment directive: propose the orchestrator rebalance a team's member set. Returns a directive id.",
            ),
        ];

        Self {
            team,
            orchestrator,
            meta,
        }
    }

    /// The role's fixed verb set, rendered verbatim into the request
    /// `tools` array (ADR 0013/0017).
    pub fn tool_defs(&self, role: Role) -> &[ToolDef] {
        match role {
            Role::TeamAgent => &self.team,
            Role::Orchestrator => &self.orchestrator,
            Role::MetaAgent => &self.meta,
        }
    }

    pub fn contains(&self, role: Role, name: &str) -> bool {
        self.tool_defs(role)
            .iter()
            .any(|def| def.function.name == name)
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_counts_match_the_map() {
        let registry = ToolRegistry::new();
        assert_eq!(registry.tool_defs(Role::TeamAgent).len(), 7);
        assert_eq!(registry.tool_defs(Role::Orchestrator).len(), 14);
        assert_eq!(registry.tool_defs(Role::MetaAgent).len(), 6);
    }

    #[test]
    fn schemas_are_strict_objects_without_schema_key() {
        let registry = ToolRegistry::new();
        for def in registry
            .tool_defs(Role::TeamAgent)
            .iter()
            .chain(registry.tool_defs(Role::Orchestrator))
            .chain(registry.tool_defs(Role::MetaAgent))
        {
            let params = def.function.parameters.as_ref().unwrap();
            let obj = params.as_object().unwrap();
            assert!(!obj.contains_key("$schema"), "{}", def.function.name);
            assert_eq!(
                obj.get("additionalProperties"),
                Some(&serde_json::Value::Bool(false)),
                "{} must deny unknown fields",
                def.function.name
            );
            assert_eq!(def.function.strict, Some(false));
        }
    }

    #[test]
    fn outcome_envelope_serializes_the_pinned_shapes() {
        assert_eq!(
            ToolOutcome::ok(serde_json::json!({"task": 1})).to_content(),
            r#"{"status":"ok","result":{"task":1}}"#
        );
        assert_eq!(
            ToolOutcome::rejected("task_not_open", "task 1 is not Open (Claimed by agent-1)")
                .to_content(),
            r#"{"status":"rejected","code":"task_not_open","message":"task 1 is not Open (Claimed by agent-1)"}"#
        );
        assert_eq!(
            ToolOutcome::unknown_verb("bogus").word(),
            "invalid",
            "unknown verb is a schema fault"
        );
        let invalid: ToolOutcome =
            serde_json::from_str(&ToolOutcome::invalid_arguments("bad").to_content()).unwrap();
        assert!(invalid.is_invalid());
    }

    #[test]
    fn args_deny_unknown_fields() {
        let err = serde_json::from_str::<ClaimTaskArgs>(r#"{"task":1,"stray":true}"#);
        assert!(err.is_err());
        let ok = serde_json::from_str::<ClaimTaskArgs>(r#"{"task":1}"#).unwrap();
        assert_eq!(ok.task, 1);
    }
}
