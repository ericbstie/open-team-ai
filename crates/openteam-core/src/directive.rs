//! Two-tier directives and their pinned line grammars (ADR 0005/0020,
//! pins §3).
//!
//! A directive is a meta-agent's process-improvement instruction. Mechanical
//! directives are applied directly by the runtime; judgment directives go to
//! the orchestrator, which must act-with-cite or decline with a logged reason
//! — there is no silent timeout (ADR 0020).

use std::fmt;

use openteam_wire::AgentId;
use serde::{Deserialize, Serialize};

use crate::ids::DirectiveId;

/// Which authority path a directive takes (CONTEXT.md: Directive tier).
///
/// Serializes as `"Judgment"` / `"Mechanical"` in event payloads (pins §7);
/// renders lowercase in the line grammars (pins §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DirectiveTier {
    Judgment,
    Mechanical,
}

impl DirectiveTier {
    /// The lowercase grammar rendering (`judgment` / `mechanical`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Judgment => "judgment",
            Self::Mechanical => "mechanical",
        }
    }
}

impl fmt::Display for DirectiveTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The six typed directive verbs — the meta registry emits nothing else
/// (ADR 0020). Serializes as the snake_case verb name (e.g.
/// `"propose_respecialize"`, `"set_parallelism"`) per ADR 0022's
/// `directive_issued` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectiveKind {
    // Mechanical — applied by the runtime, returning the applied effect.
    SetParallelism,
    SleepAgent,
    WakeAgent,
    // Judgment — enqueued to the orchestrator; the `propose_` prefix keeps
    // them textually distinct from the orchestrator's own action verbs.
    ProposeRespecialize,
    ProposeReallocate,
    ProposeRebalance,
}

impl DirectiveKind {
    /// The snake_case verb name, as serialized and as rendered in grammars.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SetParallelism => "set_parallelism",
            Self::SleepAgent => "sleep_agent",
            Self::WakeAgent => "wake_agent",
            Self::ProposeRespecialize => "propose_respecialize",
            Self::ProposeReallocate => "propose_reallocate",
            Self::ProposeRebalance => "propose_rebalance",
        }
    }

    /// The tier this verb belongs to (ADR 0020's fixed verb tables).
    pub fn tier(&self) -> DirectiveTier {
        match self {
            Self::SetParallelism | Self::SleepAgent | Self::WakeAgent => DirectiveTier::Mechanical,
            Self::ProposeRespecialize | Self::ProposeReallocate | Self::ProposeRebalance => {
                DirectiveTier::Judgment
            }
        }
    }

    /// Canonical argument-key render order for the line grammars (pins §3
    /// shows e.g. `propose_reallocate{task:2, reason:"…"}` — verb-shape
    /// order, not alphabetical).
    fn arg_order(self) -> &'static [&'static str] {
        match self {
            Self::SetParallelism => &["target"],
            Self::SleepAgent | Self::WakeAgent => &["agent"],
            Self::ProposeRespecialize => &["agent", "specialty"],
            Self::ProposeReallocate => &["task", "reason"],
            Self::ProposeRebalance => &["team", "members"],
        }
    }
}

impl fmt::Display for DirectiveKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What has become of an emitted directive (CONTEXT.md: Directive outcome).
/// A mechanical directive is fulfilled at emit time; a judgment one stays
/// `Pending` until the orchestrator acts-with-cite or declines (ADR 0020).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectiveState {
    Pending,
    Fulfilled { by: AgentId },
    Declined { by: AgentId, reason: String },
}

impl DirectiveState {
    /// The lowercase state word of the orchestrator grammar
    /// (`pending` / `fulfilled` / `declined`).
    fn state_word(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Fulfilled { .. } => "fulfilled",
            Self::Declined { .. } => "declined",
        }
    }
}

/// A meta-agent's process-improvement instruction (CONTEXT.md: Directive).
#[derive(Debug, Clone, PartialEq)]
pub struct Directive {
    pub id: DirectiveId,
    pub tier: DirectiveTier,
    pub kind: DirectiveKind,
    /// The verb's argument object, as issued (mirrors
    /// `directive_issued.args`, ADR 0022).
    pub args: serde_json::Value,
    /// The emitting meta-agent.
    pub from: AgentId,
    pub state: DirectiveState,
}

impl Directive {
    pub fn is_pending(&self) -> bool {
        self.state == DirectiveState::Pending
    }

    /// The orchestrator `## Directives` line (ADR 0016, pins §3):
    /// `- directive <id> [<tier>, <state>] <kind>{<args>} from <meta>`.
    pub fn directives_line(&self) -> String {
        format!(
            "- directive {} [{}, {}] {}{{{}}} from {}",
            self.id,
            self.tier,
            self.state.state_word(),
            self.kind,
            render_args(self.kind, &self.args),
            self.from
        )
    }

    /// The meta `## Directive outcomes` line (ADR 0016, pins §3):
    /// `- directive <id> [<tier>] <kind>{<args>} — pending|fulfilled by <h>|declined by <h>: "<reason>"`.
    pub fn outcomes_line(&self) -> String {
        let outcome = match &self.state {
            DirectiveState::Pending => "pending".to_string(),
            DirectiveState::Fulfilled { by } => format!("fulfilled by {by}"),
            DirectiveState::Declined { by, reason } => {
                format!("declined by {by}: \"{reason}\"")
            }
        };
        format!(
            "- directive {} [{}] {}{{{}}} — {}",
            self.id,
            self.tier,
            self.kind,
            render_args(self.kind, &self.args),
            outcome
        )
    }
}

