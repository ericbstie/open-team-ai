# The mock server: real-loopback transport, stateless per request, and the `ChatDecision` behavior seam

The in-process default mock is served over **real loopback HTTP**: it binds
`127.0.0.1:0`, reads back the OS-assigned port, and hands `http://127.0.0.1:{port}`
to `LlmConfig.base_url`, so the `reqwest` `LlmClient` adapter's code path is
**byte-identical** to talking to a real endpoint — it exercises serialization, the
HTTP round trip, and the `X-OpenTeam-*` headers end to end, hardening the ADR-0001
config-only real-endpoint escape hatch instead of leaving it unexercised. There is
**no** in-memory / tower-oneshot transport as a third path: the two `LlmClient`
adapters stay the reqwest default and the in-memory fake for runtime unit tests
(ADR 0018), and the mock is reached like any endpoint over the wire.

The mock is **stateless per request**: every response is a pure function of the
request body plus its identity channels — the `user` field (ADR 0012 grammar) and
the `X-OpenTeam-Call-Seq` / `X-OpenTeam-Seed` headers (ADR 0008/0015; determinism
key `(user, call-seq, seed)`, unique per completion). Because the seed rides in a
per-request header (ADR 0013), the mock needs **no per-run state**: `AppState` is
immutable shared config only — `{ behavior model, clock, token counter, optional
scenario }` — held as `Arc`. The standalone mock therefore has **no run
registration, no run lifecycle, no cleanup**; multiple concurrent runs against one
standalone mock are isolated purely by their differing seed header, and two runs
that happen to share a seed simply get the same deterministic output with zero
cross-talk, because nothing mutable is shared. This is the cleanest possible mock
and it falls straight out of ADR 0008's seed-in-header.

The behavior model is reached through a **synchronous** seam that returns only the
semantic decision, never the envelope:

```rust
trait BehaviorModel {
    fn chat(&self, req: &ChatCompletionRequest, id: &WireIdentity) -> ChatDecision;
}
// ChatDecision = the assistant ResponseMessage (text OR tool_calls) + FinishReason. Nothing else.
```

The mock server wraps a `ChatDecision` into a valid `ChatCompletionResponse`,
owning the entire envelope: `id` (**derived deterministically** from
`(user, call-seq, seed)`), `created` (from the **injected `Clock`**, frozen in
tests), the `model` echo, `choices[]`, and `usage` (via the `openteam-wire` token
free-fns, ADR 0018). This makes **"every response is schema-valid OpenAI"
structural**: the behavior model never touches the envelope, so it *cannot* emit an
invalid one — the interesting responsibility (what to say) is isolated from the
boring-but-critical one (valid framing), and #18 cannot get the framing wrong.
`chat` is synchronous because the behavior model is pure computation over
`(request, identity, seed)` with no I/O and no run-state access (the mock never sees
the board or store) — no `#[async_trait]` box-per-call is warranted. Two adapters
justify the trait: the built-in arc (#18) and the scenario player (#20), the latter
selected when `AppState`'s optional scenario is present. Deriving `id` from the key
and `created` from the `Clock` does **not** contradict the map's "byte-identical
global determinism is out of scope" exclusion — that exclusion is about the whole
run's event log across tokio interleaving; a single mock response being a pure
function of its request is exactly the determinism we want, and it lets #23's
contract tests assert exact envelopes.

Embeddings **bypass the seam entirely**. `/v1/embeddings` is a fixed deterministic
wire function (ADR 0014 signed feature-hashing, base64 f32-LE by default), reads no
`X-OpenTeam-*` headers, and is not scenario-overridable — embeddings are
computation, not behavior.

The axum app is one **`build_router() -> axum::Router`** mounted identically by the
in-process default, the standalone `openteam mock serve`, and the contract tests —
embedded and standalone differ **only** in who owns the listener's lifetime and
shutdown. Routes are `POST /v1/chat/completions` and `POST /v1/embeddings`; identity
is read from the `user` field plus the two headers into a `WireIdentity` (tolerating
`safety_identifier` / `prompt_cache_key`, ADR 0008); all errors flow through a single
`MockError -> (StatusCode, Json<wire::ApiError>)` `IntoResponse` path. The mock owns
the cheap, bug-catching slice of wire validation: require `model` + `messages`;
`400` on `stream:true` (`param:"stream"`); the embeddings request's
`deny_unknown_fields` → `400` on stray fields; honor `encoding_format`; `404` on an
unknown route or model. It **does not enforce tool-message pairing in v1** — our
harness's turn loop (ADR 0015) structurally emits one `role:"tool"` reply per
`tool_call_id`, so enforcement would validate traffic our own client never violates,
be reachable only by a contrived negative test, and add message-array-walking
complexity; recording it as a conscious faithfulness gap is safe because a real
endpoint is *stricter* (it enforces pairing), and stricter-real-vs-lenient-mock is
the safe direction — the harness already satisfies the strict rule, so pointing
`--llm-base-url` at a real server breaks nothing. Standalone serve is grouped under a
`mock` command (`openteam mock serve`, with `--port`; `0` = ephemeral) leaving room
for future mock tooling; final flag names are #21's. Contract-test hooks are exactly
`build_router()`, `serve() -> (SocketAddr, ShutdownHandle)`, and the single error
path; because transport is real loopback, contract tests point a real OpenAI client
(`async-openai` / raw `reqwest`) at the bound address and assert schema-validity —
which validator is #23's call.

**Rejected.** An in-memory / tower-oneshot transport for the in-process default (a
second client code path the real endpoint never exercises — trades away ADR-0001
hardening for a saved socket; the mock is cheap to bind); per-run mock state / a run
registry (needless lifecycle plus a concurrency hazard, when the seed-in-header
already isolates runs for free); a behavior seam returning a full
`ChatCompletionResponse` (lets #18 emit an invalid envelope, forfeiting the
structural schema-validity guarantee — the server owning the envelope is the whole
point); an `async` behavior seam (no I/O to await; `#[async_trait]`'s box-per-call
buys nothing over sync pure computation); wall-clock `created` + a random `id`
(a non-reproducible envelope defeats #23's exact-envelope assertions, for no gain);
and enforcing tool-message pairing in v1 (validation our own traffic never trips,
testable only by a contrived negative, and lenient-vs-strict-real is the safe
direction).
