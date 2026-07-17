# The orchestrator's context is rebuilt every turn, never appended

The orchestrator's prompt is reassembled each turn from token-budgeted,
relevance-ranked sections (goal, board digest, knowledge retrievals, fresh
messages, metrics) — never an append-only transcript. A transcript would degrade
and overflow over an unbounded run; rebuilding keeps the run's most central
context window permanently relevant at bounded size.
