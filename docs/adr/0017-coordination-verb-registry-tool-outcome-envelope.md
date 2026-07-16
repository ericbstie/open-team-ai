# The coordination-verb registry is dispatch-by-name; its tool-result envelope separates `rejected` from `invalid`

Coordination verbs (the only tools in v1) are organized as one fixed **tool
registry per role** — team-agent, orchestrator, meta-agent — built from a list of
`ToolEntry { def, handler }`; there is **no per-verb trait**. The `ToolRegistry` is a
concrete struct with `tool_defs(role) -> &[ToolDef]` (rendered verbatim into every
chat-completions `tools` array, per ADR 0013) and a single
`async fn dispatch(role, caller, call, ctx) -> ToolOutcome` that matches on the verb
name, serde-decodes the JSON-string `arguments` into the verb's typed args struct, and
calls a plain async handler. One real registry means a trait would be a seam with a
single adapter (codebase-design); the extensibility seam is instead the entry list plus
the `RunContext` host handlers dispatch against — external-world tools (file/shell/net,
out of scope v1) plug in as new `ToolEntry`s backed by an injected host with no change to
`dispatch`; per-specialty registries (also out of scope) would vary the entry list by
specialty rather than by role.

Every dispatch returns a three-way **tool-outcome envelope**, serialized as the string
content of the wire's one `role:"tool"` reply per `tool_call_id`:

```jsonc
{ "status": "ok",       "result": { /* verb-specific */ } }
{ "status": "rejected", "code": "<domain_code>", "message": "...", "details": { /* opt */ } }
{ "status": "invalid",  "code": "unknown_verb" | "invalid_arguments", "message": "..." }
```

The split is load-bearing because it **defines the K = 3 park counter of ADR 0015**.
`invalid` is a schema/parse fault — an unknown verb name or args that fail typed
deserialization (`#[serde(deny_unknown_fields)]` is what makes a bad-args call
`invalid`). `rejected` is a schema-valid call the domain guard refused: a lost claim
race, a task not Open, a `finish_run` with open/claimed blockers (ADR 0006), a
respecialize/sleep target that is not Idle, a dissolve with live team tasks. This
**refines ADR 0015's "malformed turn = ≥1 call, none succeeded"**: "succeeded" means
returned `ok` **or** `rejected` (was well-formed), and a turn is malformed — incrementing
the park counter — **only when it emitted ≥1 call and every call was `invalid`**. An
agent that cleanly loses three claim races in one turn is behaving correctly and must
never park; `rejected` resets the counter exactly like `ok`. Against the mock,
`invalid` cannot occur (the prompt-legibility contract tests, #18), but `rejected` does
(claim races) — so tuning K stays a real-endpoint concern while the mock still exercises
the reject path.

Tool `parameters` are JSON Schema rendered by **schemars 1.2** (draft 2020-12) from each
args struct with `#[serde(deny_unknown_fields)]` (→ `additionalProperties:false`),
`strict:false` on the tool (verbs carry optional fields; OpenAI strict-mode would force
all-required), the top-level `$schema` stripped, and the schemas built once at startup
and cached (determinism; the mock reads only names/params per ADR 0013). schemars keeps
each verb's schema and its arg deserializer in lockstep from one type.

Rejected: a `Tool` async trait per verb (single-adapter seam, no dispatch variance —
ADR 0013's dispatch-by-name resolution); a generic `issue_directive { tier, kind,
payload }` for the meta-agent (a freeform `payload` is exactly the untyped hole that
breaks "the mock reads typed tools" and defeats schemars lockstep — meta directive kinds
stay discrete typed verbs, authored in #17); a two-value ok/error envelope (would either
park agents for losing claim races or hide schema faults from the counter — the
`rejected`/`invalid` distinction is the whole point); hand-rolled `serde_json::json!`
tool schemas (viable and zero-dep, but loses the struct-schema lockstep).
