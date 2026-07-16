# The built-in behavior arc and the prompt-legibility contract: identity from the wire, phase from the rendered world

The built-in `BehaviorModel` (the default adapter behind ADR 0019's synchronous
`chat(req, identity) -> ChatDecision` seam) is a **pure function of
`(request, identity, seed)` with zero run-state** — it never sees the board, the
store, or any memory of prior turns. This ADR pins how it nonetheless carries **any**
prompt through a bounded decompose → work → converge arc that terminates by
construction, and the **prompt-legibility contract** — the two-halves-of-one-contract
agreement between what the harness *renders* and what the mock *reads*.

## The identity-vs-behavior boundary (the crux of the whole system)

There are two, and only two, things the mock does with a request, and keeping them
apart is the load-bearing rule:

- **Identity is read from the wire, never from content.** The role, agent handle, and
  specialty slug come solely from the `user` field (ADR 0012 grammar
  `orchestrator` / `meta-agent:<id>` / `team-agent:<id>:<slug>`) plus the
  `X-OpenTeam-Call-Seq` / `X-OpenTeam-Seed` headers (ADR 0008). Keying identity on
  rendered message content is **forbidden**, full stop — that is what keeps ADR 0008's
  schema purity real (any OpenAI-schema client is served identically).
- **World state is read from content — and that is the mock's entire job.** Deciding
  the *next plausible action* by reading the rendered `user`-message sections (board
  digest, claimed task, recent-activity window, fresh messages, directives/outcomes),
  the `tools` array (ADR 0013), and the turn-local `assistant`/`tool` messages
  accumulated in the inner loop (ADR 0015) is **not** content-sniffing and is not
  forbidden. Because the mock holds no state, **the board rendered in the request IS
  the arc's memory** — the phase is re-derived from the visible world every
  completion, not remembered.

The mock ignores the system-prompt prose entirely: the role skeleton (ADR 0012) is
**inert** to it. Behavior is driven only by the `user`-message sections, the `tools`
array, the turn-local messages, and the identity channels.

## The legibility contract: section line-grammars (#18 pins, #13 renders, #23 tests)

Because the mock parses the rendered world, the **line grammars** of the context
sections are a wire-level contract, not a rendering detail: each parseable section
(board digest, claimed task, recent-activity window, fresh messages,
directives/outcomes) has a **stable, machine-parseable line format** that #18 owns and
pins, the #13 context assembler must **render to exactly those grammars**, and #23's
contract tests assert the pairing. This **retroactively constrains ADR 0016** — which
left section rendering as an implementation detail; that coupling is deliberate and is
the "two halves of one contract." Minimally the mock must be able to read, from the
digest, each task's **id, state (`Open` / `Claimed{by}` / `Done` / `Cancelled`), and
team tag**; from the claimed-task section, its **presence** (Working) and the task id;
from the recent-activity window, a **countable list of its own prior work-actions**;
from the directives section, each **pending judgment directive's id**. The system
prompt carries none of this and is exempt. The mock still learns the **callable verbs
and their arg schemas solely from the request's `tools` array** (ADR 0013) — the
grammar contract governs the `user` message, the tools array governs the calls.

**No version marker in v1.** The harness and the mock are one cargo workspace built in
lockstep, so a cross-version pairing is impossible by construction; a runtime protocol
marker would guard an unreachable failure mode while polluting ADR 0008's schema purity
(a real endpoint has no such field and would ignore it). The real lockstep guard is
CI's #23 contract tests. If independent deployment ever appears, the home for a marker
is an `X-OpenTeam-*` header or a #20 scenario-file schema version — never the built-in
wire contract.

## Determinism derivation

