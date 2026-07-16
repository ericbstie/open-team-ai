# OpenTeam

The ubiquitous language for `openteam`, an offline LLM harness that runs a dynamically
re-specialized team of agents — coordinated in realtime by a persistent orchestrator and
self-monitored by meta-agents — against a deterministic mock of the OpenAI API.

## Language

### Agents & roles

**Agent**:
An LLM-driven actor in a run. Every agent — orchestrator, meta-agent, or team agent — runs on the same tool-calling turn loop.
_Avoid_: bot, worker

**Role**:
The control class an agent belongs to: Orchestrator, Meta-agent, or Team agent. Roles differ only by system prompt, tool registry, and context policy.
_Avoid_: type, kind

**Orchestrator**:
The single persistent control-plane agent that decomposes the goal, runs the task board and teams, and alone can finish the run.
_Avoid_: main agent, coordinator, manager

**Meta-agent**:
A persistent control-plane agent that observes events and metrics and reasons only about improving the process, acting through directives.
_Avoid_: monitor, supervisor

**Team agent**:
A pooled worker agent that performs task work through coordination verbs; its specialty is swappable by respecialization.
_Avoid_: worker, sub-agent

**Agent pool**:
The fixed set of team agents created at run start and never destroyed during the run.
_Avoid_: fleet

**Specialty**:
The persona of a team agent — a named identity plus its system prompt — assigned and swapped by the orchestrator.
_Avoid_: role, profession

**Team**:
A runtime entity over team agents: a routable message scope plus a claim-eligibility scope for tasks, formed and dissolved by the orchestrator.
_Avoid_: group, squad

**Respecialization**:
Swapping an idle team agent's specialty and system prompt, wiping its transcript while preserving its identity.
_Avoid_: retraining, reassignment

### Work & coordination

**Run**:
One execution of the harness, from prompt to report — the unit of seeding, artifacts, and termination.
_Avoid_: session, job

**Task**:
A unit of work tracked on the task board, claimable by team agents subject to team eligibility.
_Avoid_: ticket, todo

**Task board**:
The shared registry of tasks and their lifecycle — the orchestrator's steering surface.
_Avoid_: queue, backlog

**Coordination verb**:
A tool exposed to agents that acts on the run's shared state — messages, board, knowledge, agent lifecycle. The only kind of tool in v1.
_Avoid_: action, command

**Tool registry**:
The fixed set of coordination verbs a role may call; one registry per role.
_Avoid_: toolbox, toolkit

**Message**:
A realtime communication between agents — addressed direct, to a team, or broadcast — delivered between turns and always ingested into the knowledge store.
_Avoid_: chat, notification

**Directive**:
A meta-agent's process-improvement instruction. Mechanical directives are applied directly by the runtime; judgment directives go to the orchestrator, which must act or decline with a logged reason.
_Avoid_: command, suggestion

### Knowledge & context

**Knowledge store**:
The run-scoped, in-process shared vector store that every agent reads and writes; created fresh each run.
_Avoid_: vector DB, memory

**Knowledge entry**:
One stored item in the knowledge store — text plus its embedding and provenance.
_Avoid_: document, record

**Context assembly**:
Rebuilding an agent's prompt each turn from token-budgeted, relevance-ranked sections instead of appending to a transcript.
_Avoid_: context patching, compaction

**Context policy**:
The per-role rule deciding what context assembly puts into an agent's prompt each turn.
_Avoid_: memory policy

### Runtime & lifecycle

**Turn**:
One LLM interaction by one agent — assembled context in, response out, tool calls executed. The atomic unit of agent activity.
_Avoid_: step, round

**Tick**:
One orchestrator turn, fired when the previous turn is done and there is pending input, unassigned work, or idle agents with open work.
_Avoid_: cycle, poll, scheduler pass

**Sleep**:
The agent state the scheduler will not run until an explicit wake — entered deliberately by verb or directive, or automatically after repeated malformed turns.
_Avoid_: park, suspend, pause

**Wake**:
Returning a sleeping agent to schedulability.
_Avoid_: resume

**Liveness nudge**:
A coarse quiescence check that fires only when nothing is in flight, all agents are idle, and the board is unfinished. Its firing signals a scheduling bug loudly; it is insurance, not a mechanism.
_Avoid_: heartbeat, watchdog

**Event**:
An append-only record of something that happened in a run; the event log is what tests, meta-agents, and the report read.
_Avoid_: log entry, audit record

**Run artifacts**:
The persisted output directory of a run: the event log, the final board, the knowledge entries, and the report.
_Avoid_: output dir, results

### Mock & determinism

**Mock**:
The in-process OpenAI-schema server that plays the model for every agent — the default and only tested LLM backend in v1.
_Avoid_: simulator, stub, fake

**Behavior model**:
The mock's engine for deciding responses. The built-in one procedurally carries any prompt through a decompose → work → converge arc and terminates by construction.
_Avoid_: script

**Scenario**:
A fixture file that overrides the built-in behavior model with scripted responses for tests.
_Avoid_: script, test case

**Seed**:
The run-level value from which all mock behavior derives, making responses deterministic per agent and turn.
_Avoid_: nonce
