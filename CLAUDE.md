# open-team-ai

`openteam` — an LLM harness for parallelized agentic team working, written in
idiomatic Rust, toolchain managed by [mise](https://mise.jdx.dev/). LLM traffic
targets OpenAI-schema endpoints served by a built-in mock, so the whole system
runs offline.

## Build & run

- `mise run build` / `mise run test` / `mise run lint` / `mise run fmt` — canonical tasks (see `mise.toml`).
- `cargo run -p openteam -- run "your prompt"` — run the harness against the mock LLM. Picks a random seed each run (printed as `run seed: <n>` on stderr); pass `--seed <n>` to reproduce. The report prints to stdout (== `report.md`), tracing to stderr; `-v/-vv` raise verbosity, `--quiet` silences tracing. See ADR 0024 for the full flag surface.
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
