# Schema-pure request bodies; identity rides in legal channels

Requests to the LLM endpoint stay strictly OpenAI-schema. Agent identity goes in
the standard `user` field of the chat-completions request; auxiliary metadata
(turn number, run seed) goes in `X-OpenTeam-*` HTTP headers, which real endpoints
ignore. The mock keys its determinism on (user field, turn header, seed header)
and never sniffs message content for identity. Any OpenAI-compatible server
therefore accepts the exact same traffic the mock sees.
