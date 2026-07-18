# open-team-ai

`openteam` — an LLM harness for parallelized agentic team working, written in
idiomatic Rust, toolchain managed by [mise](https://mise.jdx.dev/). LLM traffic
speaks the OpenAI wire schema and, by default, targets a real OpenAI-compatible
endpoint (`https://api.openai.com/v1`); a deterministic built-in mock (`--mock`)
keeps the whole system runnable offline and drives the test suite (ADR 0026).

## Build & run

- `mise run build` / `mise run test` / `mise run lint` / `mise run fmt` — canonical tasks (see `mise.toml`).
- `cargo run -p openteam -- run "your prompt"` — run the harness against a real OpenAI-compatible endpoint. Needs `OPENAI_API_KEY` (or `OPENTEAM_LLM_API_KEY`); pick the model with `--model` / `--embedding-model` (default `gpt-4o-mini` / `text-embedding-3-small`), or point elsewhere with `--llm-base-url` (its full path prefix is used, so `https://host/api/` reaches Open WebUI; add `--local-embeddings` for endpoints without an `/embeddings` route). Picks a random seed each run (printed as `run seed: <n>` on stderr); pass `--seed <n>` to reproduce. The report prints to stdout (== `report.md`), tracing to stderr; `-v/-vv` raise verbosity, `--quiet` silences tracing. See ADR 0024 + ADR 0026 for the full flag surface.
- `cargo run -p openteam -- run "your prompt" --mock` — run fully offline against the deterministic built-in mock (no network, no key; what the test suite uses). `--scenario` requires `--mock`.
- `cargo run -p openteam -- tui` — launch the simplified, Claude-Code-style TUI: type a goal, watch the run's `events.jsonl` stream in as a live activity feed, and read the report inline (Esc/Ctrl-C quits, Ctrl-L clears, arrows/PgUp-PgDn scroll). Optional `--agents N`, `--meta-agents N`, `--seed U64`. Each submitted goal runs offline against a fresh in-process mock.
- `cargo run -p openteam -- mock serve --port 0` — run the standalone OpenAI-schema mock over loopback (`--port 0` = ephemeral).
- Plain `cargo build --workspace` also works if mise isn't set up.

## Status & spec

The **architecture is fully specified and the build is complete** — all four
crates are implemented and `mise run ci` is green (fmt check, clippy `-D warnings`,
the full unit/e2e/contract/pairing suite). The complete, validated spec is:
`CONTEXT.md` (glossary), `docs/adr/0001`–`0025` (every decision, some with dated
"Amended by the #22 dry-run gate" notes that are canonical),
`docs/implementation-pins.md` (code-level details the ADRs left open, pinned so the
renderer and parser halves stay in lockstep — the ADRs win on any conflict), and
`docs/prototypes/dry-run-transcript.md` (a hand-traced canonical run that validated
the whole protocol end-to-end). GitHub issue #2 indexes the map (21/21 tickets
closed). `docs/IMPLEMENTATION-PLAN.md` records the crate-by-crate build order the
implementation followed (wire → core + mock → bin).

## Agent skills

### Issue tracker

Issues live in this repo's GitHub Issues (`ericbstie/open-team-ai`), driven via
the GitHub MCP tools — **there is no `gh` CLI in this environment**. See
`docs/agents/issue-tracker.md` for the exact tool calls, including the
wayfinding operations (map / child tickets / blocking / frontier / claim /
resolve).

### Triage labels

Default five-role vocabulary (`needs-triage`, `needs-info`, `ready-for-agent`,
`ready-for-human`, `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `CONTEXT.md` at the repo root plus ADRs under `docs/adr/`,
created lazily by `/domain-modeling`. See `docs/agents/domain.md`.
