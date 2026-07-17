# Offline-first: a deterministic in-process mock is the default LLM backend

`openteam` must run fully offline with reproducible behavior. All LLM traffic speaks
the OpenAI wire schema and is served by a deterministic, seedable mock that starts
in-process when a run begins (a standalone serve mode exists too). A real
OpenAI-compatible endpoint is reachable via configuration only (`--llm-base-url`,
`OPENTEAM_LLM_API_KEY`) and is untested in v1; provider hardening — retries, rate
limits, streaming, cost tracking — is deliberately out of scope. This buys
zero-setup runs and seeded end-to-end tests, at the price of the mock being the
only proven backend.
