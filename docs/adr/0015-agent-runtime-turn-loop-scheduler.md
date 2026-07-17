# The agent runtime: capped-inner-loop turns, a three-state team-agent lifecycle, and an event-driven scheduler

> **Refined by ADR 0017**: the "malformed turn" definition below ("≥1 tool call, none succeeded / every call a schema error") is sharpened once the tool-outcome envelope exists — a turn is malformed (parks) **only when every call returns `invalid`** (a schema/parse fault); a `rejected` domain refusal (e.g. a lost claim race) is a well-formed call that resets the counter exactly like `ok`. See ADR 0017.

Every agent runs the same turn loop (ADR 0002), and a **turn is a capped inner
loop**, not a single completion: context is assembled **once** at turn start
(ADR 0004), then `completion → execute tool calls → feed schema-correct tool
results back → completion …` repeats until the model **yields** (a completion
with no tool calls) or the per-turn cap `MAX_TOOL_ITERS` (default 8,
`--max-tool-iters`) is hit. The inner loop is required, not a convenience: an
agent must *see* a verb's result (a `search_knowledge` hit, a claim win/loss)
inside the same reasoning episode to chain on it, and the OpenAI wire contract
demands one `role:"tool"` reply per `tool_call_id` — both force feeding results
back before the turn ends. This is why the mock can no longer key determinism on
a turn index: up to eight completions share one turn and would collide. The
determinism key becomes a **per-agent call-sequence counter** incremented on
every `/v1/chat/completions`, carried in the `X-OpenTeam-*` header (amends
ADR 0008); `--max-ticks` still counts orchestrator turns.

**Team-agent lifecycle is three states — `Idle` (no claimed task), `Working {
task }` (exactly one, the anchor from ADR 0010), `Asleep { since }`** — with an
orthogonal "turn in flight" flag (respecialize/unassign require `Idle && !in-flight`,
ADR 0003). The orchestrator and meta-agents are control-plane and hold no such
state. **The malformed-fault park is not a fourth state**: it enters the *same*
`Asleep`, distinguished only by its entry event — `agent_slept{reason:Deliberate}`
vs `agent_parked{reason:MalformedOutput,count}`. A turn counts **malformed** iff
it emitted ≥1 tool call and **none succeeded** (every call a schema error); a turn
with **zero** tool calls is a clean yield/decline, never malformed; any turn with a
successful verb resets the counter. **K = 3** consecutive malformed turns parks
the agent — a fixed internal constant, not a flag, because against the mock
malformed output cannot occur (the prompt-legibility contract tests guarantee it),
so tuning K is a real-endpoint concern and real endpoints are out of v1 scope.
Parking **preserves the claimed task** (`Claimed{by}` untouched — no auto-release,
consistent with ADR 0010's no-Failed-state, deliberate-transition-only contract);
the orchestrator recovers it by explicit **Wake** (restores `Working{task}`) or by
**unassign** to reallocate/respecialize.

**The scheduler is an event-driven reactor (ADR 0007), never an interval loop.** A
**nudge** — turn-completed, message-accept (ADR 0011), task-state-change,
directive-enqueue, or wake — re-evaluates dispatch. A team agent is dispatched a
turn when `Working` (progresses its task, back-to-back until release / complete /
park) or when `Idle` **and** eligible open work or a queued message exists; it must
first acquire a `--parallel` permit. Dispatch is **edge-triggered**: an idle agent
that runs a turn and declines to claim is not re-dispatched until a fresh nudge — no
busy-spin. This is the faithful reading of "keep the idle agents working while there
is work": the orchestrator creates tasks and the *scheduler* dispatches idle
**eligible** agents so they claim autonomously (pull-only, ADR 0010) — it is **not**
the orchestrator emitting wake verbs at non-asleep agents, and agents never
auto-Sleep. **Wake** is reserved strictly for `Asleep → schedulable` and is always
explicit (orchestrator verb / mechanical meta-directive), never automatic, never
self-issued. **Sleep** is legal only from `Idle` and callable three explicit ways —
orchestrator verb (the "told to" path), mechanical meta-directive, and team-agent
self-sleep. The **orchestrator tick** is one orchestrator turn fired on a nudge when
there is pending input, unassigned work, or an idle agent with open work.

**`--parallel N` is a `tokio::Semaphore(N)`**: a team agent acquires one permit per
turn and releases after; the control plane (orchestrator, meta-agents) is **exempt**
and never acquires (ADR 0002 — control must never queue behind workers). The
mechanical "tune effective parallelism" directive (ADR 0005) `add_permits` /
`forget_permits` within `[1, N]`. `--parallel` defaults to `--agents` (no throttle
unless set lower) and CLI validation (#21) errors if `--parallel > --agents`.

**The liveness nudge is a ~500 ms watchdog asserting the invariant "quiescent ⟹
board done."** It fires only when no turn is in flight, every team agent is Idle or
Asleep, the board is unfinished (≥1 Open, or a task Claimed by an Asleep agent), and
the orchestrator has no pending input — it emits a distinct `liveness_nudge` event
and **forces one orchestrator tick** (control plane, no permit): the deadlock breaker
that lets the orchestrator wake parked agents, unassign, cancel, or finish. It does
**not** auto-wake team agents (wake stays explicit). Without it, an all-parked pool
plus a quiescent orchestrator hangs forever; with it the control plane always gets a
chance to converge. In correct operation it never fires, so it cannot perturb
deterministic tests; its firing is a scheduling bug surfacing loudly.

**Rejected.** Single-completion turns (can't chain within a reasoning episode, and
strand tool results that the rebuilt next-turn context drops); a distinct
fourth "parked" state (the scheduler treats it identically to Asleep — one state,
two entry events keeps transitions minimal); auto-release on park (silently
contradicts the board's deliberate-transition contract); tunable K and keying
determinism on the turn index (both dead against the only tested backend); and a
level-triggered "dispatch while open work exists" (busy-spins idle agents that
decline — edge-triggering plus the liveness backstop covers it).

**Amended by issue #28 (2026-07-17).** The watchdog's "the orchestrator has no
pending input" clause reads as **undelivered mailbox items only** — a pending
judgment directive no longer holds the predicate false. Directives are
edge-triggered end to end: the `directive_issued` event beyond the
orchestrator's watermark dispatches the tick that renders it, and a directive
the orchestrator has seen and left pending generates no further ticks. Counting
it as pending input let an orchestrator that kept yielding on one suppress the
watchdog forever, so a fully-dead run (Open task, every agent Idle/Asleep, no
turns in flight) degraded to silent cap-riding — and a capless run would hang.
The forced tick is exactly the directive's resolve-or-decline chance, which is
this ADR's intent; the `deadlock` fixture now exercises the watchdog with a
parked judgment directive in place.
