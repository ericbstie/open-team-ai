# A fixed agent pool that respecializes instead of spawning and destroying

Team agents are created once at run start (`--agents`) and never destroyed;
capability changes happen by respecialization — swapping an idle agent's specialty
and system prompt, wiping its transcript, preserving its id. Respecializing a
non-idle agent is illegal (a half-finished thought in a new persona is
incoherence); the orchestrator must unassign/park first. Teams form and dissolve
over this pool; only the orchestrator and meta-agents persist conceptually. Chosen
over spawn/destroy because it keeps dynamism cheap and the actor topology stable.
