# The event schema is one envelope over a closed kind taxonomy; run artifacts are a streamed log plus finalized snapshots

The append-only event log is the substrate metrics, meta-agents, the report, and
the e2e tests all read (ADRs 0007/0011/0017/0020). This ADR consolidates the event
kinds previewed across the board (#7), messaging/knowledge (#8/#11), runtime
(#12/#15), and meta-layer (#17/#20) tickets into **one envelope over a closed
26-kind taxonomy**, and pins the run-artifact formats. Replay is map Out-of-scope,
but the schema must not preclude it.

## The envelope

```rust
struct Event { id: EventId, at: Timestamp, source: EventSource, kind: EventKind }
enum EventSource { Agent(AgentId), System }   // serialized as a legible string: the handle, or "system"

#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
enum EventKind { /* 26 variants below */ }
```

- **`EventId` (u64) is the single ordering key** — run-scoped monotonic, allocated on
  the one serialized write path, the **same allocator** as `MessageId` and
  `KnowledgeEntryId` (ADR 0011's up-front 3-id allocation, so the mutual reference
  `Message.knowledge_ref` ↔ `KnowledgeEntry.source_event` ↔ the emitting event is
  coherent). Every time-like metric counts in `EventId` deltas (ADR 0020).
- **`at` is informational only.** It comes from the injected `Clock` (frozen in tests,
  ADR 0019), serializes RFC3339 in the log, and is **never** read for ordering or
  determinism — the `EventId` is. This keeps the log golden-stable while carrying a
  wall-clock breadcrumb for the report's human duration line.
- **`source` = the acting agent (the verb caller), else `System`**, serialized as the
  legible agent handle (`orchestrator` / `meta-1` / `agent-2`, ADR 0012) or the literal
  `"system"` — legibility in `events.jsonl` and the report is worth more than a tidier
  `Agent | System` split. The attribution rule is **source is the actor; every
  non-actor subject rides in the payload**: `task_unassigned` source = orchestrator,
  payload `prev_claimant`; a mechanical-directive `agent_slept` source = the emitting
  meta-agent, payload `agent` (the sleeper) + `via_directive`; `agent_parked` source =
  the malformed agent (its own turn triggered the park); `messages_delivered` source =
  the recipient (delivery belongs on the recipient's timeline). This makes #23's
  "per-agent event order" invariant a clean filter on `source`, and is consistent
  across self-sleep (source = the agent), orchestrator-sleep (source = orchestrator),
  and mechanical-directive-sleep (source = the meta-agent).
- **`kind`/`data` is adjacently tagged**, flattened into the envelope, so each line is
  `{"id":42,"at":"2026-…","source":"agent-2","kind":"task_claimed","data":{…}}`. A
  reader dispatches on the stable snake_case `kind` and deserializes `data` per-kind —
  exactly what makes the schema **replay-capable without shipping replay**: the closed
  kind set plus the self-describing `run_started` header are sufficient to reconstruct
  the board, store, and mailboxes, yet no replay feature is built in v1.

## The taxonomy — 26 kinds (25 world/lifecycle events + `context_degraded`)

Payloads carry only non-actor subjects (the actor is `source`). `via_directive:
Option<DirectiveId>` on an effect event records that a meta directive caused it.

**Lifecycle bookends (2).**
- `run_started { run_id, seed, goal, agents, meta_agents, parallel, scenario: Option<String>, caps: { max_ticks?, max_llm_calls?, max_duration_ms? } }` — `EventId` 0, the self-describing config header (`scenario` records whether the built-in arc or a named #20 scenario drove the run, for reproducibility).
- `run_finished { reason: CleanFinish | CapHit(CapKind) | HarnessError, exit_code: u8 }` — exit code 0 / 2 / 1 (ADR 0006).

**Termination (1).**
- `cap_hit { cap: CapKind, limit: u64, observed: u64 }` where `CapKind ∈ { MaxTicks, MaxLlmCalls, MaxDuration }` — precedes the forced `run_finished { CapHit(cap), 2 }`.

**Task (6)** (#7).
- `task_created { task, title, description, team: Option<TeamId> }`
- `task_claimed { task, team: Option<TeamId> }`
- `task_released { task, reason: Option<String> }`
- `task_unassigned { task, prev_claimant: AgentId, reason: Option<String>, via_directive? }`
- `task_completed { task, result: String, result_ref: KnowledgeEntryId }` (== ADR 0020's "task_done"; **`task_completed` is the canonical name**)
- `task_cancelled { task, reason: String }`

**Messaging & knowledge (3)** (#8/#11).
- `message_sent { message: MessageId, address: Direct{to: AgentId} | Team{team: TeamId} | Broadcast, body: String, knowledge_ref: KnowledgeEntryId }` (source = sender)
- `messages_delivered { delivered: Vec<MessageId> }` (source = recipient; one per turn, the MessageIds drained into this turn — the mailbox-pressure/oldest-pending-age fold pairs each id's `message_sent` `EventId` against this event's `EventId`)
- `knowledge_written { entry: KnowledgeEntryId, text: String }` — **Notes only**. A `Message`-kind or `TaskCompletion`-kind entry's `source_event` points at its `message_sent` / `task_completed` event (ADR 0014's wording); the store-size-by-kind metric folds all three event sources, and no send/completion emits a redundant second event.

**Runtime (6)** (#12/#15/#20).
- `turn_completed { first_call_seq: u64, last_call_seq: u64, tool_iters: u32, outcome: Yielded | ToolIterCap, malformed: bool, usage: { prompt, completion, total }, on_task: Option<TaskId> }` — fires for every turn of every agent (source = the agent; role is derived from the handle prefix, no redundant field). See below.
- `agent_slept { agent: AgentId, via_directive? }` — deliberate sleep (orchestrator verb / self-sleep / mechanical directive; the park is separate).
- `agent_parked { agent: AgentId, count: u32 }` — the automatic K=3-malformed park (source = the malformed agent).
- `agent_woke { agent: AgentId, restored: Working{task: TaskId} | Idle, via_directive? }`
- `parallelism_changed { requested: u32, effective: u32, via_directive: DirectiveId }` — the `set_parallelism` effect (clamped to `[1, --parallel]`, ADR 0020); symmetric with sleep/wake.
- `liveness_nudge { board_open: u32, claimed_by_asleep: u32 }` — source = System; the ~500 ms watchdog firing (ADR 0015). Expected count **0** on the happy path; a fired nudge is a scheduling bug surfacing loudly.

**Teams (3)** (#9/#14).
- `team_formed { team: TeamId, members: Vec<AgentId> }`
- `team_members_set { team: TeamId, members: Vec<AgentId>, added: Vec<AgentId>, removed: Vec<AgentId>, via_directive? }` — declarative full-set replace with computed deltas (the join/leave record).
- `team_dissolved { team: TeamId }` — releases both scopes; fails loudly earlier if live team tasks remain (#7), so no task-release payload.

**Specialty (1)** (#9).
- `agent_respecialized { agent: AgentId, from: SpecialtySlug, to: SpecialtySlug, via_directive? }`

**Directives (3)** (#5/#17/#20).
- `directive_issued { directive: DirectiveId, tier: Mechanical | Judgment, kind: DirectiveKind, args }` — fires for **both** tiers.
- `directive_fulfilled { directive: DirectiveId, by: AgentId }` — judgment only.
- `directive_declined { directive: DirectiveId, kind: DirectiveKind, reason: String, by: AgentId }`

**Context (1)** (#13/#16).
- `context_degraded { agent: AgentId, sections: Vec<{ kind, budget, used, dropped_items }> }` — emitted **only** when a section is dropped/truncated under budget pressure. 0 on the happy path (mirrors `liveness_nudge`), so #23 forces a tiny budget and asserts degradation deterministically without asserting rendered text.

## `turn_completed` carries the metrics load; a tick IS an orchestrator `turn_completed`

`turn_completed` is the metrics-load-bearing event, fired once per turn for every agent
(ADR 0020 folds `usage` for token spend). It carries `on_task` — the team agent's
claimed task during the turn — so **per-done-task token spend** folds precisely (Σ turn
`usage` where `on_task == t`) rather than a crude run-total ÷ done-count average. The
`first_call_seq..last_call_seq` span records exactly which completions the turn used
(the ADR 0008/0015 determinism keys), so events correlate to wire requests for #23's
contract tests. `outcome` (`Yielded` vs `ToolIterCap`) is how the inner loop ended;
`malformed` (every emitted call `invalid`, ADR 0017) feeds the consecutive-malformed
metric and the K=3 park fold; `tool_iters == 0` marks a no-op yield for the stall
counter. **A tick is one orchestrator turn** (ADR 0007), so there is **no distinct
`tick` event** — `--max-ticks`, ticks-since-last-`task_completed`, and the stall
counters all fold orchestrator `turn_completed`. Role is derived from `source`'s handle
prefix (`orchestrator` / `meta-*` / `agent-*`), never a redundant field.

## Directives: uniform issuance, mechanical-issued-⟹-applied

`directive_issued` fires for both tiers as the one issuance record the metric folds by
`kind`/`tier`. A **mechanical** `directive_issued` fires **only on successful
application**: a guard-failed mechanical verb (e.g. `sleep_agent` on a non-Idle target)
returns a `rejected` tool-outcome (ADR 0017) *before any directive is issued*, so it
produces no directive event at all — therefore `mechanical fulfilled = count(directive_issued
where tier == Mechanical)` is exactly correct, and each issued mechanical directive's
effect event (`agent_slept` / `agent_woke` / `parallelism_changed`) carries
`via_directive`. A **judgment** directive is `directive_issued` (pending in the
orchestrator's never-dropped Directives section, ADR 0016) → then `directive_fulfilled
{ directive, by }` when the orchestrator acts-with-cite (`in_response_to`, ADR 0020 —
and the heterogeneous action event, e.g. `agent_respecialized`, *also* carries
`via_directive`), or `directive_declined { directive, kind, reason, by }`. Both the
`directive_fulfilled` event and the action event's `via_directive` are kept: the former
is one event-kind to count for the directives metric (clean fold), the latter gives
per-action traceability; deriving "fulfilled" by scanning every action kind for
`via_directive` would be a messier fold, and directives are rare so the extra volume is
negligible. Metric: **issued** = `directive_issued`; **fulfilled** = `directive_fulfilled`
(judgment) + `directive_issued where Mechanical`; **declined** = `directive_declined`.

## Run artifacts

The run directory is **`.openteam/runs/<run-id>/`**, cwd-relative, created at run start
(the override flag is #21's). `RunId` is a **UUIDv7** — a fresh, intentionally
**non-deterministic** per-run instance id (the *seed* gives behavioral determinism; the
run-id is just a unique folder name), whose time-ordering makes `.openteam/runs/` sort
chronologically by `ls`. This refines ADR 0012/#6's "uuid for run ids" to specifically
v7.

- **`events.jsonl`** — the full log, one `Event` per line, **streamed append+flush per
  event** on the serial write path, so it is complete up to the last committed event
  even on a `SIGKILL` or cap-forced kill. This is the crash/cap-durable spine.
- **`board.json`** — the final board snapshot, pretty-printed:
  `{ run_id, goal, seed, tasks: [{ id, title, description, created_by, origin_event,
  team, state }], teams: [{ id, members, dissolved }] }`. A `Done` state inlines both
  the `result` text (so the file is self-readable) and its `result_ref:
  KnowledgeEntryId`. On a cap hit it faithfully shows the leftover Open/Claimed tasks.
- **`knowledge.jsonl`** — one entry per line: `{ id, kind, author, source_event, text }`.
  **Embeddings are omitted** (no flag in v1): D=256 f32 bloats the file and is
  deterministically recomputable from `text` via the seed-independent mock embedder
  (ADR 0014). (Caveat for the untested real-endpoint path: a real embedder's vectors
  would not be recomputable — acceptable, real endpoints are out of v1 scope.)
- **`report.md`** — the orchestrator's `finish_run` report verbatim, then a
  `## Run summary` block that is exactly the `Metrics::summary()` projection (ADR 0020):
  outcome + exit code, wall-clock duration + tick count, agents/meta + specialties used,
  tasks created / completed / cancelled, message volume by address kind, knowledge
  entries + bytes, sleeps / wakes / parks, respecializations, token spend total +
  per-agent, meta interventions issued / fulfilled / declined, liveness-nudge count. On
  a **cap hit**: a stub report body ("terminated: `<cap>` cap before `finish_run`") plus
  the same summary. The rendered `report.md` content is **identical to what the CLI
  prints to stdout** (#21: stdout = the assembled answer + summary, stderr = tracing),
  so the finalize step writes one rendered report to both the file and stdout.

**Cap-hit persistence (ADR 0006).** `events.jsonl` streams, so it is always current; the
finalize step writes `board.json` / `knowledge.jsonl` / `report.md` on **every**
termination path — clean `finish_run` or cap-forced — satisfying "force-terminate with a
stub report and persisted partial artifacts."

## Rejected

- **Timestamp as the ordering/determinism key** — nondeterministic under a real clock and
  redundant with `EventId`; the Clock timestamp stays a human breadcrumb only.
- **A pure `Agent | System` source with no handle string** — loses `events.jsonl` and
  report legibility for no gain; the handle grammar is already unambiguous.
- **A distinct `tick` event and a `turn_started` event** — pure redundancy; a tick is an
  orchestrator `turn_completed`, and `turn_completed` already implies and summarizes its
  turn.
- **`knowledge_written` for all three kinds** (a store-insert event per Message and
  TaskCompletion too) — contradicts ADR 0014's `source_event`-points-at-the-causing-event
  wording and doubles events per send; the fold reads the three causing events instead.
- **Firing a context event every turn** — doubles event volume for a no-op; the
  only-on-degradation `context_degraded` is the observability hook #13 wanted, at zero
  happy-path cost.
- **Mechanical directives emitting no `directive_issued`** (metric counts them via effect
  events) — scatters the "issued" count across three heterogeneous effect kinds and makes
  the fold read `source ∈ meta`; uniform `directive_issued` for both tiers is cleaner, and
  mechanical-issued-⟹-applied keeps the fulfilled count exact.
- **Embeddings in `knowledge.jsonl`** — bulky and deterministically recomputable from text
  via the mock embedder; omitting them keeps the artifact legible (a flag is deferred until
  the untested real-endpoint path demands it).
- **A seed-derived or v4 `RunId`** — the seed already carries determinism, and v4's random
  ordering makes `.openteam/runs/` sort meaninglessly; UUIDv7 sorts chronologically.
- **A finalize-only (non-streamed) `events.jsonl`** — a `SIGKILL` or cap kill would lose
  the whole log; streaming append+flush makes the log the crash-durable spine.

## Amended by the #22 dry-run gate (2026-07-17)

"`EventId` … the same allocator as `MessageId` and `KnowledgeEntryId`" means **the same
serial write path**, not a shared counter: `EventId`, `MessageId`, `KnowledgeEntryId`, and
`TaskId` (ADR 0023) are **four independent monotonic counters**, each contiguous —
`EventId` 0-based (`run_started` = 0), the others 1-based. Contiguous `EventId`s are what
make replay and #23's "the Nth event is X" assertions tractable. See ADR 0011's amendment.
The dry-run transcript (docs/prototypes/dry-run-transcript.md) exercises the full envelope,
`board.json`, `knowledge.jsonl`, and `report.md`/stdout against this schema.
