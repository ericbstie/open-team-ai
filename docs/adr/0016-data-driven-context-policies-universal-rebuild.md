# Context is assembled by one data-driven policy per role, and every role rebuilds

**The seam is data, not a trait.** One deep assembler —
`assemble(policy, view, store, counter) -> AssembledPrompt` — interprets a
`ContextPolicy` that is a *value*: an ordered `Vec<SectionSpec>` (each
`{ kind, budget, priority, drop_rule }`) plus a total assembly budget. There is
exactly one real assembler, so a `ContextAssembler` trait would be a seam with a
single adapter (codebase-design: one adapter ⇒ no seam); policy-as-data is
inspectable, loggable, and unit-testable in a way three trait impls are not.
Per-role policies are constructors — `ContextPolicy::orchestrator()`,
`::team_agent(specialty)`, `::meta_agent()`. Presentation **order** (fixed per
policy, top-to-bottom in the `user` message) is deliberately separate from
allocation **priority** (which section gets budget first and degrades last).

**Every role rebuilds — team agents included.** This extends ADR 0004 from the
orchestrator to all three roles: no role keeps a persistent append-only
transcript across turns. It is philosophically core, not incidental — the
system's thesis is that state lives in the *shared* substrate (board, messages,
knowledge store), so rebuilding each turn forces every agent to externalize its
durable output rather than hoard a private transcript that would context-collapse
on a long task (the documented prior-art failure mode). The within-turn inner
loop (ADR 0015) still keeps `assistant`/`tool` messages so an agent can chain on a
verb result inside one reasoning episode, but those are turn-local and dropped at
the rebuild. Cross-turn continuity is rendered as **text** in the assembled
prompt, never replayed as structural `assistant`/`tool` messages — so no
`tool_call_id` ever dangles across a rebuild without the `role:"tool"` reply the
wire (ADR 0013) demands.

**A team agent's only private continuity is a bounded window.** To progress a
multi-turn task it needs to see its own recent work, so it gets a budget-capped
**recent-activity sliding window** of its own recent turns' output —
oldest-dropped under budget, reset at each assignment boundary, wiped on
respecialization — not an unbounded transcript. ADR 0003's "respecialization
wipes its transcript" means wiping this window plus the assignment association,
already empty once the agent is Idle.

**The prompt is always two messages; degradation is deterministic.** Across turns
the assembled prompt is exactly `system` (the static skeleton, ADR 0012) +
`user` (the sections as `##`-delimited markdown in policy order). Budgets degrade
in priority order: Goal and Directives are never dropped; the board digest's
terminal tail shrinks first; knowledge retrievals drop lowest-cosine hits; and any
oldest-first section (fresh messages, the recent-activity window) always delivers
at least its single oldest item, so an over-budget head-of-line entry can never
jam the queue forever (the ADR 0011 head-of-line mitigation, generalized).
Auto-retrieval is pure `VectorStore::search` cosine top-k in v1 (deterministic;
the mock's lexical embeddings make cosine ≈ overlap); a recency/importance blend
is future tuning only.

**Rejected.** A persistent per-assignment transcript for team agents (hoards
private state, context-collapses on long tasks, breaks the externalize-to-shared
-substrate thesis); `ContextPolicy` as a trait with three impls (one real adapter,
duplicates budgeting/degradation across impls, not inspectable); a separate heavy
metrics section for the orchestrator (a folded one-line run-health summary
suffices — the full metrics digest is the meta-agent's, and the report is a third
view); and recency/provenance retrieval re-ranking in v1 (non-deterministic
tuning with no payoff against lexical mock embeddings).
