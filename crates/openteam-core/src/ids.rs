//! Run-scoped identifier newtypes (ADR 0011, amended by the #22 dry-run gate).
//!
//! `EventId`, `TaskId`, `MessageId`, `KnowledgeEntryId`, and `DirectiveId` are
//! **five independent monotonic counters**, each contiguous, all advanced on the
//! run's single serial write path — `EventId` is 0-based (`run_started` = event
//! 0), the others 1-based. Allocation is a RUNTIME property of that write path;
//! this module holds only the types.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A run's unique instance id — a UUIDv7 minted at run start (ADR 0022).
///
/// Intentionally **non-deterministic**: the seed gives behavioral determinism,
/// the run id is just a unique, chronologically-sortable folder name for
/// `.openteam/runs/<run-id>/`.
pub type RunId = uuid::Uuid;

macro_rules! id_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            Serialize,
            Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl From<u64> for $name {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }

        impl From<$name> for u64 {
            fn from(id: $name) -> u64 {
                id.0
            }
        }
    };
}

id_newtype!(
    /// The run-scoped monotonic id ordering the event log — the single
    /// ordering key; the timestamp is informational only (ADR 0022).
    /// **0-based**: `run_started` is event 0. Every time-like metric counts in
    /// `EventId` deltas, never wall-clock (ADR 0020).
    EventId
);

id_newtype!(
    /// A task's id on the task board (ADR 0010). 1-based, contiguous per run.
    TaskId
);

id_newtype!(
    /// A message's id — the run-wide total order at accept (ADR 0011).
    /// 1-based, contiguous per run.
    MessageId
);

id_newtype!(
    /// A knowledge entry's id in the run-scoped knowledge store (ADR 0014).
    /// 1-based, contiguous per run.
    KnowledgeEntryId
);

id_newtype!(
    /// A directive's id (ADR 0005/0020) — its own 1-based counter, parallel to
    /// the other four (dry-run transcript §9, minor notes).
    DirectiveId
);

/// A fault validating a [`TeamId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TeamIdError {
    #[error("invalid team id {0:?}: must be non-empty with no whitespace")]
    Invalid(String),
}

/// A team's tag — a short, orchestrator-authored, non-empty identifier
/// (e.g. `"t1"`) naming a runtime team entity (ADR 0009).
///
/// Validated non-empty and whitespace-free so it composes into the pinned
/// board-digest line grammar `team:<tag|->` unambiguously (ADR 0016).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TeamId(String);

impl TeamId {
    /// Parse and validate a team tag.
    pub fn parse(tag: &str) -> Result<Self, TeamIdError> {
        if tag.is_empty() || tag.chars().any(char::is_whitespace) {
            Err(TeamIdError::Invalid(tag.into()))
        } else {
            Ok(Self(tag.into()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TeamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for TeamId {
    type Err = TeamIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for TeamId {
    type Error = TeamIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<TeamId> for String {
    fn from(id: TeamId) -> Self {
        id.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u64_ids_are_transparent_ordered_and_displayable() {
        let a = TaskId::new(1);
        let b = TaskId::new(2);
        assert!(a < b);
        assert_eq!(a.get(), 1);
        assert_eq!(a.to_string(), "1");
        assert_eq!(serde_json::to_value(a).unwrap(), serde_json::json!(1));
        let back: TaskId = serde_json::from_str("7").unwrap();
        assert_eq!(back, TaskId::new(7));
        assert_eq!(u64::from(EventId::new(0)), 0);
        assert_eq!(MessageId::from(3).get(), 3);
        assert_eq!(KnowledgeEntryId::new(6).to_string(), "6");
        assert_eq!(DirectiveId::new(1).get(), 1);
    }

    #[test]
    fn team_id_validates_nonempty_whitespace_free() {
        assert_eq!(TeamId::parse("t1").unwrap().as_str(), "t1");
        assert_eq!(TeamId::parse("docs-team").unwrap().to_string(), "docs-team");
        for junk in ["", "t 1", " t1", "t1\n"] {
            assert!(TeamId::parse(junk).is_err(), "should reject {junk:?}");
        }
        assert_eq!(
            serde_json::to_value(TeamId::parse("t1").unwrap()).unwrap(),
            serde_json::json!("t1")
        );
        assert!(serde_json::from_str::<TeamId>("\"\"").is_err());
    }
}
