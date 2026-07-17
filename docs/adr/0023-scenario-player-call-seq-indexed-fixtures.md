# The scenario player: a second `BehaviorModel` adapter driven by a call-seq-indexed JSON fixture

The **scenario player** is the second adapter behind ADR 0019's synchronous
`chat(req, id) -> ChatDecision` seam (the built-in arc, ADR 0021, being the first).
It is selected when `AppState`'s optional scenario is present, and it exists to make
the prior-art failure modes (#5 research §10) **reachable as tests** — the very
pathologies the built-in arc *structurally cannot* produce, because the arc is
bounded, terminating, and schema-valid by construction (ADR 0021): runaway
delegation, chat-loop livelock, stalls without progress, message floods, context
collapse, malformed-output → K=3 park, non-terminating cap hits, and the
liveness/deadlock path. **Scenarios are the test instrument; the built-in arc is the
default UX and the natural fallthrough for any turn a scenario doesn't script.**

## Stateless keying — the call sequence *is* the cursor

The scenario player is a **pure function of `(request, identity)` with zero
run-state**, the same statelessness contract the mock imposes on every behavior model
(ADR 0019) — so concurrent runs against one standalone `openteam mock serve
--scenario …` stay isolated by seed for free, with nothing mutable shared. It keys a
scripted response on the identity channels only:

- the **agent selector** — the handle/role parsed from the `user` field (ADR 0012
  grammar `orchestrator` / `meta-agent:<id>` / `team-agent:<id>:<slug>`); and
- the **0-based per-agent call sequence** from the `X-OpenTeam-Call-Seq` header
  (ADR 0008/0018, an `AtomicU64` that starts at 0, increments per completion, and
  never resets on respecialization).

The load-bearing move: **`call_seq` doubles as the list cursor**, so a per-agent
ordered list of responses is indexed *directly* by `call_seq` with **no per-agent
"next response" state**. A stateful response queue was rejected precisely because it
would reintroduce the run-state ADR 0019 forbids; reusing the wire's own monotonic
counter as the index is what keeps the player stateless. Keying is
**seed-independent** — scripted steps match regardless of the seed header; only
fallthrough (arc) turns consume the seed for variety. Identity is never keyed on
rendered content (ADR 0008/0021); reading the request to *choose the divergence* is a
future enhancement (see Rejected), not v1.

## The format — JSON, zero new dependency

A scenario is a JSON file (`serde_json` is already present via `openteam-wire` /
schemars — no `toml` dep; and scripted tool-call `arguments` are JSON objects that
inline verbatim). Every struct is `#[serde(deny_unknown_fields)]` so a typo in a
fixture is a fail-fast load error.

```rust
struct Scenario {
    version: u32,                 // schema version; v1 == 1 — ADR 0021's marker home
    description: Option<String>,  // doc only
    reproduces: Option<String>,   // doc only: the pathology / ADR the fixture exercises
    scripts: Vec<Script>,
}
struct Script {
    agent: Selector,              // "orchestrator" | "meta-1" | "agent-2" | "agent-*" | "meta-*"
    responses: Vec<Response>,     // responses[call_seq]  (0-based)
    repeat:    Vec<Response>,     // call_seq >= responses.len(): repeat[(seq - len) % repeat.len()]; empty ⇒ fallthrough
}
enum Response {                   // serde-untagged
    Yield,                        // the literal string "yield"       — clean no-tool-call stop
    Fallthrough,                  // the literal string "fallthrough"  — delegate THIS completion to the arc
    Say { text: Option<String>, tool_calls: Vec<Call>, finish: Option<FinishReason> },
}
struct Call { name: String, arguments: serde_json::Value }
```

**Lookup** for a request with parsed handle `h`, role `r`, and `call_seq` `s`: choose
the most specific matching `Script` (**exact handle beats role wildcard**); index its
`responses` by `s`; past the list, cycle `repeat`; if `repeat` is empty (or no script
matches at all) fall through to the built-in arc. **`finish` defaults** to
`tool_calls` when `tool_calls` is non-empty, else `stop`. Multiple `tool_calls`
entries are one parallel-call completion. The server still owns the envelope
(ADR 0019): a scenario decides *what to say*, never the `id`/`created`/`usage`
framing, so a scenario **cannot** emit an invalid *response* even while it emits an
invalid *call*.

`repeat` as a **cycling list** (not a single response) is what makes unbounded
pathologies expressible statelessly: a stalling agent is
`responses:[{claim},"yield"], repeat:[{work},"yield"]` — claim once, then loop
work-without-complete forever, each turn a clean `[work, yield]` pair rather than an
iteration-cap turn.

## Validation is structural, never semantic

The loader validates the scenario file's **own** shape (parseable JSON, known
`version`, well-formed steps, valid `finish` enum) and aborts the run with a nonzero
exit on any violation — but it **deliberately does not** check a scripted `Call`'s
`name` against the tool registry or its `arguments` against the verb's JSON Schema.
Emitting an unknown verb name or args that fail `deny_unknown_fields` is exactly how a
scenario drives the `invalid` tool-outcome and the K=3 malformed-park path (ADR 0017);
a semantic validator would forbid the one thing scenarios exist to do. So there is no
"emit invalid" marker — a bad call *is* the invalid call.

## Loading and scope

