# Termination is verb-gated and validated

A run ends only when the orchestrator calls `finish_run(report)`. The runtime
validates that no open or claimed tasks remain; otherwise the call returns an
error tool-result enumerating the blockers, and the orchestrator must complete,
reassign, or `cancel_task(id, reason)` them first — cancellation is a first-class
verb so runs can converge deliberately. Safety caps (`--max-ticks`,
`--max-llm-calls`, `--max-duration`) force-terminate with a stub report and
persisted partial artifacts. Exit codes: 0 = clean finish, 2 = cap hit,
1 = harness error.
