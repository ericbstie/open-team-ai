# The task board is flat and orchestrator-authored; claiming is pull-only

Only the orchestrator creates, unassigns, and cancels tasks; team agents surface
discovered work by messaging it. There are no task-to-task dependency edges —
ordering is orchestrator judgment over the per-tick board digest — and no
push-assignment: agents claim open tasks themselves (at most one each,
team-eligibility checked at claim time, first claim wins atomically), while the
orchestrator steers via team tags and messages. Rejected: `depends_on` edges
(duplicates the orchestrator's judgment in machinery), team-agent task creation
(dilutes the board as the steering surface), and push-assignment (a second
allocation path that muddies claim contention). Accepted trade-off: frequent
discoveries cost orchestrator ticks — watching that bottleneck is a meta-agent
concern. Reopen trigger: the dry-run transcript prototype showing the
orchestrator drowning in sequencing chatter.