`--scenario <file>` (on both `openteam run` and `openteam mock serve`, flag names
#21's) parses + structurally validates the file, builds a `ScenarioPlayer` that owns
the scripts plus an `Arc` to the built-in arc for fallthrough, and sets
`AppState.scenario` **before** `build_router()` (ADR 0019's shared router). Scenarios
are **chat-only**: `/v1/embeddings` bypasses the behavior seam entirely (ADR 0014/0019)
and is never scenario-overridable.

## Predictable `TaskId` keeps claim/complete fixtures self-contained

So a fixture can script `claim_task` / `complete_task` against a known target,
**`TaskId` is a bare monotonic integer from 1 on its own per-run counter** —
incremented only on `task_created`, and **not** the shared
`EventId`/`MessageId`/`KnowledgeEntryId` allocator (ADR 0011/0022). Because task
authorship is orchestrator-only (ADR 0010), the *N*th `create_task` the orchestrator
issues deterministically yields task *N*; a fixture that scripts the orchestrator's
`create_task` calls therefore controls the id space completely and can hardcode
`claim_task{task: N}`, staying fully self-contained rather than depending on
arc-fallthrough to claim. This predictable rendering also serves #22's transcript and
#23's e2e assertions.

## The v1 fixture library — ten fixtures, one pathology and one #23 assertion each

The **happy path is not a fixture**: the built-in arc at a pinned seed *is* the
canonical clean run (ADR 0021), and #23 tests it directly. The library is pathologies
only — each fixture makes one failure mode reachable and backs one #23 assertion.

| Fixture | Pathology (prior-art §10) | #23 assertion it backs |
|---|---|---|
| `stall` | stall without progress (Magentic-One) | ticks-since-`task_completed` counter grows; the claimed task never reaches `Done` |
| `livelock` | chat-loop livelock (CAMEL) | pair-churn stall counter; two agents exchange messages with no `task_completed` |
| `message-flood` | message flood / information overload (MetaGPT) | mailbox depth / max / oldest-pending-age; message volume by address kind |
| `context-collapse` | context collapse / token exhaustion (ChatDev, Anthropic) | `context_degraded` on the knowledge-retrieval + fresh-messages sections under a tiny budget |
| `malformed-k3` | malformed output → park (ADR 0017/0015) | consecutive-malformed counter; `agent_parked` at K=3; claimed task preserved; `rejected` does not park |
| `cap-hit` (folds runaway delegation) | non-termination / 50-subagent runaway (Anthropic) | `cap_hit`, `run_finished{CapHit}`, exit 2; partial artifacts persisted with leftover `Open`/`Claimed` tasks |
| `meta-directive` | meta round-trip alive | `directive_issued` → `directive_fulfilled` (act-with-cite) |
| `declined-directive` | directive decline path | `directive_declined{reason}`; priority-wake of the meta on decline |
| `deadlock` | quiescent-but-unfinished / liveness watchdog (ADR 0015) | `liveness_nudge` count > 0 (vs == 0 on every happy path) + cap termination, exit 2 |
| *(none — built-in arc @ pinned seed)* | happy path (ADR 0021) — **baseline, not a scenario file** | board conservation, termination via `finish_run`, 0 nudges, 0 degrades, exit 0 |

`deadlock` is the only fixture that exercises the liveness watchdog — the last
untested runtime mechanism: the orchestrator scripts a `create_task` (leaving it
`Open`) and puts every team agent `Asleep` (self-sleep from `Idle`, or
`sleep_agent`), then loops `yield`; the quiescent-but-unfinished predicate holds, the
`~500 ms` watchdog fires and forces an orchestrator tick that (being scripted to
yield) cannot converge, and the run terminates on `--max-ticks` / `--max-duration`.

## Rejected

- **TOML for the fixture format** — a new `toml` dep for no gain: scripted `arguments`
  are JSON objects that inline naturally in JSON but nest awkwardly in TOML, and the
  file-level `description`/`reproduces` fields replace TOML's comment affordance.
- **A stateful per-agent response queue / cursor** — reintroduces the run-state ADR
  0019 forbids and would desync concurrent runs; the monotonic `call_seq` already *is*
  the cursor, so a stateless list index is both simpler and correct.
- **In-file request-content matching in v1** (e.g. "respond X when the board digest
  shows task T Open") — makes the scenario parse the rendered world, replicating the
  arc's whole job; the `(agent, call_seq)` index plus fallthrough already covers the
  test needs (script the divergence, fall through for the rest). Noted as a **future
  enhancement**, not v1.
- **A scripted happy-path scenario** — would test the scenario *player*'s ability to
  drive a clean run, not the arc; the arc *is* the product default and #23 e2e-tests it
  directly, so arc drift is the arc's own unit tests' job, not a frozen fixture's.
- **An explicit "emit invalid" marker on a `Call`** — unnecessary: a bad `name` or bad
  `arguments` *is* the invalid call, and semantic validation would forbid the very
  behavior the K=3 fixture depends on.
- **Scenarios overriding embeddings** — embeddings are seed-independent computation,
  not behavior (ADR 0014), and bypass the seam (ADR 0019); a scenario is chat-only.
- **A runtime protocol version marker on the wire** (reconsidered from ADR 0021) — the
  home for any legibility-contract version marker is this file's `version: 1`, off the
  wire, so ADR 0008's schema purity is untouched.

## Clarified by the #22 dry-run gate (2026-07-17)

`TaskId` is one of **four independent contiguous per-run counters** — `EventId` (0-based),
`TaskId` / `MessageId` / `KnowledgeEntryId` (1-based) — all advanced on the single serial
write path (ADR 0011's amendment supersedes the "shared
EventId/MessageId/KnowledgeEntryId allocator" phrasing above; there is no shared counter).
The predictable-`TaskId` guarantee this ADR relies on now holds for `EventId` too, which
#23's "the Nth event is X" assertions use.
