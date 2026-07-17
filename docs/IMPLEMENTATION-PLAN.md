# Implementation plan — building `openteam`

The **map is complete**: every architectural decision is pinned in `CONTEXT.md`
(the glossary), `docs/adr/0001`–`0025`, and validated end-to-end by the hand-traced
`docs/prototypes/dry-run-transcript.md`. GitHub issue #2 indexes the whole map.
This document is the build order — it does **not** re-decide anything; where a
detail is unclear, the ADR wins.

> **Golden rule:** the ADRs and the dry-run transcript are the spec. Read the ADRs
> for the crate you are building *before* writing code. Match the pinned type,
> trait, event, and wire signatures **exactly** — later crates depend on them.

## Ground truth already in place

- 4-crate **virtual workspace** builds clean today (stubs): `openteam-wire` (leaf
  contract) → `openteam-core` (domain + runtime) + `openteam-mock` (server +
  behavior) → `openteam` (bin). See ADR 0013 for boundaries and the forbidden
  edges (`openteam-mock` depends on `openteam-wire` **only**).
- Root `Cargo.toml` `[workspace.dependencies]` + `[workspace.lints]` resolve;
  `rand`/`rand_chacha` are pinned at `0.10`. `mise.toml` has `build`/`test`/
  `lint`/`fmt`/`ci`. `Cargo.lock` is committed.
- Conventions (ADR 0013): edition 2024, rust 1.94; no `mod.rs` (module files +
  `pub use` at crate root); `thiserror` per-subsystem error enums in libs,
  `anyhow` in the bin only; `#[async_trait]` on the dyn seams (`LlmClient`,
  `VectorStore`) only, plain dispatch elsewhere; `unsafe` forbidden; the
  unwrap/expect/panic/dbg/print lints warn in libs (the bin owns stdout for the
  report). `mise run ci` = fmt check + `clippy -D warnings` + all tests.

## Build order (bottom-up; keep each phase green before the next)

Each phase: implement → `cargo build -p <crate>` → `cargo test -p <crate>` →
`cargo clippy -p <crate> --all-targets -- -D warnings` → `cargo fmt`. **Commit and
push** the green crate before starting the next (branch
`claude/agentic-team-harness-0jlb0p`).

### Phase 1 — `openteam-wire` (leaf contract)

ADRs **0018** (wire types + serde posture + `TokenCounter` + `Seed` +
`WireIdentity` + `X-OpenTeam-*` headers), **0012** (`AgentId` handle + the
`user`-field grammar parse/render + slug validation), **0008** (identity channels;
the aux header is a per-agent **call-sequence** counter), **0014** (the base64
f32-LE embedding type/codec), and `docs/research/openai-wire-subset.md` (the exact
JSON shapes). Unit targets: ADR 0025 (serde round-trips incl. response nullable
keys → explicit `null`; embeddings-request `deny_unknown_fields`; base64 f32-LE
round-trip; grammar parse↔render; `TokenCounter` + usage free-fns; slug).

### Phase 2a — `openteam-core` (domain + runtime) — depends on wire

The big crate. ADRs **0010** (Task/board), **0011** (Message/Router/mailboxes;
**four independent contiguous id counters** on one serial write path),
**0014** (`VectorStore` speaks *text*; in-memory cosine; the `Embedder` seam),
**0015** (`AgentState`; the capped-inner-loop turn; scheduler + `--parallel`
semaphore + liveness watchdog; K=3 park), **0016** (the data `ContextPolicy` +
the *pinned section line-grammars* — the mock parses these, so render them
exactly), **0017** (tool registry: `ok|rejected|invalid` outcome envelope;
dispatch-by-name; the verb tables), **0018** (`LlmClient` `#[async_trait]` +
the reqwest adapter + the per-agent `AgentChannel` owning the `AtomicU64`
call-seq + `LlmConfig`), **0020** (runtime-owned `Metrics` fold → 3 projections;
the 6 typed directive verbs; explicit-cite correlation), **0022** (the `Event`
envelope + the ~27-kind taxonomy + the `.openteam/runs/<uuidv7>/` artifacts +
the `Clock` seam). Trait seams get an in-memory fake for unit tests (ADR 0025).

### Phase 2b — `openteam-mock` (server + behavior) — depends on wire only

Can run in parallel with 2a (shares no code with core — only the wire contract).
ADRs **0019** (real-loopback axum server; stateless per request; the sync
`BehaviorModel::chat(req,id) -> ChatDecision` seam; the server owns the
schema-valid envelope; `build_router()` + `serve()`), **0021** (the built-in
behavior arc: pure fn of (request, identity, seed); reads the rendered world;
per-completion RNG `ChaCha8Rng::from_seed(hash(seed‖user‖call_seq))`; the
decompose→work→converge arc that terminates by construction; the **≤1 directive
per tier** meta bound; the three within-turn arc rules), **0023** (the scenario
player = 2nd `BehaviorModel`, `(agent-selector, call_seq)`-indexed JSON fixtures +
the ten-fixture library), **0025** (the FNV-1a-64 → `seed_from_u64` derivation,
in this crate; the `async-openai` contract-test oracle). The section line-grammars
it parses are pinned in ADR 0016 — the mock and core are two halves of one
contract.

### Phase 3 — `openteam` (bin) — depends on all

ADR **0024** (the exact clap surface: `openteam run "<goal>"` + `openteam mock
serve`; the flags/defaults/exit codes; seed random-per-run logged; `--quiet`
keeps stdout == `report.md`). Composition root: parse CLI → start the in-process
mock (unless `--llm-base-url`) → wire up core against it → run the orchestrator →
persist artifacts → print the report. Then the **e2e + contract + pairing tests**
(ADR 0025): `assert_cmd` CLI tests, the ten scenario fixtures each → its named
`events.jsonl` invariant, the `async-openai` schema oracle, the legibility-pairing
test in the bin's `tests/`, the fixed seed sweep.

## Done when

`cargo run -p openteam -- run "write an onboarding guide"` produces a real report
on stdout + a populated `.openteam/runs/<id>/` directory, and **`mise run ci` is
green** (fmt + clippy -D warnings + all unit/e2e/contract tests). The dry-run
transcript is the reference for what a run should look like.

## Recommended skills (the vendored engineering skills)

- **`/implement`** — drive each crate/phase from its ADRs + this plan.
- **`/tdd`** — for the pinned invariants (board conservation, termination,
  degradation-forces-completion, the arc bounds), write the test first.
- **`/verify`** — after each phase, exercise the real behavior (run the crate's
  tests; for the bin, actually run `openteam run` and inspect the artifacts) —
  don't rely on the type-checker alone.
- **`/code-review`** — review each phase's diff against its ADRs (the Spec axis)
  and the repo conventions (the Standards axis) before moving on.
- **`/resolving-merge-conflicts`**, **`/diagnosing-bugs`** — as needed.

Issue #2 and the per-ticket resolution comments hold the reasoning behind every
decision if a "why" is ever unclear.
