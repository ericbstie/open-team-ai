# OpenAI wire subset: chat completions (tool calling) and embeddings

Research for the wire subset the mock serves and the harness's LLM client speaks:
`POST /v1/chat/completions` (non-streaming, tool calling) and `POST /v1/embeddings`.
Resolves wayfinder ticket [#4](https://github.com/ericbstie/open-team-ai/issues/4).

**Primary sources** (retrieved 2026-07-16):

- Official OpenAPI spec, `openai/openai-openapi` `master` `openapi.yaml`, spec version **2.3.0** —
  <https://github.com/openai/openai-openapi>. Schema anchors cited below
  (`CreateChatCompletionRequest`, `ChatCompletionRequestToolMessage`, etc.) are component names in
  that file. This spec is the machine-readable source of the rendered API reference at
  <https://platform.openai.com/docs/api-reference/chat/create> (the rendered site blocks
  non-browser fetches; every claim here is taken from the spec itself).
- Official Python SDK, `openai/openai-python` — error/status mapping from the README
  (<https://github.com/openai/openai-python#handling-errors>) and embeddings client source
  (<https://github.com/openai/openai-python/blob/main/src/openai/resources/embeddings.py>).
- First-party function-calling guide snapshot in `openai/openai-cookbook`
  (`examples/data/oai_docs/function-calling.txt`) for the parallel-tool-call pairing rule.

---

## 1. POST /v1/chat/completions

### 1.1 Request (`CreateChatCompletionRequest`)

**Required: `model` (string), `messages` (array, minItems 1).** Everything else is optional.

`messages` is a discriminated union on `role` (`ChatCompletionRequestMessage`):

| role | required fields | optional fields | notes |
|---|---|---|---|
| `system` | `content`, `role` | `name` | `content`: string **or** array of `{type:"text",text}` parts |
| `developer` | `content`, `role` | `name` | o1+ replacement for `system`; same content forms |
| `user` | `content`, `role` | `name` | `content`: string or content-part array (text/image/audio/file) |
| `assistant` | `role` only | `content`, `tool_calls`, `refusal`, `name`, `audio`, `function_call` (deprecated) | `content` is **nullable** and "required unless `tool_calls` or `function_call` is specified" |
| `tool` | `role`, `content`, `tool_call_id` | — | `content`: string or text-part array. `tool_call_id` pairs it to one entry of a prior assistant `tool_calls` |
| `function` | (deprecated) | | legacy; safe to reject in the mock |

**`tools`** — array; each item for our subset is `ChatCompletionTool`:

```json
{
  "type": "function",
  "function": {
    "name": "post_message",
    "description": "Send a message to an agent or team.",
    "parameters": { "type": "object", "properties": { "...": {} }, "required": ["..."] },
    "strict": false
  }
}
```

Per `FunctionObject`: only `name` is required (a-z, A-Z, 0-9, `_`, `-`, max 64 chars);
`description`, `parameters` (arbitrary JSON Schema object), and `strict` (nullable bool, default
false) are optional. **Omitting `parameters` defines a function with an empty parameter list.**
(The spec's `tools` items are `oneOf` function tool | `custom` tool; custom tools are out of our
subset — reject or ignore `type:"custom"`.)

**`tool_choice`** (`ChatCompletionToolChoiceOption`) — one of:

- string `"none"` | `"auto"` | `"required"`;
- named function: `{"type":"function","function":{"name":"my_function"}}`;
- (also in spec, out of our subset: `{"type":"allowed_tools",...}` and `{"type":"custom",...}`).

Default is `none` when no `tools` present, `auto` when tools are present.

**`parallel_tool_calls`** — bool, **default `true`** (`ParallelToolCalls`).

**`user`** — string. NOTE: the spec now marks it **deprecated**, "being replaced by
`safety_identifier` and `prompt_cache_key`" — but it remains a standard accepted field, so
ADR 0008 (agent identity in `user`) stays wire-legal. The mock should also accept
`safety_identifier` / `prompt_cache_key` without erroring.

**Optional params the mock must at least accept without erroring** (all top-level, all optional in
the spec): `temperature` (0–2, default 1, nullable), `top_p` (0–1, default 1, nullable),
`max_completion_tokens` (int, nullable), `max_tokens` (deprecated, nullable), `n` (1–128, default
1), `stop` (string | array of up to 4 strings | null), `frequency_penalty` / `presence_penalty`
(−2..2, default 0), `seed` (int, deprecated), `logit_bias` (map, nullable), `logprobs` (bool,
default false), `top_logprobs` (0–20), `response_format` (`{"type":"text"|"json_object"|"json_schema",...}`),
`metadata`, `store` (default false), `service_tier`, `stream_options`, `prediction`, `modalities`,
`reasoning_effort`, `verbosity`, `audio`, `web_search_options`, plus deprecated `functions` /
`function_call`. Accept-and-ignore is the compatible posture (the spec does not set
`additionalProperties: false` on this schema; note the real API does reject unknown top-level keys,
so scenarios must not rely on smuggling extra fields).

**`stream`** — bool, nullable, **default false**. Streaming is out of scope v1: the mock must
reject `stream: true` with HTTP **400** and the standard error body (§3), e.g.
`param: "stream"`, `type: "invalid_request_error"` — a shape every OpenAI SDK turns into its
`BadRequestError` without choking. Absent or `false` proceeds normally.

### 1.2 Response (`CreateChatCompletionResponse`)

Required top-level keys: **`id`, `object`, `created`, `model`, `choices`**. `usage` is formally
optional but always present on non-streaming responses — the mock must always emit it (token
numbers come from the shared `TokenCounter`). `system_fingerprint` is deprecated/optional; omit.

Each element of `choices` requires **`index`, `message`, `finish_reason`, `logprobs`** — note
`logprobs` is a *required key* (nullable): emit `"logprobs": null`.

`message` (`ChatCompletionResponseMessage`) requires **`role` (always `"assistant"`), `content`
(nullable), `refusal` (nullable)**; `tool_calls` optional. Emit `"content": null, "refusal": null`
explicitly on tool-call turns.

`finish_reason` enum: **`stop` | `length` | `tool_calls` | `content_filter` | `function_call`**
(last is deprecated). The mock needs `stop` (natural end / `finish_run` turn), `tool_calls`
(coordination-verb calls), and `length` if it ever honors `max_completion_tokens`.

Each `tool_calls` entry (`ChatCompletionMessageToolCall`) requires all of:

```json
{
  "id": "call_abc123",
  "type": "function",
  "function": { "name": "get_current_weather", "arguments": "{\"location\": \"Boston, MA\"}" }
}
```

**`function.arguments` is a JSON-encoded *string*, not an object** — and the spec warns the model
"does not always generate valid JSON" (our behavior model, by contrast, must always emit valid
JSON; contract tests pin that). Real ids look like `call_<alnum>`; ids must be unique within the
turn since tool results pair on them.

`usage` (`CompletionUsage`) requires **`prompt_tokens`, `completion_tokens`, `total_tokens`**
(ints). Optional detail objects `prompt_tokens_details` (`cached_tokens`, `audio_tokens`, …) and
`completion_tokens_details` (`reasoning_tokens`, `audio_tokens`, `accepted_prediction_tokens`,
`rejected_prediction_tokens`) may be omitted entirely.

Canonical tool-call response (spec `x-oaiMeta` example, trimmed):

```json
{
  "id": "chatcmpl-abc123",
  "object": "chat.completion",
  "created": 1699896916,
  "model": "gpt-4o-mini",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": null,
        "tool_calls": [
          {
            "id": "call_abc123",
            "type": "function",
            "function": { "name": "get_current_weather", "arguments": "{\n\"location\": \"Boston, MA\"\n}" }
          }
        ]
      },
      "logprobs": null,
      "finish_reason": "tool_calls"
    }
  ],
  "usage": { "prompt_tokens": 82, "completion_tokens": 17, "total_tokens": 99 }
}
```

### 1.3 Multi-tool-call turns: ordering and pairing

From the first-party function-calling guide: a parallel-call turn returns **one assistant message
whose `tool_calls` array holds N calls, each with an `id`**; the client must then append **N
`role:"tool"` messages, each with `tool_call_id` referencing one `id`**, before the next model
turn. Parallel calling is disabled with `parallel_tool_calls: false` (then the model calls at most
one tool per turn).

The wire contract a faithful mock must validate on incoming `messages`:

1. Every `role:"tool"` message must respond to a preceding assistant message carrying `tool_calls`
   (400 `invalid_request_error` otherwise).
2. An assistant message with `tool_calls` must be followed by tool messages answering **every**
   `tool_call_id` in it before any other role appears (400 otherwise).
3. Order of the N tool messages among themselves is not significant beyond matching ids; one tool
   message per id.

Full two-round transcript shape:

```jsonc
// round 1 request messages
[
  { "role": "system", "content": "You are agent worker-3." },
  { "role": "user", "content": "Weather in Boston and in Paris?" }
]
// round 1 response: finish_reason "tool_calls", message.content null, two calls
{ "role": "assistant", "content": null, "tool_calls": [
    { "id": "call_1", "type": "function", "function": { "name": "get_weather", "arguments": "{\"city\":\"Boston\"}" } },
    { "id": "call_2", "type": "function", "function": { "name": "get_weather", "arguments": "{\"city\":\"Paris\"}" } } ] }
// round 2 request appends the assistant message VERBATIM, then one tool message per id
[
  ...,
  { "role": "assistant", "content": null, "tool_calls": [ /* as above */ ] },
  { "role": "tool", "tool_call_id": "call_1", "content": "68F, sunny" },
  { "role": "tool", "tool_call_id": "call_2", "content": "22C, cloudy" }
]
// round 2 response: normal assistant text, finish_reason "stop"
```

---

## 2. POST /v1/embeddings

### 2.1 Request (`CreateEmbeddingRequest`)

**Required: `model`, `input`.** The schema sets **`additionalProperties: false`** — unknown fields
are a schema violation here (unlike chat), so the mock should 400 on unknown embedding fields.

`input` is `oneOf` four forms (empty string not allowed; arrays 1–2048 items):

1. string — `"This is a test."`
2. array of strings
3. array of integers (a single pre-tokenized input)
4. array of integer-arrays (multiple pre-tokenized inputs)

The harness's client only needs forms 1–2; a faithful mock should at minimum 400 cleanly (not
panic) on the token-array forms if it doesn't implement them.

Optional: **`encoding_format`** (`"float"` (default) | `"base64"`), **`dimensions`** (int ≥ 1,
only `text-embedding-3`+), **`user`** (string, same identity field as chat).

### 2.2 Response (`CreateEmbeddingResponse`)

Required top-level: **`object` (always `"list"`), `data`, `model`, `usage`**. `usage` requires
**`prompt_tokens`, `total_tokens`** (no `completion_tokens`). Each `data[]` item (`Embedding`)
requires **`object` (always `"embedding"`), `index`, `embedding`** — `index` is the position in
the input array; `embedding` is an array of floats (1536 for ada-002/3-small class models), or a
base64 string when `encoding_format:"base64"`.

```json
{
  "object": "list",
  "data": [
    { "object": "embedding", "embedding": [0.0023064255, -0.009327292, -0.0028842222], "index": 0 }
  ],
  "model": "text-embedding-ada-002",
  "usage": { "prompt_tokens": 8, "total_tokens": 8 }
}
```

### 2.3 The base64 trap (openai-python)

`openai-python` **silently sends `encoding_format: "base64"` whenever the caller did not specify
one**, then decodes the returned string client-side as a base64-encoded **little-endian float32
buffer** (`np.frombuffer(b64decode(data), dtype="float32")` / `array.array("f", ...)`). Source:
`src/openai/resources/embeddings.py` (`if not is_given(encoding_format): params["encoding_format"] = "base64"`).

Consequences for the mock:

- To be faithful, implement base64: base64 of the f32-LE byte serialization of the vector.
- openai-python's decoder happens to skip non-string `embedding` values, so returning float arrays
  even when base64 was requested works *for that client* — but that is client leniency, not
  contract; other SDKs may not tolerate it. Implement base64; it is ~5 lines.

---

## 3. Errors

Error body (`ErrorResponse` / `Error`): a top-level `error` object whose **four keys are all
required** — `message` (string), `type` (string), `param` (string | null), `code` (string | null):

```json
{
  "error": {
    "message": "Streaming is not supported by this endpoint.",
    "type": "invalid_request_error",
    "param": "stream",
    "code": null
  }
}
```

Status codes as consumed by the official SDKs (openai-python README table — this is what an
unmodified client expects to map to typed exceptions):

| status | SDK error | mock usage |
|---|---|---|
| 400 | `BadRequestError` | validation failures, `stream: true`, unknown embedding fields, tool-message pairing violations |
| 401 | `AuthenticationError` | (mock: only if it chooses to enforce a key) |
| 403 | `PermissionDeniedError` | — |
| 404 | `NotFoundError` | unknown route; real API also uses it for unknown model (`code: "model_not_found"`) |
| 422 | `UnprocessableEntityError` | — |
| 429 | `RateLimitError` | — (no rate limiting in the mock) |
| ≥500 | `InternalServerError` | mock/harness bugs |

Common `type` values: `invalid_request_error` (the workhorse for 400/404 validation) plus
`authentication_error`, `rate_limit_error`, `server_error`. For the mock, every rejection can be
`invalid_request_error` with a precise `message` and `param`; SDK behavior keys off the HTTP
status, not `type`.

---

## 4. Minimal-but-faithful mock subset checklist

For an unmodified OpenAI-compatible client (openai-python/node, async-openai in Rust) to work:

- `POST /v1/chat/completions`: accept full request schema above (ignore unknown optional params);
  require `model` + `messages`; validate tool-message pairing; respond with `id`
  (`chatcmpl-<uid>`), `object: "chat.completion"`, `created` (unix seconds), `model` (echo),
  `choices[0] = {index, message{role,content,refusal}, tool_calls?, logprobs: null, finish_reason}`,
  and always a `usage` with the three required ints. `n` > 1: either honor it or 400; never return
  fewer choices than promised.
- Tool calls: `id` unique per turn (`call_<uid>`), `type: "function"`, `function.arguments` a
  valid-JSON *string*; `finish_reason: "tool_calls"` on those turns and `content: null`.
- `tool_choice`: honor `none` / `auto` / `required` / named-function; `parallel_tool_calls: false`
  caps calls per turn at 1.
- Reject `stream: true` with 400 + standard error body, `param: "stream"`.
- `POST /v1/embeddings`: require `model` + `input`; accept string and string-array inputs
  (one `data[]` entry per input, `index` = input position); honor `encoding_format` **including
  base64 (f32-LE)** since openai-python defaults to it; honor or validate `dimensions`; respond
  `object: "list"` + `data` + `model` + `usage{prompt_tokens, total_tokens}`.
- All errors: `{ "error": { message, type, param, code } }` with all four keys present.
- Headers: real API wants `Authorization: Bearer <key>`; SDKs pass arbitrary extra headers, so the
  ADR 0008 `X-OpenTeam-*` channel rides cleanly.

## 5. Surprises worth remembering

1. **`user` is now deprecated** in the spec (replaced by `safety_identifier` / `prompt_cache_key`)
   though still accepted — ADR 0008 unaffected on the wire, but future OpenAI SDK majors could
   stop surfacing it; the mock should read all three.
2. **`content` is string-or-array everywhere on input** (even `system` and `tool` allow text-part
   arrays) — client types need an untagged enum, and the mock must accept both forms.
3. **Response `message.content` is null (not "") on tool-call turns**, and `content`/`refusal`/
   `logprobs` are required-but-nullable keys — emit them as explicit `null`.
4. **`function.arguments` is a JSON string**, and OpenAI documents that it may be *invalid* JSON —
   real-endpoint client code must treat parse failure as a model error, not a bug (mock contract
   tests guarantee validity from our behavior model).
5. **`parallel_tool_calls` defaults to `true`**; multiple calls arrive in one assistant message and
   each needs its own `role:"tool"` reply keyed by `tool_call_id`.
6. **openai-python requests `encoding_format: "base64"` by default** for embeddings (§2.3).
7. **`CreateEmbeddingRequest` is `additionalProperties: false`** in the spec while the chat request
   is not — strictness is asymmetric between the two endpoints.
8. `tools` items and `tool_choice` have grown non-function variants (`custom`, `allowed_tools`) —
   our subset can reject them, but client enums should be `#[non_exhaustive]`-minded.
9. `max_tokens` is deprecated in favor of **`max_completion_tokens`**; accept both, prefer the
   latter.
