# Implementation pins — code-level details the ADRs leave open

**Subordinate to the ADRs and the dry-run transcript** — this file re-decides
nothing; it only pins code-level details the spec leaves open, so that
`openteam-core` (the renderer) and `openteam-mock` (the parser) are written in
lockstep by construction. Where anything here seems to conflict with an ADR,
**the ADR wins**.

## 1. Verb argument shapes (JSON, all structs `deny_unknown_fields`)

Team (7): `claim_task {task: int}` · `complete_task {result: string}` ·
`release_task {reason?: string}` · `post_message` (below) ·
`write_knowledge {text: string}` · `search_knowledge {query: string, k?: int}`
(default k = 3) · `sleep {}`.

Orchestrator (14): `create_task {title, description, team?: string|null}` ·
`cancel_task {task: int, reason: string}` ·
`unassign_task {task: int, reason?: string|null, in_response_to?: int|null}` ·
`form_team {team: string, members: [string]}` · `dissolve_team {team: string}` ·
`set_team_members {team: string, members: [string], in_response_to?: int|null}` ·
`respecialize {agent: string, specialty: {name, description, focus}, in_response_to?: int|null}` ·
`sleep_agent {agent: string}` · `wake_agent {agent: string}` ·
`decline_directive {directive: int, reason: string}` · `post_message` ·
`write_knowledge` · `search_knowledge` · `finish_run {report: string}`.

Meta (6): `set_parallelism {target: int}` · `sleep_agent {agent}` ·
`wake_agent {agent}` · `propose_respecialize {agent: string, specialty: string}` ·
`propose_reallocate {task: int, reason: string}` ·
`propose_rebalance {team: string, members: [string]}`.

`post_message { to?: string, team?: string, broadcast?: bool, body: string }` —
exactly one of `to` / `team` / `broadcast:true` must be set; anything else is a
**`rejected {code: "invalid_address"}`** (well-formed call, domain refusal —
never `invalid`).

Selected `ok` result payloads (the arc reads only `status`, but these two are
shown in the transcript and are pinned): `propose_*` →
`{"directive_id": N}`; `set_parallelism` → `{"applied": true, "effective": N}`.
Other `ok` payloads are small verb-specific objects (e.g. `claim_task` →
`{"task": N}`, `create_task` → `{"task": N}`, `post_message` →
`{"message": N}`, `write_knowledge` → `{"entry": N}`, `search_knowledge` →
`{"hits": [{entry, score, kind, author, text}]}`).

Tool-outcome envelope (ADR 0017, serialized as the `role:"tool"` content):
`{"status":"ok","result":{…}}` ·
`{"status":"rejected","code":"…","message":"…","details":{…}?}` ·
`{"status":"invalid","code":"unknown_verb"|"invalid_arguments","message":"…"}`.

## 2. Section headers, per-role order, and placeholders

- Orchestrator `user` message: `## Goal`, `## Board digest`,
  `## Knowledge retrievals`, `## Fresh messages`, `## Directives`.
- Team agent: `## Goal`, `## Board digest`, `## Claimed task`,
  `## Recent activity`, `## Knowledge retrievals`, `## Fresh messages`.
- Meta agent: `## Goal`, `## Metrics digest`, `## Directive outcomes`,
  `## Recent events`.

Sections are separated by one blank line. Empty-section placeholders (exact):
board digest `(empty)` (the orchestrator's still ends with its `run-health:`
line); `## Claimed task`, `## Recent activity`, `## Knowledge retrievals`,
`## Fresh messages`, `## Directives` → `(none)`; `## Directive outcomes` →
`(none issued)`.

## 3. Line grammars (restating ADR 0016 only where a detail was open)

- Board digest task line: `- task <id> [<state>] team:<tag|->  "<title>"` —
  **two spaces** before the quoted title; `<state>` ∈ `Open` /
  `Claimed by <agent>` / `Done` / `Cancelled`.
- run-health line:
  `run-health: done <d>/<n> · agents <w>W/<i>I/<s>S · mailbox depth <cur> (max <max>) · ticks-since-done <t>`.
- Claimed task: `task <id> — "<title>" (team <t>)`; untagged tasks render
  `(team -)`.
- Recent activity: `- [turn <n>] <verb>{<gist>} -> <ok|rejected|invalid>`;
  `<n>` is the agent's 1-based per-run turn index; the gist is compact and
  unparsed (the mock reads only the verb name before `{` and the outcome after
  `-> `).
