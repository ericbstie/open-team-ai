//! Agent identity: positional handles, the `user`-field grammar, and the
//! auxiliary header channels (ADRs 0008, 0012, 0018).
//!
//! Identity rides in legal wire channels only: the standard `user` field carries
//! the rendered grammar (`orchestrator` / `meta-agent:<id>` /
//! `team-agent:<id>:<slug>`), and the per-agent call-sequence counter plus the
//! run seed ride in `X-OpenTeam-*` HTTP headers that real endpoints ignore.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Header carrying the per-agent monotonic completion counter (ADR 0008/0015).
pub const HEADER_CALL_SEQ: &str = "X-OpenTeam-Call-Seq";
/// Header carrying the run seed (ADR 0008).
pub const HEADER_SEED: &str = "X-OpenTeam-Seed";

/// The run-level seed from which all mock behavior derives.
pub type Seed = u64;

/// A fault parsing a handle, slug, or `user`-field rendering.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    #[error("invalid agent handle {0:?}: expected orchestrator, meta-<n>, or agent-<n> (1-based)")]
    InvalidHandle(String),
    #[error(
        "invalid specialty slug {0:?}: expected non-empty lowercase [a-z0-9-], starting and ending alphanumeric"
    )]
    InvalidSlug(String),
    #[error(
        "invalid user field {0:?}: expected orchestrator, meta-agent:<id>, or team-agent:<id>:<slug>"
    )]
    InvalidUserField(String),
    #[error("role mismatch in user field {0:?}: handle {1:?} does not belong to the stated role")]
    RoleMismatch(String, String),
}

/// The control class an agent belongs to (CONTEXT.md: Role).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Orchestrator,
    MetaAgent,
    TeamAgent,
}

/// The positional agent handle — `orchestrator`, `meta-N`, or `agent-N` — one
/// id space used everywhere an agent is named (ADR 0012). A newtype over the
/// compact, human-legible handle; UUIDs are reserved for `RunId`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentId(String);

impl AgentId {
    /// The single persistent orchestrator handle.
    pub fn orchestrator() -> Self {
        Self("orchestrator".into())
    }

    /// The `n`-th meta-agent handle, 1-based (`meta-1`…`meta-M`).
    pub fn meta(n: usize) -> Self {
        Self(format!("meta-{n}"))
    }

