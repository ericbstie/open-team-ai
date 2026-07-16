# Rust crate inventory for the pinned stack

Research for [#6](https://github.com/ericbstie/open-team-ai/issues/6). Verified 2026-07-16
against crates.io (live `cargo add --dry-run` / `cargo info` resolution from a scratch
project on the local Rust 1.94.1 toolchain), docs.rs, and official changelogs. Every MSRV
below clears Rust 1.94.

## Workspace conventions

- **Edition 2024** everywhere — stabilized in Rust 1.85.0 ([release post](https://blog.rust-lang.org/2025/02/20/Rust-1.85.0/): "This stabilizes the 2024 edition as well"). Set `rust-version = "1.94"` in `[workspace.package]`.
- Edition 2024 defaults to the **rust-version-aware resolver** (resolver "3") and changes how `default-features = false` interacts with inherited workspace dependencies — so declare feature sets once, in `[workspace.dependencies]`, and have member crates use `tokio = { workspace = true }`.
- Several current crates (clap 4.6, rand 0.10, uuid 1.24, assert_cmd 2.2) themselves now require **MSRV 1.85** / edition 2024 — fine for us, but it means this stack cannot be built on pre-2025 toolchains.

## Recommended dependency table

Versions are the exact latest resolutions on 2026-07-16; pin at the minor in `[workspace.dependencies]` (e.g. `tokio = "1.52"`).

| Crate | Version | Features | Role | Alternative considered |
|---|---|---|---|---|
| `tokio` | 1.52.4 | `rt-multi-thread`, `macros`, `sync`, `time`, `net`, `signal` | Async runtime; `sync` for channels, `net` for the mock's listener | `full` (fine for a bin, but curated features keep lib builds honest) |
| `axum` | 0.8.9 | defaults (`json`, `http1`, `tokio`, …) | Mock OpenAI-schema server + standalone serve mode | none — pinned by ADR-0001 |
| `clap` | 4.6.2 | `derive`, `env` | CLI (`env` for `OPENTEAM_LLM_API_KEY`-style vars) | none — pinned |
| `serde` | 1.0.228 | `derive` | All wire/artifact serialization | none — pinned |
| `serde_json` | 1.0.150 | — | OpenAI wire schema, tool-call args, event log | none |
| `tracing` | 0.1.44 | — | Instrumentation in lib crates | none — pinned |
| `tracing-subscriber` | 0.3.23 | `env-filter`, `fmt` | Subscriber in the bin; `RUST_LOG` filtering | none |
| `rand` | 0.10.2 | defaults | RNG traits + distributions | — |
| `rand_chacha` | 0.10.0 | — | The Seed: `ChaCha8Rng`, portable + reproducible | `StdRng` (output not contractually stable across rand majors) |
| `uuid` | 1.24.0 | `serde` | Run/agent/task ids, built from the run RNG (see below) | `ulid` 3.0.0 (time-prefixed → worse for determinism) |
| `jiff` | 0.2.32 | `serde` | Event-log timestamps (`Timestamp`), behind a `Clock` seam | `chrono` 0.4.45 (incumbent; either works — pick one) |
| `thiserror` | 2.0.18 | — | Typed errors in lib crates (verbs, board, store) | — |
| `anyhow` | 1.0.103 | — | Context-chained errors in the bin only | — |
| `async-trait` | 0.1.89 | — | Dyn-compatible async traits (`LlmClient`, knowledge store) | native AFIT (not dyn-compatible); `dynosaur` 0.3.1 (pre-1.0) |
| `schemars` | 1.2.1 | `derive` (default) | JSON Schema for coordination-verb tool definitions | hand-rolled `serde_json::json!` (viable; see below) |
| `tempfile` (dev) | 3.27.0 | — | Isolated run-artifacts dirs in tests | — |
| `assert_cmd` (dev) | 2.2.2 | — | Drive the `openteam` bin in e2e tests | — |
| `predicates` (dev) | 3.1.4 | — | Assert on stdout / report contents | — |
| *(none)* — hand-rolled | — | — | In-process cosine similarity for the knowledge store | `ndarray` 0.17.2 — rejected: one ~10-line f32 loop doesn't justify the dep tree |

## Async traits on Rust 1.94: AFIT vs `async-trait`

- Native `async fn` in traits (AFIT) has been stable since Rust 1.75, but per the
  [stabilization post](https://blog.rust-lang.org/2023/12/21/async-fn-rpit-in-traits/):
  "Traits that use `-> impl Trait` and `async fn` are not object-safe, which means they
  lack support for dynamic dispatch." That is unchanged on 1.94: **no `Box<dyn Trait>`
  over a trait with native `async fn`**.
- The `LlmClient` trait (mock in-process vs real endpoint via `--llm-base-url`) and the
  knowledge-store trait want runtime-swappable implementations — i.e. `dyn` dispatch.
  **Recommendation: `#[async_trait]` (async-trait 0.1.89) on exactly these
  dyn-dispatched seams.** Cost is one box per call — noise next to an HTTP round trip or
  an embedding computation.
- Where static dispatch suffices (generics over the behavior model, the RNG, the clock),
  prefer native AFIT. If a spawned task needs the future to be `Send`, the official
  answer is `trait-variant` 0.1.2 (`trait_variant::make`) rather than hand-written
  bounds.
- `dynosaur` 0.3.1 (rust-lang AWG crate, MSRV 1.84) generates dyn-compatible wrappers
  for AFIT traits — the likely eventual replacement for async-trait, but pre-1.0; not
  for the v1 pin.
- Tool registry note: the registry itself is a fixed map per role and needs no async
  trait at all — dispatch verbs by name to plain async fns; only the store/LLM seams
  need `dyn`.

## rand 0.10: renames to watch (sharp edge for pre-2026 examples)

Per the [rand CHANGELOG](https://github.com/rust-random/rand/blob/master/CHANGELOG.md),
0.10 (MSRV 1.85, edition 2024) renamed heavily versus 0.9:

- trait `Rng` → `RngExt` (because `rand_core::RngCore` became `Rng`)
- `thread_rng()` → `rand::rng()`; `Rng::gen` → `random` (2024's `gen` keyword)
- `rand::distributions` → `rand::distr`; `Standard` → `StandardUniform`
- `OsRng` → `SysRng`; feature `getrandom` → `os_rng`; `small_rng` feature removed
- `StdRng` is now backed by the `chacha20` crate (output identical today), but its
  algorithm is still not contractually stable across rand majors.

**Determinism:** seed `ChaCha8Rng::seed_from_u64(seed)` (rand_chacha docs: generators
are "all deterministic and portable … with testing against reference vectors") and
derive per-agent/per-turn streams from the run Seed. The `ulid` crate's optional `rand`
feature is already aligned to rand ^0.10, so no version split if ulid is ever added.

## Id generation: uuid, deterministically

`uuid::Builder::from_random_bytes(bytes)` sets only the v4 version/variant bits, requires
**no feature flags**, and is explicitly designed for caller-supplied randomness
([docs](https://docs.rs/uuid/latest/uuid/struct.Builder.html)) — so ids come straight off
the run RNG: `Builder::from_random_bytes(rng.random()).into_uuid()`. Avoid the `v4`
feature's `Uuid::new_v4()` inside runs (OS randomness breaks the Seed). ULID's first 48
bits encode wall-clock milliseconds; deterministic tests would need `Ulid::from_parts`
plus a mock clock — an extra moving part for sortability we can get from the event log
ordering instead.

## tokio broadcast: lagging-receiver semantics

From the [broadcast docs](https://docs.rs/tokio/latest/tokio/sync/broadcast/index.html):

- The channel is a ring buffer: "If a value is sent when the channel is at capacity, the
  oldest value currently held by the channel is released." A receiver that missed it gets
  `RecvError::Lagged(n)` on its next `recv`, and its cursor jumps to the oldest retained
  value — **skipped messages are silently gone**.
- `Sender::send` returns `Err(SendError(msg))` when all receivers have been dropped; the
  value is returned, not stored. Subscribers only see values sent **after** `subscribe`.

Implication for the Message invariant ("always ingested into the knowledge store"):
broadcast alone cannot be the delivery mechanism for agent messages. Write to the
knowledge store / event log first, then notify — per-agent `mpsc` mailboxes with explicit
fan-out for team and broadcast scopes. If broadcast is used for the event stream, treat
`Lagged` as a loud bug event and size capacity generously.

## axum 0.8: extractor and routing changes

Per the [0.8 announcement](https://tokio.rs/blog/2025-01-01-announcing-axum-0-8-0)
(registry MSRV for 0.8.9: 1.80):

- Path params are `/{id}`, wildcards `/{*rest}` — the old `/:id` / `/*rest` syntax is
  gone; literal braces escape as `{{`/`}}`. Pre-0.8 snippets will 404.
- Custom `FromRequestParts` / `FromRequest` impls must **drop `#[async_trait]`** — axum
  now uses native return-position `impl Trait` in traits.
- `Option<T>` as an extractor now requires `OptionalFromRequestParts` instead of
  silently mapping rejections to `None`.

The mock's surface (a handful of `POST /v1/...` routes with `Json` extractors) touches
none of the sharp parts — but the syntax change matters for any copied examples.

## Error-handling split

Current consensus is unchanged: **thiserror 2.0.18 in library crates** (typed, matchable
errors so verb handlers and the board can branch on failure kinds) and **anyhow 1.0.103
in the binary only** (context chains for the final report / exit path). thiserror 2.x
has been the stable line since late 2024; both MSRVs (1.68) are ancient history for us.

## JSON Schema for tool definitions

[schemars 1.x](https://docs.rs/schemars/latest/schemars/) generates **JSON Schema
2020-12 by default**, with `SchemaSettings::draft07()` / `openapi3()` available if the
mock or a real endpoint wants an older dialect. It derives from serde attributes —
`#[serde(deny_unknown_fields)]` becomes `additionalProperties: false`, which strict
OpenAI-style tool schemas want — keeping each verb's schema and its arg deserializer in
lockstep from one struct. Since the tool registry is a small fixed set of coordination
verbs, hand-rolled `serde_json::json!` blocks are an acceptable zero-dep fallback, but
the lockstep property is why schemars is the recommendation.

## CLI e2e testing

`assert_cmd` 2.2.2 + `predicates` 3.1.4 + `tempfile` 3.27.0 is the standard trio:
assert_cmd runs the `openteam` bin against a scenario fixture, tempfile isolates the
run-artifacts directory, predicates asserts on the report and event log. Note
assert_cmd's MSRV is now 1.85.

## Sources

- crates.io registry via `cargo add --dry-run` / `cargo info`, 2026-07-16, Rust 1.94.1
- <https://github.com/rust-random/rand/blob/master/CHANGELOG.md>
- <https://docs.rs/rand_chacha/latest/rand_chacha/>
- <https://docs.rs/tokio/latest/tokio/sync/broadcast/index.html>
- <https://docs.rs/tokio/latest/tokio/sync/broadcast/struct.Sender.html>
- <https://tokio.rs/blog/2025-01-01-announcing-axum-0-8-0>
- <https://blog.rust-lang.org/2023/12/21/async-fn-rpit-in-traits/>
- <https://blog.rust-lang.org/2025/02/20/Rust-1.85.0/>
- <https://docs.rs/schemars/latest/schemars/>
- <https://docs.rs/uuid/latest/uuid/struct.Builder.html>
- <https://docs.rs/ulid/latest/ulid/struct.Ulid.html>
