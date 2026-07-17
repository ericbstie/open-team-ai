# Real OpenAI-compatible endpoints are the default; the mock is a test/offline backend behind `--mock`

This ADR reverses the default posture of ADR 0001. `openteam run` now targets a
**real OpenAI-compatible endpoint** — `https://api.openai.com/v1` unless
`--llm-base-url` overrides it — and reaches the deterministic in-process mock
**only under `--mock`**. The mock is no longer the default backend; it is what
the test suite and offline local runs select explicitly. Nothing else about the
transport changes: the default path reuses the **existing** OpenAI-schema module
(`openteam-wire` types + the `ReqwestLlmClient` reqwest adapter, ADR 0018), which
ADR 0019 already exercises byte-identically over real loopback, so pointing it at
a real endpoint adds no new client code (DRY). `async-openai` stays a
`openteam-mock` dev-dep contract oracle (ADR 0025), never a runtime dependency.

## The surface (amends ADR 0024)

`run` gains four flags and one fallback; the mock's selection flips from
"absence of `--llm-base-url`" to an explicit opt-in:

- `--mock` — run against the built-in deterministic mock (in-process, seeded, no
  external network — real loopback HTTP only, ADR 0019). **Conflicts with
  `--llm-base-url`.** This is what the e2e/pairing suites pass and how a user runs
  fully offline.
- `--llm-base-url <URL>` — override the real endpoint (any OpenAI-compatible
  server that exposes the `/v1/...` routes: a proxy, a local model server).
  Absent ⇒ the OpenAI default. The reqwest adapter joins absolute `/v1/...`
  paths, so a base carrying its own path prefix (e.g. Azure-style
  `/openai/deployments/...`) is out of scope here.
- `--model <ID>` (`OPENTEAM_MODEL`) — chat model; default `gpt-4o-mini` on the
  real path, `openteam-mock` under `--mock`.
- `--embedding-model <ID>` (`OPENTEAM_EMBEDDING_MODEL`) — embedding model;
  default `text-embedding-3-small` on the real path, `openteam-mock` under
  `--mock`.
- API key resolves from `--llm-api-key` / `OPENTEAM_LLM_API_KEY` (clap `env`,
  unchanged) **then** the conventional `OPENAI_API_KEY`, so a standard OpenAI
  environment works with no extra config.
- `--scenario` now **requires `--mock`** — only the mock consumes scenarios
  (ADR 0023); it was silently ignored against an external endpoint before.

Two new validation-phase (exit 2, no artifacts — ADR 0006/0024) checks, both
resolved in the bin:

- `--mock` with `--llm-base-url`, and `--scenario` without `--mock`, are clap
  conflicts.
- The default OpenAI endpoint with no resolved key fails fast with a pointer to
  the three fixes (set a key / set `--llm-base-url` / use `--mock`), rather than
  surfacing a bare `401` mid-run. A custom `--llm-base-url` may legitimately need
  no key (local servers), so the guard covers only the default.

## Consequences

The seed-independence and cosine-ranking invariants of ADR 0014 / ADR 0025 Tier 1
hold only for mock embeddings; the real path returns genuine semantic vectors, so
determinism is a mock property, selected with `--mock`, not a default guarantee.
Provider hardening — retries, rate limits, streaming, cost tracking — remains out
of scope (ADR 0001's exclusion still holds); making real the default does not by
itself buy those, and YAGNI keeps them out until asked for. The `openteam-mock`
crate stays a normal dependency of the bin: `openteam mock serve` and `--mock`
both embed it, and ADR 0013's dependency direction is untouched.

## Rejected alternatives

- **Keep the mock as default, add `--real`.** Inverts the user's ask; leaves the
  offline mock as the thing every run accidentally hits.
- **Adopt `async-openai` as the runtime client.** Would fork the wire types the
  mock and harness already share (ADR 0013), violating DRY, and drag a full HTTP
  client into production deps against ADR 0025's dev-only pin — for no capability
  the existing `ReqwestLlmClient` lacks.
- **A config file.** Still out of scope (ADR 0024); flags + env only.