/// Render a directive's args as the pinned `key:value` comma-space pairs
/// (pins §3): bare values for handles/slugs/ints, strings quoted only when
/// they contain spaces/punctuation (reason strings), arrays as `[a b]`.
/// Keys render in the verb's canonical order, then any extras in map order.
fn render_args(kind: DirectiveKind, args: &serde_json::Value) -> String {
    let serde_json::Value::Object(map) = args else {
        return render_value(args);
    };
    let canonical = kind.arg_order();
    let mut pairs: Vec<String> = Vec::with_capacity(map.len());
    for key in canonical {
        if let Some(value) = map.get(*key) {
            pairs.push(format!("{key}:{}", render_value(value)));
        }
    }
    for (key, value) in map {
        if !canonical.contains(&key.as_str()) {
            pairs.push(format!("{key}:{}", render_value(value)));
        }
    }
    pairs.join(", ")
}

/// `true` when a string renders bare (handles, slugs, tags, ints-as-text) —
/// anything with spaces or punctuation outside `[A-Za-z0-9_-]` gets quoted.
fn is_bare(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn render_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) if is_bare(s) => s.clone(),
        serde_json::Value::String(s) => format!("\"{s}\""),
        serde_json::Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(render_value).collect();
            format!("[{}]", inner.join(" "))
        }
        serde_json::Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}:{}", render_value(v)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn respecialize_directive(state: DirectiveState) -> Directive {
        Directive {
            id: DirectiveId::new(1),
            tier: DirectiveTier::Judgment,
            kind: DirectiveKind::ProposeRespecialize,
            args: json!({"agent": "agent-3", "specialty": "doc-reviewer"}),
            from: AgentId::meta(1),
            state,
        }
    }

    #[test]
    fn tier_and_kind_serialize_per_the_event_schema() {
        assert_eq!(
            serde_json::to_value(DirectiveTier::Judgment).unwrap(),
            json!("Judgment")
        );
        assert_eq!(
            serde_json::to_value(DirectiveTier::Mechanical).unwrap(),
            json!("Mechanical")
        );
        assert_eq!(
            serde_json::to_value(DirectiveKind::ProposeRespecialize).unwrap(),
            json!("propose_respecialize")
        );
        assert_eq!(
            serde_json::from_str::<DirectiveKind>("\"set_parallelism\"").unwrap(),
            DirectiveKind::SetParallelism
        );
        assert_eq!(
            DirectiveKind::SetParallelism.tier(),
            DirectiveTier::Mechanical
        );
        assert_eq!(
            DirectiveKind::ProposeRebalance.tier(),
            DirectiveTier::Judgment
        );
    }

    #[test]
    fn directives_line_matches_the_transcript() {
        let directive = respecialize_directive(DirectiveState::Pending);
        assert_eq!(
            directive.directives_line(),
            "- directive 1 [judgment, pending] propose_respecialize{agent:agent-3, specialty:doc-reviewer} from meta-1"
        );
    }

    #[test]
    fn outcomes_line_covers_all_three_outcomes() {
        assert_eq!(
            respecialize_directive(DirectiveState::Pending).outcomes_line(),
            "- directive 1 [judgment] propose_respecialize{agent:agent-3, specialty:doc-reviewer} — pending"
        );
        assert_eq!(
            respecialize_directive(DirectiveState::Fulfilled {
                by: AgentId::orchestrator()
            })
            .outcomes_line(),
            "- directive 1 [judgment] propose_respecialize{agent:agent-3, specialty:doc-reviewer} — fulfilled by orchestrator"
        );
        assert_eq!(
            respecialize_directive(DirectiveState::Declined {
                by: AgentId::orchestrator(),
                reason: "already specialized".into()
            })
            .outcomes_line(),
            "- directive 1 [judgment] propose_respecialize{agent:agent-3, specialty:doc-reviewer} — declined by orchestrator: \"already specialized\""
        );
    }

    #[test]
    fn args_render_in_canonical_order_with_quoting_and_arrays() {
        // task before reason, reason quoted (pins §3 example).
        let reallocate = Directive {
            id: DirectiveId::new(2),
            tier: DirectiveTier::Judgment,
            kind: DirectiveKind::ProposeReallocate,
            args: json!({"reason": "stuck too long", "task": 2}),
            from: AgentId::meta(1),
            state: DirectiveState::Pending,
        };
        assert_eq!(
            reallocate.outcomes_line(),
            "- directive 2 [judgment] propose_reallocate{task:2, reason:\"stuck too long\"} — pending"
        );

        // team before members, array as [a b] (pins §3 example).
        let rebalance = Directive {
            id: DirectiveId::new(3),
            tier: DirectiveTier::Judgment,
            kind: DirectiveKind::ProposeRebalance,
            args: json!({"members": ["agent-1", "agent-2"], "team": "t1"}),
            from: AgentId::meta(1),
            state: DirectiveState::Pending,
        };
        assert_eq!(
            rebalance.directives_line(),
            "- directive 3 [judgment, pending] propose_rebalance{team:t1, members:[agent-1 agent-2]} from meta-1"
        );

        // Mechanical int arg, bare.
        let parallelism = Directive {
            id: DirectiveId::new(2),
            tier: DirectiveTier::Mechanical,
            kind: DirectiveKind::SetParallelism,
            args: json!({"target": 2}),
            from: AgentId::meta(1),
            state: DirectiveState::Fulfilled {
                by: AgentId::meta(1),
            },
        };
        assert_eq!(
            parallelism.directives_line(),
            "- directive 2 [mechanical, fulfilled] set_parallelism{target:2} from meta-1"
        );
    }
}
