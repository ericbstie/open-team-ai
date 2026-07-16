# open-team-ai

`openteam` — an LLM harness for parallelized agentic team working, written in
idiomatic Rust, toolchain managed by [mise](https://mise.jdx.dev/). LLM traffic
targets OpenAI-schema endpoints served by a built-in mock, so the whole system
runs offline.

## Build & run

- `mise run build` / `mise run test` / `mise run lint` / `mise run fmt` — canonical tasks (see `mise.toml`).
- `cargo run -p openteam -- run "your prompt"` — run the harness against the mock LLM.
- Plain `cargo build --workspace` also works if mise isn't set up.

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
