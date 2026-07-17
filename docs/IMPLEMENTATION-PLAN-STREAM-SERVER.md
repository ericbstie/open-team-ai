# Implementation plan — building the stream server (`openteam serve`)

The stream-server **map (#36) is complete**: every decision is pinned in ADRs
**0027** (sidecar topology, `run.lock` flock liveness, poll tailing, CLI),
**0028** (SSE + snapshot + run-list wire contract), **0029** (`/v1/` versioning,
debug page), **0030** (`openteam-serve` crate, pure-reader test posture), and
`docs/implementation-pins.md` **§9** (exact routes, status codes, JSON shapes,
`ServeConfig` defaults, bound-address print). ADRs 0022/0024 carry dated
amendment notes (`run.lock` in the run dir; the `serve` subcommand). This
document is the build order — it does **not** re-decide anything; where a detail
is unclear, the ADR wins, then pins §9.

> **Golden rule:** determinism statement for everything below is ADR 0030's
> **"same run-dir bytes → same responses"** — the server is a pure reader.
> Fixtures are frozen files, never fresh runs (except step 8's invariant
> helper, which is the deliberate exception).

## Ground truth already in place

- The 4-crate workspace is green (`mise run ci`): `openteam-wire` →
  `openteam-core` + `openteam-mock` → `openteam` (bin). ADR 0019's
  `build_router()`/`serve()` loopback pattern and the embedded-fixture
  `include_str!` idiom exist in `openteam-mock`; `axum 0.8` is
  workspace-pinned, **default features only** (SSE included — no changes).
- `EventsWriter::append` already flushes per event (ADR 0022), so a same-host
  tailer sees events ~instantly — no writer change needed beyond step 1.
- Conventions carry over unchanged (ADR 0013): edition 2024, no `mod.rs`,
  `thiserror` in libs, the unwrap/print lints, `clippy -D warnings`.

## Build order (keep each step green before the next)

Each step: implement → `cargo test -p <crate>` → `cargo clippy --all-targets --
-D warnings` → `cargo fmt`. Commit each green step. Steps 1–2 are independent
of each other; everything else is sequential as listed.

### Step 1 — the three granted writer/core changes

*Scope*: the **only** permitted changes outside the new crate, pinned in §9:
(a) `RunSummary` gains `#[derive(Serialize)]` — no renames/skips (ADR 0030);
(b) public owned board-snapshot construction — promote `BoardSnapshot`
(`crates/openteam-core/src/artifacts.rs`) to a public owned type **or** add a
public serializer (implementer's choice), output matching `board.json`
semantics with pinned key order `run_id, goal, seed, tasks, teams`;
(c) `openteam run` creates `<run-dir>/run.lock` at run start and holds an
exclusive advisory `flock` for the run's lifetime (ADR 0027; the file carries
no data, no event-schema change).

*Accepted when*: existing suite still green; a unit test asserts
`serde_json::to_value(RunSummary)` matches §9's field/tuple shapes; a unit
test asserts the public board snapshot serializes byte-equal to what
`board.json` finalize writes; an e2e-adjacent assertion on an existing
`--mock` happy-path run shows `run.lock` exists in the run dir.

### Step 2 — `openteam-serve` skeleton + `ServeConfig`

*Scope*: new library crate `openteam-serve` in `[workspace]` +
`[workspace.dependencies]` (deps: `openteam-core`, `axum`, `tokio`,
`serde`/`serde_json`; `openteam-wire` only if naming warrants — ADR 0030
"Cargo hygiene"). Constructor-injectable
`ServeConfig { poll_interval, keep_alive, retry_ms, broadcast_capacity }`
with defaults **100 ms / 15 s / 2000 ms / 1024** (§9); stub
`build_router(...)`/`serve() -> (SocketAddr, ShutdownHandle)` on the ADR 0019
pattern.

*Accepted when*: workspace builds; `serve()` binds loopback and shuts down
cleanly in a smoke test; config is test-overridable (~5 ms poll) without any
CLI surface.

### Step 3 — discovery, run-state classifier, tailer

*Scope*: discovery of run dirs under one root; the **finished/live/aborted**
classifier (bookend × flock trichotomy, ADR 0027); the poll tailer obeying
the **complete-line rule** (torn final line left unconsumed, re-read next
poll — §9).

*Accepted when* (unit, in-crate; the **test is the writer** — same-process
flock conflict per ADR 0030): classifier returns all three states from
hand-built tempdirs; torn-line test (partial line not emitted within N poll
*cycles*, then completed line arrives); abort test (lock dropped, no bookend
→ classified aborted). Negative assertions bounded in poll cycles, never
wall-clock sleeps.

### Step 4 — the fold + wire types

*Scope*: the `fold` module — events → board state (driving core's public
`Board` mutators + step 1(b)'s snapshot), **four-state agents**
(`idle | working{task} | asleep | parked`, defining transitions pinned in
ADR 0028; `MeterState` untouched), metrics via `Metrics::fold`/`RunSummary`
(ADR 0020's fourth view). The wire-facing types (snapshot envelope, run-list
entry, agent-state, `run_state` control frame) in a schema module beside
their only producer (ADR 0030) — serialized shapes exactly per §9.

*Accepted when* (unit): four-state transition table covered including the
`agent_parked` / `agent_woke`-restores cases; **folded snapshot ≡
`board.json`** holds on a hand-built finished log; fold of an empty/partial
log yields a coherent live snapshot with correct `as_of`.

### Step 5 — router + list/snapshot endpoints

*Scope*: real `build_router()` mounting `Router::nest("/v1", ...)` (ADR 0029):
`GET /v1/runs` (JSON array, UUIDv7-ascending, cheap fields only, `finished`
block only when finished — §9) and `GET /v1/runs/{run_id}/snapshot`
(`{ as_of, run, board, agents, metrics }`; 404 unknown run).

*Accepted when* (integration, serve crate `tests/`, real loopback): list and
snapshot JSON are **value-golden** against the frozen fixture (step 8 lands
the fixture; until then, hand-built tempdir logs); 404s; snapshot of a live
and an aborted tempdir run works (no `board.json` present — the fold is the
only path, ADR 0028's grounding fact).

### Step 6 — the SSE stream endpoint

*Scope*: `GET /v1/runs/{run_id}/events` — one SSE event per line, `id:` =
decimal `EventId`, `data:` = **verbatim line bytes**, no `event:` field;
resume arithmetic (`Last-Event-ID: n` / `?from=n` → replay from n+1; absent →
from 0; non-u64 → **400**); **204** on caught-up connect to any terminal run;
the id-less `event: run_state` / `data: {"state":"aborted"}` control frame
then stream end; per-live-run bounded `tokio::sync::broadcast`
(subscribe-first → file catch-up → dedupe by `EventId`), **disconnect on
lag**; stream headers, keep-alive, `retry:` per §9.

*Accepted when* (integration): SSE `data:` payloads **byte-golden** vs the
fixture's `events.jsonl` lines; framing asserted semantics-only (ADR 0030);
resume from-0 / from-n+1 / `?from=` equivalence; 400; 204 for finished *and*
aborted; abort control frame observed id-less then EOF; disconnect-on-lag
provoked via a tiny test `broadcast_capacity`.

### Step 7 — the debug page

*Scope*: one static HTML file, `include_str!`, served at `GET /` outside
`/v1/` — run list, click-to-select, verbatim pretty-printed snapshot, bare
`EventSource` tail + named `run_state` listener. Hard line (ADR 0029): the
page **never interprets, folds, or styles domain data — it only tails and
dumps**. Non-contract surface.

*Accepted when*: the single permitted assertion — `GET /` returns 200
`text/html` (smoke, integration tier). Manual browser check is worthwhile but
not CI.

### Step 8 — CLI wiring + fixtures + e2e

*Scope*: (a) the bin gains `openteam-serve = { workspace = true }` and the
`Serve` clap variant — surface exactly `serve --dir <runs-root>` (default
`.openteam/runs`) `--port <PORT>` (`0` = ephemeral), global `-v/-vv/--quiet`;
on bind print exactly ``openteam serve listening on http://<addr>`` to stdout
(§9). (b) The **frozen fixture run dir** (`events.jsonl` + `board.json`,
captured once from a `--mock` run) under the serve crate's `tests/fixtures/`;
retrofit steps 5–6's goldens onto it. (c) The thin e2e (bin `tests/`):
`drive()` a `--mock` run, spawn `serve --dir <tempdir> --port 0`, parse the
printed address, hit list + snapshot + stream-to-204, kill the child. (d) The
equivalence test's second tier: fold every fresh `--mock` run dir the existing
happy-path e2e tests produce and assert folded snapshot ≡ `board.json`
(library call, no server). **No live-run e2e** — rejected-for-now, ADR 0030.

*Accepted when*: all four land; `mise run ci` runs the new unit + integration
+ e2e tiers green alongside the existing suite.

## Done when

`openteam run "goal" --mock` then `openteam serve --port 0` against the same
directory serves the run list, a correct snapshot, and a byte-verbatim SSE
replay ending in 204; the debug page tails a live tempdir run in a browser;
and **`mise run ci` is green** with the new tiers included.

## Recommended skills

**`/tdd`** for steps 3–6 (the pinned invariants are all test-shaped);
**`/verify`** after steps 6 and 8 (drive the real server, not just tests);
**`/code-review`** per step against its ADR. The per-ticket resolution
comments on map #36 hold the reasoning if a "why" is unclear.
