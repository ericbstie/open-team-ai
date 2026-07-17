# Wire types are one shared contract; the LLM client is a stateless transport behind per-agent channels

The OpenAI wire subset (chat completions with tool calling, embeddings, the
error body) is **one set of Rust types in `openteam-wire`**, each deriving both
`Serialize` and `Deserialize` because the harness and the mock sit on opposite
ends of the same type — the harness serializes a request the mock deserializes,
the mock serializes a response the harness deserializes. Two serde idioms carry
the spec's nullable distinction: request optional params are
`Option<T>` + `#[serde(skip_serializing_if = "Option::is_none")]` (**omit when
absent**), while response required-but-nullable keys (`content`, `refusal`, the
per-choice `logprobs`) are plain `Option<T>` with **no** skip, so `None`
serializes as an explicit `null` — the shape every OpenAI SDK expects. Unknown-field
posture is **asymmetric, tracking the spec**: the embeddings request derives
`#[serde(deny_unknown_fields)]` (spec `additionalProperties: false` → the mock
400s on stray fields), while the chat request and all responses accept-and-ignore
unknowns for forward-compat. `function.arguments` is typed as a `String`
(JSON-encoded, never parsed at the wire layer); input message `content` is
`#[serde(untagged)] enum MessageContent { Text(String), Parts(Vec<serde_json::Value>) }`
so the mock stays a faithful general OpenAI server that won't reject a real
client's content-part array — the harness only ever builds `Text`, and the mock
never reads content anyway (determinism keys on identity, ADR 0008). Embeddings
ride base64 by default: the client requests `encoding_format: "base64"` (matching
the openai-python reference client, so every offline run exercises ADR 0014's
committed base64-f32-LE path), the response `embedding` is
`#[serde(untagged)] enum EmbeddingVector { Base64(String), Float(Vec<f32>) }`, and
the f32-LE codec lives in `openteam-wire` (adds the `base64` crate, a #15-added
dep like #10's `reqwest`).

The `LlmClient` trait lives in `openteam-core` (not wire — wire is data only,
ADR 0013), is `#[async_trait]` + `Send + Sync` (dyn-dispatched, per the crate
inventory), and is **transport-agnostic**: `complete(&self, id: &WireIdentity,
req: &ChatCompletionRequest)` and `embed(&self, req: &EmbeddingRequest)`, both
returning a `LlmError { Http { status, error: wire::ApiError }, Transport(String),
Malformed(String) }` that carries no reqwest types, so the two adapters — the
default **reqwest HTTP client** and an **in-memory fake for runtime unit tests** —
both satisfy it cleanly. Because the mock depends only on `wire` (ADR 0013) and
core cannot call the mock's behavior model, the in-process default is reached over
the wire like any endpoint; the concrete in-process transport (real loopback HTTP
vs an in-memory service) is #16's to pin, with a standing **lean toward real
loopback** so the client code path is byte-identical to a real endpoint.

Identity injection is a **two-layer split**. The `dyn LlmClient` transport is a
single stateless value shared as `Arc` (one connection pool); each agent holds a
cheap **`AgentChannel { transport, agent, seed, call_seq: AtomicU64 }}`** whose
`complete()` does `fetch_add(1)` on the counter, renders the `user` field from the
agent's current handle-and-specialty per ADR 0012's grammar, packs a
`WireIdentity { user, call_seq, seed }`, and delegates. The adapter stamps the
`user` field into the schema-pure body and writes the auxiliary channels as the
`X-OpenTeam-Call-Seq` and `X-OpenTeam-Seed` headers (names constant in `wire`).
The counter is **monotonic per channel for the whole run and never resets on
respecialization** — respecialize only changes the specialty slug the channel
renders into `user` (`team-agent:agent-1:slug-A` → `:slug-B`); the counter keeps
climbing, so no two completions by one agent ever collide to a single
`(user, call-seq, seed)` determinism key, even across a specialty swap. Embeddings
carry no `X-OpenTeam-*` headers: mock embeddings are seed-independent (ADR 0014),
so the call-sequence channel is chat-only. Configuration is `LlmConfig { base_url:
Option<Url>, api_key: Option<String>, model, embedding_model }`, built from clap in
the bin: `base_url = None` means "use the in-process mock's bound address",
`--llm-base-url` overrides it and skips starting the mock, and
`OPENTEAM_LLM_API_KEY` (clap `env`) is sent as `Authorization: Bearer` when present
— the config-only, untested real-endpoint escape hatch of ADR 0001.

`TokenCounter` stays a single-method primitive in `wire` —
`fn count(&self, text: &str) -> usize`, default `CharCountTokenizer` = `ceil(chars
/ 4)` — because summation is fixed policy, not a pluggable concern. Wire free
functions do the accounting the mock needs to fill `usage`: `prompt_tokens = Σ
count()` over every request message's rendered content plus each tool-call
`arguments` string, `completion_tokens = count()` over the generated assistant text
(or the serialized `tool_calls` on a tool-call turn), `total = prompt +
completion`. Usage is informational (ADR 0001, no cost tracking), so
deterministic-and-plausible is the bar. The same `count(&str)` primitive serves
both consumers — the mock's `usage` (via the free-fns) and the context assembler's
section budgets (ADR 0016, via direct calls) — one tokenizer, never two.

**Rejected.** N full `LlmClient`s, one per agent (duplicates the transport and its
connection pool; the counter is the only per-agent state, so a cheap handle over a
shared transport is the right cut); a counter that resets on respecialization
(pure downside — risks `(user, call-seq, seed)` collisions for marginal
tidiness); `content: String` only (would make the mock 400 on a real client's
content-part array, breaking ADR 0013's "provably serves any OpenAI-schema
client"); requesting `encoding_format: "float"` for our own traffic (leaves ADR
0014's committed base64 default documented but unexercised by real runs); a
reqwest-typed error on the trait (leaks the transport into the seam and blocks the
in-memory fake adapter); and growing `TokenCounter` with `count_message` /
`count_request` methods (the summation is one fixed policy — it belongs in
free-fns, not multiplied across the trait's adapters).

**Amended by ADR 0026 (2026-07-17).** `LlmConfig` and the `ReqwestLlmClient`
reqwest adapter are unchanged, but the bin no longer resolves an absent `base_url`
to the in-process mock — the default now resolves to the real OpenAI URL
(`https://api.openai.com/v1`), and `--mock` is what selects the mock. The
`base_url: None` ⇒ mock doc comment above is stale in spirit: base-url resolution
(real default vs `--mock`) now lives entirely in the bin.
