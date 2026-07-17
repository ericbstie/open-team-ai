# openteam

An LLM harness for parallelized agentic team working.

```sh
mise install   # pins Rust 1.94 — any recent rustup toolchain works too

export OPENAI_API_KEY=sk-...
cargo run -p openteam -- run "Design a rate limiter for a public API"
```

No key? The built-in deterministic mock runs the whole system offline:

```sh
cargo run -p openteam -- run "Design a rate limiter for a public API" --mock
```

The report prints to stdout, tracing to stderr:

```
# Design a rate limiter for a public API

## Completed work
- Summarize the public notes
- Research the rate plan
- Summarize the design notes
- Draft the rate overview

...

## Run summary
- Outcome: CleanFinish (exit 0)
- Duration: 0.20s wall · 12 ticks
- Agents: 4 team + 1 meta · specialties used: generalist
- Tasks: created 4 · completed 4 · cancelled 0
- Effective parallelism: 4 → 3 (meta set_parallelism)
- Tokens: 20.0k total — orchestrator 5.8k, agent-1 2.6k, ...
- Meta interventions: issued 2 · fulfilled 1 · declined 1
```

Artifacts land in `.openteam/runs/<run-id>/`: `report.md`, `board.json`,
`events.jsonl`, `knowledge.jsonl`.

## Reproduce a run

Every run logs its seed to stderr (`run seed: 16213438880691117477`). Feed it
back for an identical run:

```sh
cargo run -p openteam -- run "Design a rate limiter for a public API" --mock --seed 16213438880691117477
```

## Shape the team

```sh
cargo run -p openteam -- run "..." --agents 8 --parallel 4 --model gpt-4o --max-llm-calls 200
```

`cargo run -p openteam -- run --help` lists the rest: caps, meta-agents,
other OpenAI-compatible endpoints (`--llm-base-url`), scenario fixtures.

## Standalone mock server

```sh
cargo run -p openteam -- mock serve --port 8080
# openteam mock listening on http://127.0.0.1:8080

curl -s http://127.0.0.1:8080/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"openteam-mock","messages":[{"role":"user","content":"hello"}]}'
# {"id":"chatcmpl-0-anon-0","object":"chat.completion", ... "content":"Understood; proceeding." ...}
```

Serves `/v1/chat/completions` and `/v1/embeddings` over loopback.

## Develop

```sh
mise run test   # unit / e2e / contract / pairing suites
mise run ci     # fmt check + clippy -D warnings + test
```

The spec lives in-repo: `CONTEXT.md` (glossary), `docs/adr/` (every decision),
`docs/implementation-pins.md`, and `docs/prototypes/dry-run-transcript.md`
(a hand-traced canonical run).