Every seeded choice in a completion draws from a single per-completion RNG,
`ChaCha8Rng::from_seed(hash(seed ‖ user ‖ call_seq))` (rand_chacha 0.10, the pinned
stack), keyed on exactly ADR 0008's determinism tuple `(user, call-seq, seed)` — unique
per completion, so a turn's up-to-eight completions never collide. The decision is a
pure function of `(request, identity, seed)`. This forces two testing tiers (#23):
exact-envelope **contract tests feed synthetic fixed requests** and assert the pure
function; **live e2e tests assert only logical invariants** (board conservation,
termination, per-agent event order) — never global byte-identity, which stays map
Out-of-scope because tokio interleaving varies the *sequence of requests* even though
each response is pure.

## The arc, per role — phase re-derived from the rendered world each completion

**Orchestrator** (phase from board digest + directives + fresh messages). Total task
budget `T = f(seed) ∈ [1, MAX_TASKS=8]`, recomputed from the seed header each turn
(stable). Read the board count `n` from the digest: **`n == 0` → decompose** (emit
`create_task` up to `T`, optionally forming a team / authoring specialties; staged in
≤2 batches, batch two firing when `0 < n < T`); a **pending judgment directive present
→ resolve it** (act-with-cite via `in_response_to`, or `decline_directive`, seeded);
**non-terminal tasks remain (Open/Claimed) → yield** (the mandatory no-tool-call yield,
`finish_reason:stop`, letting team agents work — with a rare seeded steer `post_message`);
**all tasks terminal and `n > 0` → `finish_run(report)`**. Decomposition cannot run
away because the memory that bounds it — the visible board count — caps creation at `T`;
`finish_run` is only called once the digest shows no Open/Claimed task, so it never
trips ADR 0006's blocker refusal.

**Team agent** (phase from claimed-task section + digest + recent-activity window +
turn-local messages). **Has a claimed task**: count its own prior work-actions visible
in the recent-activity window; **complete when that count ≥ `W_task = g(seed,agent,task)
∈ [1..3]` OR the window is degraded** — otherwise emit one **work-action** seeded among
`write_knowledge` (Note) / `post_message` (team status) / `search_knowledge`, then
`complete_task(result)`. **No claimed task, an eligible Open task in the digest →
`claim_task`** (a lost race returns `rejected`, which is well-formed and safe; if the
turn-local messages already show a lost claim this turn, **yield** rather than hammer —
the scheduler re-dispatches on the next nudge, ADR 0015). **No claimed task, no eligible
work → yield.**

**Meta-agent** (phase from metrics digest + directive-outcomes slot). **Directive-
outcomes shows none issued by me → emit ≤1 seeded directive** (`propose_respecialize`
on an Idle generalist, or a mechanical `set_parallelism`); **else → yield.** The
already-issued bound is read statelessly from the outcomes slot. Under `--meta-agents N`
each handle keys a distinct stream (ADR 0020) → `N` diverse proposals; scenarios crank
it further.

## Termination by construction (the three guarantees)

1. **Bounded decomposition**: creation is hard-capped at `T = f(seed) ≤ 8` because the
   board count that bounds it is *visible in the request*, so no seed and no
   interleaving can make it exceed `T`.
2. **Every task converges**: a claimed task completes within `W_task ≤ 3` work-actions.
   The completion rule is **degradation-safe by inversion** — "complete when actions
   seen ≥ `W_task` **OR** the window is degraded", so a window that drops items under
   budget pressure *forces completion sooner* rather than blocking it. Had the rule been
   "loop until I see `W_task` actions", degradation dropping items below `W_task` could
   loop forever; making degradation a shortcut-to-completion, never a block, closes that
   hole. The recent-activity window is sized to comfortably hold `W_task + 1` items
   (trivial at `W_task ≤ 3`), so under normal budgets the count is faithful and the
   shortcut fires only under extreme pressure. This is the subtlest correctness point in
   the arc.
3. **Turns end**: per `(agent, call-seq)` the arc emits the **no-tool-call yield**
   (`finish_reason:stop`) whenever nothing plausible remains — an idle agent with no
   eligible work, the orchestrator waiting on in-flight tasks, a meta-agent past its one
   directive — so a turn always terminates before `MAX_TOOL_ITERS` (ADR 0015) and an
   agent never loops to the cap every turn.

Together these drain the board to all-terminal, at which point the orchestrator finishes
— with the liveness nudge (ADR 0015) a backstop that never fires on the happy path.

## The realism dial

The seed drives **plausible variety that never touches structural invariants**: the
task count `T`, task title/description phrasings (which may weave in goal words for
plausibility), `W_task` per task, whether a team forms and its size, the count and kind
of messages / knowledge notes, and which meta directive is proposed. The dial varies
**counts and text within fixed bounds only** — it never alters the arc's shape (always
decompose → work → converge → finish, always bounded, always terminating). No seed can
produce a non-terminating or schema-invalid run; #23's invariants (structural, not
textual) therefore hold for **all** seeds, and varied phrasings are safe because no test
asserts exact generated text.

**Rejected.** Remembering the phase in mock state (breaks ADR 0019's stateless purity —
the board-as-memory is what makes statelessness work); keying any behavior off the
system-prompt prose (it is inert; the `user`-message sections are the behavioral input);
a runtime protocol version marker (guards an unreachable cross-version pairing and
pollutes ADR 0008's schema purity — CI contract tests are the real guard); unbounded or
depth>1 decomposition and discovered-work runaway in the built-in arc (they are the
prior-art failure modes #23 exercises as #20 *scenarios*, not the default UX);
single-turn claim→work→complete tasks as the default (hides the product's signature
dynamics — realtime inter-agent messaging, idle re-dispatch, over-time work — behind a
trivial one-shot; multi-turn is the default, single-turn a scenario); a
"loop-until-seen-`W_task`" completion rule (a degrading window could drop below the
threshold and loop forever — the OR-degraded inversion is mandatory); and passive
yield-only meta in the default arc (would hide the signature self-monitoring layer — the
default must show the bounded propose→fulfill/decline round-trip alive).
