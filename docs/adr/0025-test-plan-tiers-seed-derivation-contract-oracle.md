# The test plan: three tiers, an FNV-1a-64→`seed_from_u64` derivation, a third-party contract oracle, and invariant-only e2e

This ADR makes the already-decided test **strategy** (charting round 1) concrete: the
unit-target list (module → what it asserts), the seeded-e2e matrix and its invariant
discipline, the mock contract-test set, the pinned seed-derivation scheme, and the
`mise run ci` gate. It is the last map ticket (#23); resolving it closes the map. Three
things here are genuine, hard-to-reverse trade-offs and are the reason this is an ADR
rather than a checklist: the **seed-derivation function** (mock and tests must agree on
it forever), the **contract-test oracle** (ADR 0019 explicitly deferred "which validator"
to #23), and the **e2e invariant discipline** under nondeterministic tokio interleaving.

## Three test tiers, placed per the workspace (ADR 0013/#10)

Tests live as inline `#[cfg(test)]` modules (unit), per-crate `tests/` (integration), and
`assert_cmd` in the bin's `tests/` (e2e); dev-deps `tempfile` / `assert_cmd` / `predicates`
(rust-crate-inventory), plus `async-openai` as a **`openteam-mock` dev-dep** (contract
oracle, below).

### Tier 1 — unit targets (core logic; the seams the tdd skill agrees up front)

| Crate | Target | Seam → what it asserts |
|---|---|---|
| `openteam-wire` | **TokenCounter** | `CharCountTokenizer` = `ceil(chars/4)`; usage free-fns (`prompt = Σ` message content + tool-call `arguments`; `completion`; `total = prompt+completion`). |
| `openteam-wire` | **Wire-type serde** | request optionals omit-when-absent; response nullable keys (`content`/`refusal`/`logprobs`) serialize explicit `null`; embeddings-request `deny_unknown_fields`; base64 f32-LE codec round-trips. **This is the strictness layer** (paired with the lenient contract oracle). |
| `openteam-core` | **Context assembler** | exactly two messages (`system`+`user`); fixed section order; renders the **exact ADR 0016 pinned line-grammars** (the render-side golden of the legibility contract); priority degradation — Goal+Directives never dropped, board-digest terminal tail shrinks first, retrievals drop lowest-cosine, oldest-first sections always deliver ≥1 oldest item; emits `context_degraded` only on an actual drop/truncation. |
| `openteam-core` | **Scheduler / reactor** | edge-triggered (an idle agent that declines to claim is not re-dispatched without a fresh nudge); `Semaphore(--parallel)` acquired by team agents only, control plane exempt; liveness watchdog fires **only** on the quiescent-unfinished predicate, forces exactly one orchestrator tick, never auto-wakes team agents; sleep legal only from Idle, wake only from Asleep. |
| `openteam-core` | **Board transitions** | Open→Claimed first-claim-wins (second claim → `rejected:task_not_open`); complete/release/unassign/cancel transitions; no Failed state; ≤1 claimed task per agent; team-eligibility checked at claim; `finish_run` `rejected` while any Open/Claimed task remains, enumerating blockers. |
| `openteam-core` | **Respecialization** | legal only when `Idle && !in-flight`; wipes the recent-activity window + assignment association; preserves identity and the monotonic call-seq; emits `agent_respecialized{from,to,via_directive?}`. |
| `openteam-core` | **Vector cosine scoring** | feature-hash embedder deterministic **and seed-independent** (identical text → identical vector; L2-normalized; D=256); cosine top-k ranks higher-token-overlap text first; the internal `Embedder` seam is injectable so ranking is tested without a live mock. |
| `openteam-core` | **Metrics fold** *(added)* | `fold(&Event)` → the three projections, each metric exact and counted in EventId-delta / tick units (never wall-clock); `issued = directive_issued`, `fulfilled = directive_fulfilled + directive_issued[Mechanical]`, `declined = directive_declined`. |
| `openteam-mock` | **Built-in arc invariants** | a **fixed seed sweep** (below) asserts, for every seed: `T = f(seed) ∈ [1,8]`; `W_task = g(seed,agent,task) ∈ [1..3]`; the arc **never emits an `invalid` call** (so it never parks itself); a **targeted** test that a degraded recent-activity window forces completion (the degradation-safe inversion, ADR 0021); phase re-derived from the rendered world only. |
| `openteam-mock` | **Seed derivation** *(added)* | the derivation fn (below) is stable (same tuple → same stream) and decorrelated across `seed` / `user` / `call_seq`. |
| `openteam-mock` | **Scenario-player lookup** *(added)* | exact-handle selector beats role wildcard; `responses[call_seq]` indexing; `repeat` cycling past the list; empty `repeat` → arc fallthrough; structural-only validation that **permits** `invalid` scripted calls (ADR 0023). |

### Tier 2 — seeded e2e (bin `tests/`, `assert_cmd` + `tempfile` + `predicates`)

Driven with a fixed `--seed` + `--scenario`, asserting on the **persisted** artifacts
(`events.jsonl`, `board.json`, `report.md`, exit code). The **invariant discipline** (the
crux — see the determinism section) governs every case.

- **happy path — built-in arc @ pinned seed, `--parallel N`**: board conservation
  (every `task_created` ends Done or Cancelled — a task-event fold), termination via
  `finish_run`, `liveness_nudge` count == 0 && `context_degraded` count == 0, exit 0.
- **happy path — built-in arc @ pinned seed, `--parallel 1`** *(the tighter variant)*:
  with a single active worker the concurrent-actor set shrinks to
  {orchestrator, meta, one worker}, so the event order is *more constrained* and a fuller
  ordered subsequence is assertable — catching payload/ordering bugs the folds miss. This
  is a **tighter invariant assertion, not a byte-identical global golden**: the control
  plane still interleaves, so global byte-identity stays map-Out-of-scope.
- **the nine ADR 0023 fixtures**, each backing its one assertion: `stall`
  (ticks-since-`task_completed` grows; claimed task never Done), `livelock` (pair-churn
  counter; no `task_completed`), `message-flood` (mailbox depth/max/oldest-pending-age;
  volume by address kind), `context-collapse` (`context_degraded` on retrievals +
  fresh-messages under a tiny budget), `malformed-k3` (consecutive-malformed counter →
  `agent_parked` at K=3; claimed task preserved; `rejected` does not park), `cap-hit`
  (`cap_hit` → `run_finished{CapHit}`, exit 2; partial artifacts persisted with leftover
  Open/Claimed tasks), `meta-directive` (`directive_issued` → `directive_fulfilled`),
  `declined-directive` (`directive_declined{reason}` + priority-wake of the meta),
  `deadlock` (`liveness_nudge` count > 0 + cap termination, exit 2).
- **CLI e2e**: flag parsing; the **exit-2 discriminator** — `--parallel > --agents` exits
  2 with **no** artifacts / no `run_started` (a usage error) vs a cap-hit exit 2 **with**
  artifacts and a `run_started` (ADR 0024); the three run exit codes (0/2/1); `--quiet`
  stdout byte-compared to the persisted `report.md`.

### Tier 3 — mock contract tests (`openteam-mock` `tests/`, real loopback `serve()`)

Synthetic fixed requests fed to the router bound on real loopback (ADR 0019), asserting:
1. **Schema-valid OpenAI**: every response deserializes cleanly into **`async-openai`'s
   response types** — the third-party oracle (below).
2. **Exact-envelope determinism**: a frozen injected `Clock`, `id = chatcmpl-{seed}-{user}-{call_seq}`,
   and the same `(user, call_seq, seed)` tuple → **byte-identical** response.
3. **The built-in arc never emits an `invalid` call** (structural, re-asserted at the wire).
4. **Embeddings**: deterministic and seed-independent (identical text → identical vector),
   `deny_unknown_fields` → 400 on stray fields, base64 the default `encoding_format`.

### The legibility-pairing test (bin `tests/`) — the "two halves of one contract"

Placed in the **bin's `tests/`** — the composition root is the only place the `core`
assembler and the `mock` arc meet. It renders a known world through the **real** core
assembler, feeds the exact rendered request to the **real** mock arc, and asserts the arc
reads the intended **world state** (task states, claimed-task presence, work-action count,
directive kind+args) — the *pairing*, never exact generated text. This complements the
render-side golden (Tier-1 assembler test) with a wiring-side check against the real
parser, so a grammar drift on either half fails a test.

## The seed-derivation scheme — FNV-1a-64 over a length-delimited encoding → `seed_from_u64`

ADR 0021 pins `ChaCha8Rng::from_seed(hash(seed ‖ user ‖ call_seq))` but named no `hash`,
and the pinned stack (rust-crate-inventory) carries **no** 32-byte hash crate (no
`sha2`/`blake3`). Concretized, using only the pinned stack:

```
seed_bytes = fnv1a64( LEN(seed_u64_le) ‖ seed_u64_le
                    ‖ LEN(user_utf8)   ‖ user_utf8
                    ‖ LEN(call_seq_le) ‖ call_seq_u64_le )      // hand-rolled FNV-1a-64
rng        = ChaCha8Rng::seed_from_u64(seed_bytes)             // rand_chacha 0.10 SplitMix expansion → 32-byte seed
```

- **Length-delimited canonical encoding is load-bearing** — it prevents the
  `("a", 12)` vs `("a1", 2)` field-boundary collision that a bare concatenation would
  admit. Each field is prefixed with its byte length.
- **FNV-1a-64** is hand-rolled (zero new dep, same hash family as the ADR 0014 feature-hash
  embedder). We **accept the 64-bit funnel**: a mock needs determinism + decorrelation,
  not cryptographic entropy, and collision probability across a run's few-thousand
  `(user, call_seq)` tuples on 2⁶⁴ is negligible. `seed_from_u64`'s SplitMix step fills
  ChaCha8's 32-byte seed deterministically and portably (stable within the pinned
  rand_chacha 0.10).
- **Ownership**: the derivation fn lives in **`openteam-mock`** — the harness only
  increments `call_seq` and stamps the `X-OpenTeam-*` headers; it never derives the RNG.
  The contract tests (same crate) call the identical fn, so mock and tests provably agree.

Rejected: a full 32-byte hash fill or adding `sha2`/`blake3` (crypto entropy a
content-blind mock does not need, and a new dep for no determinism gain); a bare
concatenation without length prefixes (field-boundary collisions).

## The contract-test oracle — deserialize into `async-openai`, not our own types

ADR 0019 deferred "which validator" here. The oracle is a **third-party OpenAI client's
types**: deserialize every mock response into `async-openai`'s response structs (a
`openteam-mock` **dev-dep**, test-only, out of production deps). This is the faithful
realization of ADR 0013's "the mock provably serves any OpenAI-schema client" — round-
tripping our own `openteam-wire` types would be **tautological** (validating output with
the same types that produced it proves nothing; the tdd skill's anti-pattern). The two
layers cover both properties: async-openai's lenient deserialize is the *consumability*
oracle ("a real client accepts this"); the Tier-1 `openteam-wire` serde unit tests pin
*exactness* (explicit `null`s, omitted optionals). **Fallback** (if async-openai's dep
tree proves heavy or version-drifts at implementation): a small hand-written reference-
response struct set in the test module, independent of `openteam-wire`, `deny_unknown_fields`,
shaped from the #4 wire-subset research — lighter, still non-tautological. Rejected: a
`jsonschema` crate against a captured OpenAI schema (heavier, and the captured schema
drifts from the live API).

## E2e determinism — invariant-only, per-`source` order, over the real multi-thread runtime

Byte-identical global event logs are map-Out-of-scope: the multi-thread tokio runtime
varies the *sequence of requests* across runs even though each mock response is a pure
function of its request (ADR 0021). So e2e runs the **real multi-thread default** and
asserts **only interleaving-invariant facts**:

- **Fold / set / count invariants**: board conservation, terminal-state counts, exit
  codes, `liveness_nudge` == 0 && `context_degraded` == 0 on happy paths, message volume
  by kind, directive counts — none depends on interleaving.
- **Per-`source` ordered subsequences**: each agent's own events are totally ordered by
  its call-seq regardless of interleaving, so per-agent order is a clean filter on
  `source` (ADR 0022). Per-agent assertions are phrased over **"the claimant of task N"**,
  never a fixed handle — the happy-path claim race means *which* agent wins varies
  run-to-run.
- **Predictable `TaskId`** (1-based own counter, ADR 0023): the Nth `create_task` is task
  N because authorship is orchestrator-only and serial, so `claim_task{task:N}` /
  `complete_task` assertions are stable.
- **No global positional "the Nth event is X"** in multi-agent e2e — those assertions live
  only where a total order is pinned: the single-active-agent fixtures and the unit tier.
  (Contiguous EventIds, ADR 0022, make id-based fixture assertions tractable, but the
  *assignment* of an id to an event still follows interleaving.)

These invariants hold **by construction** (bounded decomposition, `W_task ≤ 3`
convergence, every turn yields — ADR 0021), so the seed sweep and the e2e suite are a
**regression guard confirming the construction, not the primary correctness argument** —
the bound is a code invariant, not a statistical hope.

## The seed sweep — fixed, not proptest

The "holds for all seeds" arc invariants run a **fixed in-process sweep of seeds `0..1000`
plus edge seeds (`0`, `1`, `u64::MAX`, a couple of large primes)**, calling
`BehaviorModel::chat()` directly (no HTTP), asserting `T ∈ [1,8]` / `W_task ∈ [1..3]` /
never-`invalid` / never-self-park. A fixed sweep is chosen over `proptest` deliberately:
proptest adds a dep and injects run-to-run variance (randomized cases + shrink) that
`mise run ci` must not have — the suite stays reproducible.

## The CI gate

`mise run ci` = `fmt:check` + `clippy --workspace --all-targets -- -D warnings` + `test`
(the full workspace test suite) — exactly ADR 0024 / `mise.toml`, unchanged. The fixed
seed sweep and the real-loopback contract tests fit this budget (in-process `chat()` calls
and localhost round-trips; no external I/O).

## Rejected

- **Validating mock output with our own `openteam-wire` types** — tautological; a
  third-party client is the only honest "any OpenAI-schema client" oracle.
- **A `jsonschema` crate against a captured OpenAI spec** — heavier, and the captured
  schema drifts from the live API; the deserialize-into-a-real-client approach tracks a
  real consumer.
- **A 32-byte hash fill / adding a crypto hash crate** for seed derivation — entropy a
  content-blind mock does not need, and a dep for no determinism gain.
- **`proptest` for the arc seed sweep** — randomized cases + shrink add a dep and
  run-to-run variance that a reproducible `mise run ci` must avoid.
- **Forcing a single-threaded tokio runtime in e2e to chase byte-identical logs** — the
  production default is multi-thread; a test that only passes single-threaded would test a
  configuration we don't ship. Invariant-only assertions are runtime-agnostic.
- **A dedicated cross-crate integration test-crate for the pairing test** — ceremony; the
  bin is already the composition root where both crates meet and where e2e lives.

**Amended by ADR 0026 (2026-07-17).** Default `openteam run` now hits the network, so
the Tier-2 e2e and the legibility-pairing cases pass `--mock` (the shared `drive()`
helper adds it) to keep every seeded test offline and deterministic. The `async-openai`
contract oracle stays a `openteam-mock` dev-dep, unchanged. The seed-independence /
cosine-ranking embedding invariants are now explicitly *mock-path* properties — the real
embedder returns genuine semantic vectors, so they hold only under `--mock`.
