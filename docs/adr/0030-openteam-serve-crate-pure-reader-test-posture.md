# The stream server is a new `openteam-serve` crate — a pure reader over `openteam-core`, tested deterministically at three tiers

This ADR pins where the stream server's code lives and how it is tested
(#40, #41). The two go together because one structural fact drives both: the
server is a **pure reader** — a deterministic function of run-dir bytes, with
no RNG. Its determinism statement is exactly: **"same run-dir bytes → same
responses."** ADR 0025's seed-derivation machinery does not extend to it — and
byte-exact assertions that are impossible for the *writer* (tokio interleaving
makes `events.jsonl` not byte-stable across runs even at a fixed seed) are
trivially available for the server, provided fixtures are frozen files, not
fresh runs.

## A new library crate, `openteam-serve`, on the ADR 0019 pattern

`openteam-serve` mirrors the `openteam-mock` / ADR 0019 precedent exactly: it
exposes **`build_router()`** and **`serve() -> (SocketAddr, ShutdownHandle)`**,
and the `openteam` bin's `serve` subcommand (ADR 0027) is a thin CLI wrapper
around it. SSE resume arithmetic, tail/liveness detection, and the fold are
unit/contract-testable against the router without spawning the binary.

## The new wire-facing types live in `openteam-serve`

The snapshot envelope, the four-state agent vocabulary, the run-list entry,
and the `run_state` control frame (all ADR 0028) are defined in a schema/wire
module inside `openteam-serve`, beside their only producer.

**Deciding principle:** the canonical GUI-facing contract is the **pinned JSON
shapes** in the ADRs + `docs/implementation-pins.md` §9 — matching how ADR 0022
pins `events.jsonl` — **not Rust types**. `openteam-wire` stays untouched: its
ADR 0013 charter is the LLM contract shared with the mock, "and only that";
adding GUI-stream types would drag jiff/uuid/event-shape concerns into the
mock's dependency tree. Extraction to a dedicated contract crate remains a
cheap later refactor **if a Rust stream-consumer ever appears** — a noted seam,
not work.

## Plain dependency on `openteam-core`; exactly two granted core changes

`openteam-serve` depends plainly on `openteam-core` — no extraction, no feature
gates. `Event`'s serde **is** the `events.jsonl` contract
(`Event`/`EventKind`/`EventId` are public with full `Serialize + Deserialize`),
and `Metrics::fold` + `RunSummary` give ADR 0020's pinned fourth view for free —
reuse with zero drift. The bin already links all of core, so binary weight is
unchanged; the transitive reqwest/schemars pull into serve's build is cosmetic
(the workspace builds them anyway).

**Serve owns the reader-side fold** (events → board state + four-state agents +
metrics) in a `fold` module, driving core's public `Board` mutators and
`Metrics::fold`. It defines the four-state wire vocabulary per ADR 0028; the
internal `MeterState` is untouched. Exactly **two** core changes are granted,
pinned in `docs/implementation-pins.md` §9:

1. **`RunSummary: Serialize`** (currently `Debug, Clone, PartialEq` only) —
   required by the snapshot's metrics block.
2. **Public owned board-snapshot construction** — today's `BoardSnapshot`
   (`crates/openteam-core/src/artifacts.rs`) is `pub(crate)`, borrow-based,
   private-fields. Promote it to a public owned type **or** expose a public
   serializer — implementer's choice between those two; the shape must match
   `board.json` byte semantics (pinned key order).

Drift between the serve fold and runtime semantics is guarded by the
finished-run **"folded snapshot ≡ board.json" equivalence test**, adopted at
two tiers below.

## Cargo hygiene

- `openteam-serve` joins `[workspace]` members and `[workspace.dependencies]`
  like its siblings; deps are `openteam-core`, `axum`, `tokio`,
  `serde`/`serde_json` (+ `openteam-wire` directly if naming
  `AgentId`/`SpecialtySlug` warrants it — it arrives transitively regardless).
- `axum` stays a **single workspace-pinned `0.8` entry, default features
  only** — SSE is in the defaults (ADR 0028); no feature additions, no dedup
  issue (the mock consumes the same entry).
- The `openteam` bin gains `openteam-serve = { workspace = true }` and a
  `Serve(...)` clap variant in ADR 0024's style.

## Three test tiers — weight in unit + serve-crate integration, thin e2e

- **Unit (inline `#[cfg(test)]`)**: the snapshot fold including the four-state
  transitions (ADR 0028); the tail line-parser (complete-line rule; torn-line
  re-read on next poll — ADR 0027); the finished/live/aborted classifier (the
  bookend × flock trichotomy).
- **Integration (serve crate's own `tests/`, ADR 0019 pattern — real loopback
  `serve()`, in-process, deterministic)**: the full endpoint contract — run
  list JSON; snapshot JSON; SSE stream content; resume arithmetic (fresh
  connect = from `EventId` 0; `Last-Event-ID: n` = replay from n+1; `?from=`
  equivalent); 400 on an unparseable resume id; 204 on caught-up reconnect to
  any terminal run; the id-less `run_state` abort control frame;
  disconnect-on-lag.
- **E2e (bin `tests/`)**: one thin case — `drive()` a `--mock` run to
  completion, spawn `openteam serve --dir <tempdir> --port 0` as a child
  process, read the bound address it prints (the parseable print of ADR 0027
  is load-bearing here), hit list + snapshot + stream-to-204, kill the child.

**Live-run e2e (concurrent real `openteam run --mock` + `serve` processes) is
rejected-for-now** per ADR 0025's anti-flake doctrine: every live-path piece
(tail, flock, abort) is covered deterministically at lower tiers. **Recorded
fallback if a future need arises**: a single coarse smoke test asserting only
"stream eventually delivers `run_finished` then 204" under a generous timeout —
nothing ordering- or timing-sensitive. Not in v1.

## Live-tail determinism: the test is the writer

Integration-tier tail tests use a tempdir run dir where **the test itself is
the writer**: it appends `events.jsonl` bytes and holds the `run.lock` flock (a
separate `open()` of the same file in the same process conflicts under flock
semantics — no second process needed).

- **Torn line**: append a partial line without the trailing newline → assert
  not emitted within N poll cycles; append the remainder + `\n` → assert the
  event arrives.
- **Abort**: drop the test's lock with no `run_finished` bookend present →
  assert the `run_state: aborted` control frame, then stream end.
- **Discipline**: negative assertions are bounded in *poll cycles*, never
  wall-clock sleeps (feasible via the injectable config below).

## Golden-stability posture

1. **SSE `data:` payloads are byte-golden**: each payload must be
   byte-identical to the corresponding `events.jsonl` line in the fixture. The
   verbatim-line guarantee (ADR 0028) *is* the contract; assert it as bytes.
2. **SSE framing is NOT byte-golden**: assert parsed frame semantics only (id
   present/absent, event name, data, status codes). Keep-alive comments, field
   order, and blank-line placement are not contract.
3. **Snapshot JSON is value-golden** (`serde_json::Value` equality — key order
   is not contract). The **folded snapshot ≡ board.json** equivalence test for
   finished runs runs in **both** places: (a) integration tier against the
   frozen fixture; (b) as a cheap e2e invariant helper folded over every
   freshly generated `--mock` run dir the happy-path tests already produce —
   upgrading it from "holds for one frozen log" to "holds for any seed" at
   near-zero cost (a library call, no server involved).
4. **Fixture provenance**: one small **checked-in fixture run dir** (frozen
   `events.jsonl` + `board.json`, captured once from a `--mock` run) under the
   serve crate's `tests/fixtures/`, plus **synthetic hand-built tempdir logs**
   for edge cases (torn line, abort, empty run). Regeneration on schema change
   is a deliberate act (the event taxonomy is ADR-0022-frozen per the map's
   out-of-scope list); part (b) of the equivalence test covers the
   drift-honesty concern that fresh generation would otherwise buy.

Debug-page coverage is at most a "`GET /` returns 200 `text/html`" smoke
assertion — it is a non-contract surface (ADR 0029).

## Timing knobs: constructor-injectable `ServeConfig`, not CLI

A constructor-injectable config — indicatively
`ServeConfig { poll_interval, keep_alive, retry_ms, broadcast_capacity }` —
with pinned production defaults **100 ms / 15 s / 2000 ms / 1024**
(`docs/implementation-pins.md` §9). Tests construct the router/state directly
with a ~5 ms poll, short keep-alive, and a tiny broadcast capacity (making
disconnect-on-lag testable without generating 1024+ events). The CLI surface
stays exactly `serve --dir --port` (ADR 0027). This matches the mock's
`FrozenClock` injection precedent (ADR 0019).

## Rejected

- **A module in the bin** — the bin has no lib target (`cli.rs` + `main.rs`,
  tested via `assert_cmd`); everything would be testable only end-to-end
  through the binary, hostile to SSE-resume and torn-line edge cases, and it
  breaks the ADR 0019 symmetry.
- **In `openteam-core`** — puts an HTTP *server* (axum) into the domain/runtime
  crate whose only HTTP today is client-side reqwest, bloating every core
  consumer for no gain.
- **An `openteam-events` contract-crate extraction** — splits a green,
  fully-specified crate and churns ADR-pinned import paths for a consumer that
  doesn't exist; recorded as the noted future seam.
- **Feature-gating core** — cfg seams + CI matrix cost for a compile-time
  saving nobody feels.
- **GUI-stream types in `openteam-wire`** — violates its ADR 0013 charter and
  drags jiff/uuid/event-shape concerns into the mock's dependency tree.
- **Live-run e2e in v1** — reintroduces interleaving nondeterminism ADR 0025
  exists to keep out, for no reader coverage the lower tiers lack;
  rejected-for-now with the recorded coarse-smoke fallback.
- **Driving a real concurrent `--mock` run for tail tests** — couples the serve
  crate to the bin and reintroduces interleaving nondeterminism for no extra
  reader coverage; the test-as-writer covers it in-process.
- **Byte-golden SSE framing or key-ordered snapshot JSON** — freezes
  non-contract incidentals (comment cadence, serializer key order) and makes
  benign refactors test-breaking.
- **CLI flags for the timing knobs** — test-only knobs on a closed CLI surface
  (ADR 0024's discipline); constructor injection serves tests without widening
  the surface.
