# Schema-pure request bodies; identity rides in legal channels

Requests to the LLM endpoint stay strictly OpenAI-schema. Agent identity goes in
the standard `user` field of the chat-completions request; auxiliary metadata
(a per-agent LLM-call sequence number, run seed) goes in `X-OpenTeam-*` HTTP
headers, which real endpoints ignore. The mock keys its determinism on
(user field, call-sequence header, seed header) and never sniffs message content
for identity. Any OpenAI-compatible server therefore accepts the exact same
traffic the mock sees.

**Amendment (ADR 0015).** The auxiliary metadata is a per-agent **call-sequence
counter** — incremented on every `/v1/chat/completions` an agent issues — not a
turn index. A turn is a capped inner completion↔tool loop (ADR 0015), so up to
`MAX_TOOL_ITERS` completions share one turn; keying determinism on the turn index
would collide them to a single response. The call sequence is unique per
completion and keeps determinism intact. `--max-ticks` still counts orchestrator
turns; the wire header counts completions.
