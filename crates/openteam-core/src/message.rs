//! Store-first, channel-free messaging: addresses, messages, and mailboxes
//! (ADR 0011).
//!
//! A message send is accepted on the run's single serialized write path —
//! ingest into the knowledge store, append the `message_sent` event, push onto
//! each recipient's mailbox. No channel carries message data; the store is the
//! source of truth, so losslessness is structural. Delivery is between-turn
//! injection: context assembly drains a mailbox oldest-first under a token
//! budget with carryover — never dropped, never summarized away.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use openteam_wire::AgentId;
use serde::{Deserialize, Serialize};

use crate::ids::{KnowledgeEntryId, MessageId, TeamId};

/// The routing scope of a message (CONTEXT.md: Address): one agent (direct),
/// a team's members at acceptance time, or broadcast to the orchestrator and
/// all team agents — meta-agents observe traffic through events rather than
/// receiving broadcasts (ADR 0011).
///
/// Serializes in serde's default externally-tagged form, matching the pinned
/// `message_sent` payload (ADR 0022, pins §7): `{"Direct":{"to":"agent-1"}}` /
/// `{"Team":{"team":"t1"}}` / `"Broadcast"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Address {
    Direct { to: AgentId },
    Team { team: TeamId },
    Broadcast,
}

impl Address {
    /// The scope label of the pinned fresh-messages line grammar (ADR 0016):
    /// `direct` / `team:<t>` / `broadcast`.
    pub fn scope_label(&self) -> String {
        match self {
            Self::Direct { .. } => "direct".into(),
            Self::Team { team } => format!("team:{team}"),
            Self::Broadcast => "broadcast".into(),
        }
    }
}

impl fmt::Display for Address {
    /// Renders the fresh-messages scope label (see [`Address::scope_label`]).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.scope_label())
    }
}

/// A realtime communication between agents — always ingested into the
/// knowledge store at acceptance (`knowledge_ref` is the resulting entry),
/// delivered between turns via the recipients' mailboxes, never dropped
/// (ADR 0011/0014).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub sender: AgentId,
    pub address: Address,
    pub body: String,
    /// The knowledge entry the body was ingested as (`Message` kind); mutual
    /// with that entry's `source_event` (ADR 0014).
    pub knowledge_ref: KnowledgeEntryId,
}

impl Message {
    /// The pinned fresh-messages line (ADR 0016):
    /// `- msg <id> from <sender> (<direct|team:<t>|broadcast>): "<body>"`.
    pub fn fresh_line(&self) -> String {
        format!(
            "- msg {} from {} ({}): \"{}\"",
            self.id,
            self.sender,
            self.address.scope_label(),
            self.body
        )
    }
}

/// The per-agent ordered queues of accepted-but-undelivered messages
/// (CONTEXT.md: Mailbox) — plain `VecDeque<MessageId>`s, unbounded and
/// lossless (ADR 0011). Message data lives in the knowledge store; a mailbox
/// holds only ids.
///
/// Draining is budgeted and oldest-first with carryover: the caller (context
/// assembly) decides how many to take, and everything left carries over to
/// the next turn — never dropped.
#[derive(Debug, Clone, Default)]
pub struct Mailboxes {
    queues: BTreeMap<AgentId, VecDeque<MessageId>>,
}

impl Mailboxes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push an accepted message onto each recipient's queue, in the given
    /// recipient order. The caller resolves the address to recipients at
    /// acceptance time (team membership is read then, ADR 0011).
    pub fn push_for_recipients<I>(&mut self, recipients: I, message: MessageId)
    where
        I: IntoIterator<Item = AgentId>,
    {
        for recipient in recipients {
            self.queues.entry(recipient).or_default().push_back(message);
        }
    }

    /// The pending ids for `agent`, oldest first, without removing them.
    pub fn peek<'a>(&'a self, agent: &AgentId) -> impl Iterator<Item = MessageId> + 'a {
        self.queues.get(agent).into_iter().flatten().copied()
    }

    /// Remove and return up to `take` oldest pending ids for `agent`; the
    /// rest carry over (ADR 0011's budgeted oldest-first drain).
    pub fn drain(&mut self, agent: &AgentId, take: usize) -> Vec<MessageId> {
        let Some(queue) = self.queues.get_mut(agent) else {
            return Vec::new();
        };
        let n = take.min(queue.len());
        let drained = queue.drain(..n).collect();
        if queue.is_empty() {
            self.queues.remove(agent);
        }
        drained
    }

    /// The oldest pending id for `agent`, if any.
    pub fn oldest(&self, agent: &AgentId) -> Option<MessageId> {
        self.peek(agent).next()
    }

    /// Current queue depth for `agent`.
    pub fn depth(&self, agent: &AgentId) -> usize {
        self.queues.get(agent).map_or(0, VecDeque::len)
    }

    /// Total pending ids across all agents.
    pub fn total_depth(&self) -> usize {
        self.queues.values().map(VecDeque::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.queues.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(n: u64) -> MessageId {
        MessageId::new(n)
    }

    #[test]
    fn address_scope_labels_match_the_fresh_messages_grammar() {
        assert_eq!(
            Address::Direct {
                to: AgentId::team(1)
            }
            .scope_label(),
            "direct"
        );
        assert_eq!(
            Address::Team {
                team: TeamId::parse("t1").unwrap()
            }
            .scope_label(),
            "team:t1"
        );
        assert_eq!(Address::Broadcast.scope_label(), "broadcast");
        assert_eq!(Address::Broadcast.to_string(), "broadcast");
    }

    #[test]
    fn message_fresh_line_matches_the_pinned_grammar() {
        let message = Message {
            id: msg(1),
            sender: AgentId::orchestrator(),
            address: Address::Direct {
                to: AgentId::team(1),
            },
            body: "Prioritize the setup section; the guide leads with it.".into(),
            knowledge_ref: KnowledgeEntryId::new(1),
        };
        assert_eq!(
            message.fresh_line(),
            "- msg 1 from orchestrator (direct): \"Prioritize the setup section; the guide leads with it.\""
        );
    }

    #[test]
    fn mailboxes_drain_oldest_first_with_carryover_and_stay_lossless() {
        let a1 = AgentId::team(1);
        let a2 = AgentId::team(2);
        let mut boxes = Mailboxes::new();
        boxes.push_for_recipients([a1.clone(), a2.clone()], msg(1));
        boxes.push_for_recipients([a1.clone()], msg(2));
        boxes.push_for_recipients([a1.clone()], msg(3));

        assert_eq!(boxes.depth(&a1), 3);
        assert_eq!(boxes.total_depth(), 4);
        assert_eq!(boxes.oldest(&a1), Some(msg(1)));

        // Budgeted drain: take 2, one carries over.
        assert_eq!(boxes.drain(&a1, 2), vec![msg(1), msg(2)]);
        assert_eq!(boxes.depth(&a1), 1);
        assert_eq!(boxes.peek(&a1).collect::<Vec<_>>(), vec![msg(3)]);

        // Over-budget take drains what's there.
        assert_eq!(boxes.drain(&a1, 10), vec![msg(3)]);
        assert_eq!(boxes.depth(&a1), 0);

        // a2's queue was untouched — lossless.
        assert_eq!(boxes.peek(&a2).collect::<Vec<_>>(), vec![msg(1)]);
        assert!(!boxes.is_empty());
        assert_eq!(boxes.drain(&a2, 1), vec![msg(1)]);
        assert!(boxes.is_empty());
        assert!(boxes.drain(&a2, 1).is_empty());
    }
}