- Fresh messages: `- msg <id> from <sender> (<direct|team:<t>|broadcast>): "<body>"`.
- Directives: `- directive <id> [<tier>, <state>] <kind>{<args>} from <meta>`;
  tier/state lowercase (`judgment, pending`). Args render as `key:value` pairs,
  comma-space separated, bare (unquoted) values for handles/slugs/ints:
  `propose_respecialize{agent:agent-3, specialty:doc-reviewer}` ·
  `propose_reallocate{task:2, reason:"…"}` ·
  `propose_rebalance{team:t1, members:[agent-1 agent-2]}`.
- Directive outcomes:
  `- directive <id> [<tier>] <kind>{<args>} — <pending|fulfilled by <h>|declined by <h>: "<reason>">`.
- Knowledge retrievals: `- entry <id> (<kind> by <author>, cos <score>): "<text>"`,
  score with two decimals.
- Recent events (meta, not parsed by the mock): `- event <id> <kind> (<source>)`,
  oldest first.

### Metrics digest (exact line shapes; the mock parses only `utilization`)

```
throughput: <done> task_completed / <events> EventIds · latency: work median <m> EventIds
utilization:
  - <agent>: Idle, <specialty> (idle <k>)
  - <agent>: Working (task <t>), <specialty>
  - <agent>: Asleep, <specialty>
mailbox: depth <d>, max <m>, oldest-pending-age <a>
tokens: run <t> · faults: parks <p>, malformed[<agent>:<c> …] · directives: issued <i>/ful <f>/dec <d>
```

`latency: work n/a` when no task has completed. Token counts render as `X.Yk`
at ≥1000, bare integer below. Idle streak `(idle <k>)` in EventId deltas.

### Degradation marker (closes the "window is degraded" legibility gap)

