# The wire crate is the contract; the mock depends on nothing else

`openteam-wire` is the contract crate — it holds everything the harness and the
mock must agree on, and only that: the OpenAI wire subset, the identity grammar
(`AgentId`/role/specialty-slug ⇄ the `user` field, per ADR 0012), the
`X-OpenTeam-*` header names, the `Seed`, and the `TokenCounter` (which exists to
fill the wire's `usage` fields and budget what goes over the wire). The mock's
only internal dependency is `openteam-wire`, and its behavior model learns the
available coordination verbs solely from each request's `tools` array — the
mock's only knowledge of the harness is the request it is currently reading.
This makes ADR 0008's schema purity structural: the mock provably serves any
OpenAI-schema client, not just ours. Rejected: a shared verb-constants module
between harness and mock (couples the mock to harness internals and hides
drift) and a mock dependency on the domain crate (privileged knowledge no real
endpoint has). Accepted trade-off: verb/schema lockstep is guarded by the
prompt-legibility contract tests instead of the type system.
