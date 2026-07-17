# Agent identity is a positional handle, not a UUID

`AgentId` is a newtype over a compact, human-legible handle minted from pool
position at run start — `orchestrator`, `meta-1`…`meta-M`, `agent-1`…`agent-N` —
one id space used everywhere an agent is named: events, messages, board claims,
the report, and the OpenAI `user` field. The wire grammar (extending ADR 0008's
identity channel) is `orchestrator` / `meta-agent:<agent-id>` /
`team-agent:<agent-id>:<specialty-slug>` — which is also how the mock reads a
team agent's specialty and keys plausible behavior on it without ever parsing
prompt content. The fixed, persistent pool (ADR 0003) is what makes positional
handles deterministic and stable for the run; UUIDs are reserved for `RunId`.
Rejected: UUID agent ids (illegible in logs, events, and the report, and an
indirection with no payoff when agents are never created or destroyed mid-run);
dual id spaces (internal UUID plus display handle) — two names for one thing
invites drift.
