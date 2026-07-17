# One agent runtime: the control plane is LLM-driven too

The orchestrator, meta-agents, and team agents all run on the same LLM tool-calling
turn loop; a role is nothing more than (system prompt, tool registry, context
policy). One consequence is recorded here because it follows directly: the
`--parallel` worker cap gates team agents only — the orchestrator and meta-agents
are control plane and are never queued behind workers, which would invert
priorities and stall scheduling. Rejected alternative: a hand-coded (non-LLM)
orchestrator, which would have made the control plane cheaper but broken the
premise that orchestration itself is model-reasoned and mockable.
