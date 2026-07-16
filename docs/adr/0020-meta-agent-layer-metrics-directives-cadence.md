# The meta-agent layer: one runtime-owned metrics module, counter-triggered meta turns, and six typed directive verbs

**Metrics are runtime-owned, computed by one accumulator, projected three ways.**
A single `Metrics` module folds the append-only event log incrementally
(`fold(&Event)`) and exposes three pure projections: `run_health_line()` — the
compact throughput / utilization / mailbox-pressure line folded into the
orchestrator's board digest (ADR 0016); `digest() -> MetricsDigest` — the
meta-agent's full process-metrics view (its context slot 2); and
`summary() -> RunSummary` — the report's run-summary block (#19). This resolves
the map's "N-consumer metrics projection" question as **one computation, three
views**. Ownership is load-bearing: the module lives in the runtime, not the
meta-agent, so a `--meta-agents 0` run still emits the health line and the report
summary — metrics never depend on the meta role existing. Every field is a fold
over events (`task_created` / `claimed` / `released` / `done` / `cancelled` /
`unassigned`, `message_sent`, `agent_slept` / `parked` / `woke`,
`knowledge_written`, `directive_issued` / `fulfilled` / `declined`,
`liveness_nudge`, plus each completion's `usage` for token spend), so metrics are
an external oracle over the log rather than an LLM opinion (prior-art §9). All
time-like quantities — task latency, oldest-pending-age, stall windows — are
counted in **deterministic units (EventId deltas / orchestrator ticks), never
wall-clock**, so the digest is golden-testable; wall-clock duration appears only
in the report's human summary. `MetricsDigest` carries: throughput; task latency
(queue `created→claimed`, work `claimed→done`); task churn (repeated releases,
per-task and per-agent); agent utilization (Idle/Working/Asleep split, idle
streaks); mailbox pressure (depth, max, oldest-pending-age); token spend
(per-agent, run total, per-done-task); faults (park count, parked set,
consecutive-malformed counters); sleep/wake counts; message volume (by address
kind, with orchestrator-directed volume as the discovery-load proxy);
knowledge-store size (entry count, byte size, by kind); stall counters
(ticks-since-last-`task_done`, repeated-no-op-yield, pair-churn); and a
**directives** category (issued / fulfilled / declined, by kind) that measures the
meta layer's own effectiveness and feeds the report's "meta interventions" line.

**A meta-turn is counter-triggered, never per-tick and never per-event.** The
meta-agent is control-plane (ADR 0002), so it acquires no `--parallel` permit, and
it is dispatched by the event-driven reactor (ADR 0007), never an interval loop.
The trigger has two parts: a **coalesced cadence** fires one meta-turn once
unobserved events of the subscribed kinds cross a fixed internal threshold (bursts
coalesce into a single turn — no per-event busy-spin), and a **priority wake**
fires a meta-turn immediately on the signals the layer exists to answer —
`agent_parked`, a task's release count crossing threshold, `directive_declined`,
and `liveness_nudge` (a fired watchdog is a proven stall, the loudest possible
"improve the process" signal; the forced orchestrator tick breaks the deadlock
while the meta-turn reasons about why it happened). The threshold is a fixed
constant like the K=3 park counter, not a flag: meta responsiveness is internal
tuning, and the user's flag budget is already spent on `--meta-agents`;
`--meta-interval` is deferred until demand appears.

**Directives are six discrete typed verbs across two tiers; the meta-agent emits
nothing else.** The meta registry is directive-emitters only — no messages, no
knowledge writes, no search — so it observes purely through its four context
slots. Mechanical verbs are applied by the runtime and return `ok{applied}`:
`set_parallelism{target}` (clamped to `[1, --parallel]`, add/forget semaphore
permits, returns the new effective value), `sleep_agent{agent}` (Idle-only, else a
`rejected` domain refusal), `wake_agent{agent}` (Asleep/parked → schedulable,
returns the restored state). Judgment verbs are enqueued to the orchestrator and
return `ok{directive_id}`: `propose_respecialize{agent, specialty}`,
`propose_reallocate{task, reason}`, `propose_rebalance{team, members}`. The
`propose_` prefix keeps meta's judgment verbs textually distinct from the
orchestrator's own `respecialize` / `unassign` / `set_team_members` — a reader
always knows who proposes and who acts. **Meta governs allocation, never
authorship**: it may move, respecialize, rebalance, and throttle, but it cannot
create or cancel tasks — task authorship is orchestrator-only (ADR 0010, #7).
`propose_reallocate` (moving existing work) is allocation and allowed;
create/cancel (authoring or killing work) is content and forbidden. This boundary
is what stops the meta layer from becoming a second orchestrator — the
two-brains-one-body failure ADR 0005 guards against.

**Judgment directives are correlated by explicit citation, with no silent
timeout.** A judgment directive lands in the orchestrator's never-dropped
Directives section (ADR 0016) as pending, tagged with its `directive_id`. The
orchestrator must resolve each: it **acts** by citing the id via an optional
`in_response_to: directive_id` on the action verb, which makes the runtime emit
`directive_fulfilled{directive_id}` and clear the pending entry; or it **declines**
via `decline_directive{directive_id, reason}`, emitting
`directive_declined{directive_id, kind, reason, by}`. There is no timeout-decline —
an unresolved directive nags in the never-dropped section until the orchestrator
explicitly acts-with-cite or declines, so the meta-agent's directive-outcomes slot
stays accurate. Explicit citation was chosen over auto-correlation, which would
mis-attribute when the orchestrator acts on its own initiative rather than in
response to a proposal.

**`--meta-agents N`: default 1, 0 permitted, N>1 a diverse panel.** `0`
instantiates no meta role — the run loses every directive (parallelism tuning,
meta sleep/wake, all proposals) but keeps the runtime metrics. `N>1` runs
redundant observers over the same event stream and the same digest, with no
observation partitioning or directive dedup in v1 — and this is a feature, not
waste: each meta-agent has a distinct positional handle (`meta-0`, `meta-1`, …),
so under the identity channels (ADR 0008) the mock keys each on a distinct `user`
field and returns a different behavior stream, yielding *diverse* independent
process-improvement proposals that the orchestrator arbitrates (declining
duplicates). Conflicts self-resolve: a redundant mechanical call (e.g. sleeping an
already-asleep agent) returns `rejected`; duplicate judgment proposals are
declined. Partitioning observation labor across meta-agents is a future
optimization.

**Rejected.** A meta-owned metrics computation (would vanish under
`--meta-agents 0`, taking the orchestrator's health line and the report summary
with it); three independent metric computations (triplicates the fold and lets the
three views drift); wall-clock in the digest (nondeterministic — breaks golden
tests); per-tick or per-event meta dispatch (lazy or busy-spinning — the
coalesced-plus-priority trigger covers both); a `--meta-interval` flag (internal
tuning, not user surface, in v1); a generic `issue_directive{tier, kind, payload}`
(the untyped-payload hole ADR 0017 already closed); meta task create/cancel (makes
meta a second orchestrator); auto-correlation or timeout-decline of judgment
directives (mis-attributes, or silently drops the orchestrator's obligation to
respond); and observation partitioning / directive dedup across meta-agents (v1
keeps them as an independent diverse panel).
