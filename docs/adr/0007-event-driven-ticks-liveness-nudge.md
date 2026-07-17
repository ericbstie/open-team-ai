# Event-driven ticks, with a liveness nudge that must never fire

There is no fixed-interval scheduler loop. A tick is one orchestrator turn, fired
when the previous turn is done and there is pending input, unassigned work, or
idle agents with open work. A coarse (~500ms) liveness nudge runs only when the
system is quiescent but incomplete (all agents idle, nothing in flight, board not
done), re-evaluates scheduling, and emits a distinct event. It is deadlock
insurance, not a scheduling mechanism: in correct operation it never fires, so it
cannot perturb deterministic tests — and if it ever fires, that is a bug surfacing
loudly instead of a silent hang.