When an oldest-first section (`## Recent activity`, `## Fresh messages`) drops
or withholds items under budget pressure, its **first content line** is
`(degraded: <n> dropped)`, followed by the surviving lines. The mock treats a
`## Recent activity` section whose first line starts with `(degraded` as the
degraded window (ADR 0021's completion shortcut). Retrievals/board-digest
degradation carries no marker (the mock doesn't need it).

## 4. Request framing (core → mock)

- `tool_choice: "auto"` on every request; `parallel_tool_calls: true` for the
  orchestrator and team agents, `false` for meta-agents (per the transcript).
- The built-in arc emits **at most one tool-bearing completion per turn**
  (possibly several parallel calls — a decompose batch, or an action plus a
  seeded steer), then yields on the following completion; `tool_iters` counts
  completions that carried ≥1 tool call, so `tool_iters: 0` marks a no-op
  yield.
- Mock chat `id`: `chatcmpl-<seed>-<handle>-<call_seq>` where `<handle>` is the
  agent handle parsed from `user` (the raw `user` string if unparseable,
  `anon` if absent).
- Default model ids (ADR 0026): the default real path uses `gpt-4o-mini` (chat)
  and `text-embedding-3-small` (embeddings), overridable via
  `--model`/`--embedding-model`; under `--mock` both default to `openteam-mock`
  (the mock echoes whatever non-empty model it receives).
- Default real base URL: `https://api.openai.com/v1` (used when neither `--mock`
  nor `--llm-base-url` is given).

## 5. Internal constants (fixed, not flags)

- K = 3 consecutive-malformed park (pinned by ADR 0015).
- `MAX_TOOL_ITERS` default 8 (`--max-tool-iters`, ADR 0024).
- Meta coalesced-cadence threshold: **6** unobserved events not sourced by the
  observing meta-agent itself (low enough that the flagship demo reliably shows
  both directive tiers before the run converges).
- Repeated-release priority-wake threshold: a task's **3rd** release.
- Liveness watchdog period: **500 ms**.
- Auto-retrieval (context assembly): cosine **top-3**; the query text is the
  goal plus (for a Working team agent) its claimed task's title; skipped while
  the store is empty.
- Zero-tool-call turns neither increment nor reset the consecutive-malformed
  counter (only `ok`/`rejected` reset it; only all-`invalid` turns increment).

### Scheduler edge-trigger reading (core-only)

- The orchestrator tick predicate reads ADR 0007's "pending input" as: events
  exist beyond the watermark taken at the orchestrator's last turn end that
  were not sourced by the orchestrator itself, or a pending directive/mailbox
  item. This is what fires the all-terminal `finish_run` tick (a completed task
  is pending input); extra ticks that merely yield are bounded because ticks
  are edge-triggered on the watermark and world events are bounded by the caps.
  The very first tick fires unconditionally (the goal is pending input).
- Team-agent idle dispatch uses the same per-agent watermark: an Idle agent is
  dispatched when eligible Open work or queued mail exists AND events newer
  than its last turn end exist.
- A team agent's recent-activity window clears on a successful `claim_task`
  (the assignment boundary), then records that claim line; it is wiped on
  respecialization (ADR 0016).
- A successful `finish_run` short-circuits the inner loop: the turn ends
  without a further yield completion (`turn_completed` precedes
  `run_finished`, per transcript events 32–33).

### Deterministic dispatch under `--mock` (`RunConfig.serial_dispatch`)

ADR 0025 placed byte-identical run logs *Out-of-scope* because the default
multi-thread runtime lets concurrently-spawned turns win the single write-path
lock (ADR 0011) in a wall-clock/thread-scheduling-dependent order — even though
every mock response is already a pure function of its request (ADR 0021). That
order feeds decisions (EventId assignment, the meta cadence counters, the
edge-trigger watermarks), so the same `--seed` produced materially different
runs. ADR 0026 then made `--mock` an explicit opt-in whose *whole point* is
deterministic offline runs, so we now pin reproducibility for that path:

- `RunConfig.serial_dispatch` (set by the bin iff `--mock`) makes the reactor
  drive each planned batch **to completion one turn in flight at a time, in the
  fixed plan order** — orchestrator, then meta-agents and team agents in
  `BTreeMap` (handle) order. With never two turns racing for the lock, the
  write-path commit order — and therefore the whole run — is a pure function of
  seed + goal. `openteam run <goal> --mock --seed N` is **byte-identical across
  invocations**, report.md and events.jsonl alike, save the wall-clock
  `Duration:` line and the `uuidv7` run id.
- `--parallel` still shapes the run under `serial_dispatch`: it caps how many
  team agents one planning pass admits (the `Semaphore`, ADR 0015), so work
  still spreads across the pool — only the *completion overlap* is traded away.
  The watchdog/duration tasks are unaffected (they never fire on the happy
  path, ADR 0007/0015), so they cannot perturb the deterministic order.
- Off (`serial_dispatch = false`, the real path and the direct-`run` unit
  tests) the batch is spawned concurrently exactly as before — ADR 0025's
  multi-thread posture and its invariant-only e2e discipline are unchanged (the
  seeded e2e suite passes `--mock` per ADR 0026, so it now runs deterministically
  yet still asserts only interleaving-invariant facts).

## 6. Assembly budgets (defaults; test knob)

Default per-section budgets (tokens, `CharCountTokenizer`): Goal 200, Board
digest 800, Claimed task 100, Recent activity 400, Knowledge retrievals 600,
Fresh messages 800, Directives 400, Metrics digest 800, Directive outcomes 400,
Recent events 400. Generous enough that no happy-path run degrades.
`RunConfig` carries an optional global assembly-budget override (scales/caps
section budgets) which the bin wires from the **test-only** env var
`OPENTEAM_ASSEMBLY_BUDGET` (an integer token pool; not a CLI flag — ADR 0024's
surface is closed) so the context-collapse fixture can force deterministic
degradation.

## 7. Event/artifact details beyond ADR 0022's text

- Serde representations follow the transcript exactly: externally-tagged
  payload enums (`"address":{"Direct":{"to":"agent-1"}}` / `"Team"` /
  `"Broadcast"`; board state `"Open"` / `{"Claimed":{"by":…}}` /
  `{"Done":{"result":…,"result_ref":…}}` / `{"Cancelled":{"reason":…}}`;
  `agent_woke.restored` `{"Working":{"task":1}}` / `"Idle"`;
  `run_finished.reason` `"CleanFinish"` / `{"CapHit":"MaxTicks"}` /
  `"HarnessError"`; tier `"Judgment"` / `"Mechanical"`; turn outcome
  `"Yielded"` / `"ToolIterCap"`).
- `turn_completed.usage` keys: `{"prompt","completion","total"}` (sums over the
  turn's completions).
- `run_started.caps` object holds only the caps that were set
  (`max_ticks` / `max_llm_calls` / `max_duration_ms`); `{}` when none.
- `run_started.scenario`: the `--scenario` path string as given, else `null`.
- `context_degraded.sections[].kind`: the section kind in snake_case
  (`knowledge_retrievals`, `fresh_messages`, `recent_activity`, `board_digest`).
- `DirectiveId` is its own 1-based counter (transcript §9, minor notes).
- `run_finished` source: the orchestrator on a clean finish, `system` on a cap
  hit or harness error.
- `on_task` on a claiming turn = the task claimed by turn end.

## 8. Skeletons (inert to the mock; shape only)

One-paragraph role skeletons in the spirit of the transcript's samples; the
team-agent skeleton interpolates its specialty as
`Specialty: <slug> — <description> Focus: <focus>`. Nothing behavioral reads
them.