    /// The `n`-th team-agent handle, 1-based (`agent-1`…`agent-N`).
    pub fn team(n: usize) -> Self {
        Self(format!("agent-{n}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The role, derived from the handle prefix — never a redundant field
    /// (ADR 0022).
    pub fn role(&self) -> Role {
        if self.0 == "orchestrator" {
            Role::Orchestrator
        } else if self.0.starts_with("meta-") {
            Role::MetaAgent
        } else {
            Role::TeamAgent
        }
    }

    /// Parse and validate a handle string.
    pub fn parse(handle: &str) -> Result<Self, IdentityError> {
        if handle == "orchestrator" {
            return Ok(Self::orchestrator());
        }
        for (prefix, make) in [
            ("meta-", Self::meta as fn(usize) -> Self),
            ("agent-", Self::team as fn(usize) -> Self),
        ] {
            if let Some(rest) = handle.strip_prefix(prefix) {
                // 1-based, no leading zeros, no sign — the positional mint only.
                if !rest.is_empty()
                    && rest.chars().all(|c| c.is_ascii_digit())
                    && !rest.starts_with('0')
                {
                    let n: usize = rest
                        .parse()
                        .map_err(|_| IdentityError::InvalidHandle(handle.into()))?;
                    return Ok(make(n));
                }
            }
        }
        Err(IdentityError::InvalidHandle(handle.into()))
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for AgentId {
    type Err = IdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for AgentId {
    type Error = IdentityError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<AgentId> for String {
    fn from(id: AgentId) -> Self {
        id.0
    }
}

/// A specialty's slug name (CONTEXT.md: Specialty) — non-empty lowercase
/// `[a-z0-9-]`, starting and ending alphanumeric, so it composes into the
/// colon-delimited `user`-field grammar unambiguously.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SpecialtySlug(String);

impl SpecialtySlug {
    /// The harness-shipped default specialty every team agent boots with.
    pub fn generalist() -> Self {
        Self("generalist".into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse and validate a slug.
    pub fn parse(slug: &str) -> Result<Self, IdentityError> {
        let valid = !slug.is_empty()
            && slug
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !slug.starts_with('-')
            && !slug.ends_with('-');
        if valid {
            Ok(Self(slug.into()))
        } else {
            Err(IdentityError::InvalidSlug(slug.into()))
        }
    }
}

impl fmt::Display for SpecialtySlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SpecialtySlug {
    type Err = IdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for SpecialtySlug {
    type Error = IdentityError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<SpecialtySlug> for String {
    fn from(slug: SpecialtySlug) -> Self {
        slug.0
    }
}

/// The parsed `user`-field grammar (ADR 0012):
/// `orchestrator` / `meta-agent:<agent-id>` / `team-agent:<agent-id>:<specialty-slug>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedUser {
    Orchestrator,
    MetaAgent {
        agent: AgentId,
    },
    TeamAgent {
        agent: AgentId,
        specialty: SpecialtySlug,
    },
}

impl ParsedUser {
    /// Render into the wire `user` field.
    pub fn render(&self) -> String {
        match self {
            Self::Orchestrator => "orchestrator".into(),
            Self::MetaAgent { agent } => format!("meta-agent:{agent}"),
            Self::TeamAgent { agent, specialty } => format!("team-agent:{agent}:{specialty}"),
        }
    }

    /// Parse a wire `user` field.
    pub fn parse(user: &str) -> Result<Self, IdentityError> {
        if user == "orchestrator" {
            return Ok(Self::Orchestrator);
        }
        if let Some(handle) = user.strip_prefix("meta-agent:") {
            let agent =
                AgentId::parse(handle).map_err(|_| IdentityError::InvalidUserField(user.into()))?;
            if agent.role() != Role::MetaAgent {
                return Err(IdentityError::RoleMismatch(user.into(), handle.into()));
            }
            return Ok(Self::MetaAgent { agent });
        }
        if let Some(rest) = user.strip_prefix("team-agent:") {
            let (handle, slug) = rest
                .split_once(':')
                .ok_or_else(|| IdentityError::InvalidUserField(user.into()))?;
            let agent =
                AgentId::parse(handle).map_err(|_| IdentityError::InvalidUserField(user.into()))?;
            if agent.role() != Role::TeamAgent {
                return Err(IdentityError::RoleMismatch(user.into(), handle.into()));
            }
            let specialty = SpecialtySlug::parse(slug)
                .map_err(|_| IdentityError::InvalidUserField(user.into()))?;
            return Ok(Self::TeamAgent { agent, specialty });
        }
        Err(IdentityError::InvalidUserField(user.into()))
    }

    /// The agent handle this identity names.
    pub fn agent(&self) -> AgentId {
        match self {
            Self::Orchestrator => AgentId::orchestrator(),
            Self::MetaAgent { agent } | Self::TeamAgent { agent, .. } => agent.clone(),
        }
    }

    pub fn role(&self) -> Role {
        match self {
            Self::Orchestrator => Role::Orchestrator,
            Self::MetaAgent { .. } => Role::MetaAgent,
            Self::TeamAgent { .. } => Role::TeamAgent,
        }
    }
}

impl fmt::Display for ParsedUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

/// What an `AgentChannel` hands the transport per completion (ADR 0018): the
/// rendered `user` field plus the two auxiliary header values — the whole of
/// what ADR 0008 lets identity ride on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireIdentity {
    /// The rendered ADR 0012 grammar, stamped into the schema-pure body.
    pub user: String,
    /// The per-agent monotonic completion counter (`X-OpenTeam-Call-Seq`).
    pub call_seq: u64,
    /// The run seed (`X-OpenTeam-Seed`).
    pub seed: Seed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_mint_and_render_positionally() {
        assert_eq!(AgentId::orchestrator().as_str(), "orchestrator");
        assert_eq!(AgentId::meta(1).as_str(), "meta-1");
        assert_eq!(AgentId::team(3).as_str(), "agent-3");
        assert_eq!(AgentId::orchestrator().role(), Role::Orchestrator);
        assert_eq!(AgentId::meta(2).role(), Role::MetaAgent);
        assert_eq!(AgentId::team(1).role(), Role::TeamAgent);
    }

    #[test]
    fn handle_parse_round_trips_and_rejects_junk() {
        for handle in ["orchestrator", "meta-1", "agent-12"] {
            assert_eq!(AgentId::parse(handle).unwrap().as_str(), handle);
        }
        for junk in [
            "", "agent", "agent-", "agent-0x", "meta-01", "worker-1", "agent-0",
        ] {
            assert!(AgentId::parse(junk).is_err(), "should reject {junk:?}");
        }
    }

    #[test]
    fn agent_id_serializes_as_bare_handle() {
        let id = AgentId::team(2);
        assert_eq!(serde_json::to_value(&id).unwrap(), "agent-2");
        let back: AgentId = serde_json::from_str("\"meta-1\"").unwrap();
        assert_eq!(back, AgentId::meta(1));
        assert!(serde_json::from_str::<AgentId>("\"nope\"").is_err());
    }

    #[test]
    fn slug_validation() {
        for slug in ["generalist", "doc-reviewer", "a", "x9", "a-b-c1"] {
            assert_eq!(SpecialtySlug::parse(slug).unwrap().as_str(), slug);
        }
        for junk in ["", "Doc", "doc reviewer", "-doc", "doc-", "doc:rev", "café"] {
            assert!(
                SpecialtySlug::parse(junk).is_err(),
                "should reject {junk:?}"
            );
        }
    }

    #[test]
    fn user_grammar_parse_render_round_trips() {
        let cases = [
            "orchestrator",
            "meta-agent:meta-1",
            "team-agent:agent-3:generalist",
            "team-agent:agent-3:doc-reviewer",
        ];
        for user in cases {
            let parsed = ParsedUser::parse(user).unwrap();
            assert_eq!(parsed.render(), user);
        }
    }

    #[test]
    fn user_grammar_extracts_identity() {
        let parsed = ParsedUser::parse("team-agent:agent-3:doc-reviewer").unwrap();
        assert_eq!(parsed.agent(), AgentId::team(3));
        assert_eq!(parsed.role(), Role::TeamAgent);
        match parsed {
            ParsedUser::TeamAgent { specialty, .. } => {
                assert_eq!(specialty.as_str(), "doc-reviewer");
            }
            other => panic!("expected team agent, got {other:?}"),
        }
    }

    #[test]
    fn user_grammar_rejects_malformed_and_mismatched() {
        for junk in [
            "",
            "orchestrator:extra",
            "meta-agent:",
            "meta-agent:agent-1",           // team handle under the meta role
            "team-agent:agent-1",           // missing slug
            "team-agent:meta-1:generalist", // meta handle under the team role
            "team-agent:agent-1:Bad Slug",
            "boss",
        ] {
            assert!(ParsedUser::parse(junk).is_err(), "should reject {junk:?}");
        }
    }

    #[test]
    fn header_names_are_pinned() {
        assert_eq!(HEADER_CALL_SEQ, "X-OpenTeam-Call-Seq");
        assert_eq!(HEADER_SEED, "X-OpenTeam-Seed");
    }
}
