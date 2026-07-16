# OpenTeam

The ubiquitous language for `openteam`, an offline LLM harness that runs a dynamically
re-specialized team of agents — coordinated in realtime by a persistent orchestrator and
self-monitored by meta-agents — against a deterministic mock of the OpenAI API.

## Language

### Agents & roles

**Agent**:
An LLM-driven actor in a run. Every agent — orchestrator, meta-agent, or team agent — runs on the same tool-calling turn loop.
_Avoid_: bot, worker

**Agent handle**:
The short positional identifier an agent keeps for the whole run — `orchestrator`, `meta-N`, or `agent-N` — the one name used everywhere the agent is named: events, messages, the report, and the wire `user` field.
_Avoid_: UUID, agent number

**Role**:
The control class an agent belongs to: Orchestrator, Meta-agent, or Team agent. Roles differ only by system prompt, tool registry, and context policy.
_Avoid_: type, kind

**Prompt skeleton**:
The harness-owned system prompt of a role — a shared preamble plus a role block — static for the run; its only variable slot is the team agent's specialty.
_Avoid_: prompt template, persona prompt

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
The persona of a team agent — a slug name, a one-line description for the orchestrator's roster, and a freeform focus — authored by the orchestrator and rendered into the team-agent prompt skeleton.
_Avoid_: role, profession

**Generalist**:
The harness-shipped default specialty every team agent boots with; the only built-in specialty — all others are orchestrator-authored.
_Avoid_: unspecialized, blank agent

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

**Claim**:
A team agent taking exclusive ownership of an open task — at most one claimed task per agent, team eligibility checked at claim time, first claim wins.
_Avoid_: assign, pick up, lock

**Release**:
A claimant returning its claimed task to open, optionally with a reason. Repeated releases are a meta-agent-visible signal; there is no failed task state.
_Avoid_: drop, abandon, fail

**Unassign**:
The orchestrator forcibly returning a claimed task to open — the reallocation and pre-respecialization move.
_Avoid_: revoke, reclaim

**Board digest**:
The token-budgeted summary of the task board that context assembly gives an agent each turn — the full board for the orchestrator, a claimed-plus-eligible slice for team agents.
_Avoid_: board dump, task list

**Coordination verb**:
A tool exposed to agents that acts on the run's shared state — messages, board, knowledge, agent lifecycle. The only kind of tool in v1.
_Avoid_: action, command

**Tool registry**:
The fixed set of coordination verbs a role may call, dispatched by name to a plain async handler; one registry per role, rendered verbatim into every request's `tools` array.
_Avoid_: toolbox, toolkit

**Tool outcome**:
The three-way result of dispatching a coordination verb — `ok` (executed), `rejected` (a well-formed call the domain refused), or `invalid` (a schema/parse fault) — carried as the string content of the wire's single `tool` reply. Only a turn whose every call is `invalid` feeds the malformed-park counter; `rejected` resets it like `ok`.
_Avoid_: tool result, return code, error

**Message**:
A realtime communication between agents — addressed direct, to a team, or broadcast — always ingested into the knowledge store at acceptance, delivered between turns via the recipient's mailbox, and never dropped.
_Avoid_: chat, notification

**Address**:
The routing scope of a message: one agent (direct), a team's members at acceptance time, or broadcast to the orchestrator and all team agents — meta-agents observe traffic through events rather than receiving broadcasts.
_Avoid_: recipient, target

**Mailbox**:
The per-agent ordered queue of accepted-but-undelivered messages, drained oldest-first into the agent's next turn under a token budget with carryover; unbounded and lossless.
_Avoid_: inbox, channel, buffer

**Directive**:
A meta-agent's process-improvement instruction. Mechanical directives are applied directly by the runtime; judgment directives go to the orchestrator, which must act or decline with a logged reason.
_Avoid_: command, suggestion

**Directive tier**:
Which authority path a Directive takes: `mechanical` (applied directly by the runtime, its emitter verb returning the applied effect) or `judgment` (enqueued to the orchestrator, its emitter verb returning a directive id). The meta-agent emits directives only through tier-typed verbs — never messages or knowledge writes.
_Avoid_: level, kind, class

**Directive outcome**:
What becomes of an emitted Directive: a mechanical one is `applied` at emit time; a judgment one stays pending in the orchestrator's never-dropped Directives section until the orchestrator either fulfills it (acting while citing its directive id) or declines it with a reason — both recorded as events the meta-agent reads. There is no silent timeout.
_Avoid_: directive result, ack, response

### Knowledge & context

**Knowledge store**:
The run-scoped, in-process shared vector store that every agent reads and writes; created fresh each run.
_Avoid_: vector DB, memory

**Knowledge entry**:
One stored item in the knowledge store — text plus its embedding and provenance (author, source event, kind). One entry per ingested item; never chunked.
_Avoid_: document, record

**Knowledge kind**:
What produced a knowledge entry: an ingested Message body, a task-completion result, or a Note. The store's only filter/provenance dimension — there are no freeform tags.
_Avoid_: type, category, label

**Note**:
A passive knowledge contribution — an agent recording a fact into the store (via `write_knowledge`) with no mailbox delivery, discoverable later only by search. The passive counterpart to a Message's push.
_Avoid_: memo, annotation, memory

**Knowledge retrieval**:
A top-k cosine search of the knowledge store, issued either explicitly by an agent (`search_knowledge`) or automatically by context assembly's retrievals section each turn.
_Avoid_: query, lookup, recall

**Context assembly**:
Rebuilding an agent's prompt each turn from token-budgeted, relevance-ranked sections instead of appending to a transcript — every role, team agents included. Emits exactly two messages: a `system` skeleton and a `user` message carrying the sections.
_Avoid_: context patching, compaction

**Context policy**:
The per-role value — an ordered list of context sections, each with a token budget, an allocation/degradation priority, and a drop rule — that the single context assembler interprets to build an agent's prompt. Data, not a trait: inspectable, loggable, and testable. One per role (`orchestrator`, `team_agent`, `meta_agent`).
_Avoid_: memory policy

**Context section**:
A labeled, individually-budgeted block of assembled context — Goal, Board digest, Knowledge retrievals, Fresh messages, Directives, Claimed task, Recent-activity window — rendered as one `##`-headed markdown block inside the single assembled `user` message. Presentation order is fixed per policy and is distinct from allocation priority.
_Avoid_: context block, chunk

**Section budget**:
A context section's token cap plus its allocation/degradation priority. The assembler allocates the assembly pool (context window minus skeleton minus reserved output) across sections in priority order and, on overflow, degrades bottom-up; Goal and Directives are never dropped, and any oldest-first section always delivers at least its single oldest item.
_Avoid_: token limit, quota

**Recent-activity window**:
A team agent's budget-capped sliding window of its own recent turns' output (actions and tool results), rendered as text — the only private context bridging its reasoning across turns within one assignment. Oldest-dropped under budget, reset at each assignment boundary, wiped on respecialization. There is no persistent per-agent transcript; durable output lives in the board, messages, and knowledge store.
_Avoid_: transcript, history, scratchpad

**Run-health line**:
The compact one-line steering summary (throughput, agent utilization, mailbox pressure) folded into the orchestrator's board-digest section — the lightest of the Metrics module's three projections, distinct from the meta-agent's full metrics digest, which is where heavy process metrics are reasoned on.
_Avoid_: metrics section, dashboard

**Metrics**:
The single runtime-owned accumulator that folds the event log into process measurements — one computation feeding three projections (the orchestrator's run-health line, the meta-agent's metrics digest, and the report's run summary), so a run with no meta-agents still measures itself.
_Avoid_: telemetry, stats, dashboard

**Metrics digest**:
The meta-agent's full process view projected from Metrics — throughput, latency, task churn, utilization, mailbox pressure, token spend, faults, message and knowledge volume, stall counters, and directive outcomes — with every time-like value counted in deterministic event/tick units, never wall-clock. The heavy counterpart to the run-health line.
_Avoid_: metrics section, dashboard, stats

### Runtime & lifecycle

**Agent state**:
A team agent's lifecycle position: Idle (no claimed task), Working (exactly one claimed task), or Asleep (descheduled until an explicit wake). Orthogonal to whether a turn is currently in flight. The orchestrator and meta-agents are control-plane and hold no such state.
_Avoid_: status, mode

**Turn**:
One reasoning episode by one agent — context assembled once, then a capped loop of completion → execute tool calls → feed results back — ending when the agent yields (a completion with no tool calls) or the per-turn tool-iteration cap is hit. The atomic unit of scheduling and lifecycle; may span several completions (each its own Call sequence entry).
_Avoid_: step, round

**Tick**:
One orchestrator turn, fired when the previous turn is done and there is pending input, unassigned work, or idle agents with open work.
_Avoid_: cycle, poll, scheduler pass

**Meta cadence**:
The rule that dispatches a meta-agent's turn: a coalesced count of unobserved events crossing a fixed internal threshold, plus an immediate priority wake on high-signal events (park, repeated release, decline, liveness nudge). Event-driven like every other turn, and control-plane, so it takes no parallel permit.
_Avoid_: meta poll, monitor interval, meta tick

**Sleep**:
The state (Asleep) in which the scheduler will not run an agent until an explicit wake. Entered only deliberately and only from Idle — by the sleep verb (the orchestrator on an idle agent, or a team agent on itself) or a mechanical meta-directive. The automatic malformed-fault entry into this same state is Park.
_Avoid_: suspend, pause

**Wake**:
Returning a sleeping or parked agent to schedulability — always explicit (orchestrator verb or mechanical meta-directive), never automatic and never self-issued. Restores the prior state: Working with its still-claimed task, else Idle.
_Avoid_: resume

**Park**:
The automatic entry into Sleep after K = 3 consecutive malformed turns. Emits a distinct meta-visible event and preserves any claimed task; recovery is by explicit Wake (or the orchestrator unassigning the task to reallocate). A successful turn resets the consecutive-malformed counter.
_Avoid_: crash, fail, kill

**Malformed turn**:
A turn that emitted at least one tool call and whose every call returned `invalid` (a schema/parse fault) — none `ok`, none `rejected`. A turn with zero tool calls is a clean yield, never malformed; a turn with one or more `ok` or `rejected` outcomes resets the consecutive-malformed counter to zero (a `rejected` domain refusal is a well-formed call, so it counts as behaving correctly — see Tool outcome).
_Avoid_: bad turn, error turn

**Liveness nudge**:
A coarse (~500 ms) watchdog asserting the invariant that a quiescent system has finished the board. Fires only when no turn is in flight, every team agent is Idle or Asleep, the board is unfinished, and the orchestrator has no pending input; it emits a distinct event and forces one orchestrator tick (the deadlock breaker — it never auto-wakes team agents). Insurance, not a mechanism: in correct operation it never fires, so its firing is a scheduling bug surfacing loudly.
_Avoid_: heartbeat, timer

**Event**:
An append-only record of something that happened in a run; the event log is what tests, meta-agents, and the report read.
_Avoid_: log entry, audit record

**Run artifacts**:
The persisted output directory of a run: the event log, the final board, the knowledge entries, and the report.
_Avoid_: output dir, results

### Mock & determinism

**Mock**:
The OpenAI-schema server that plays the model for every agent — the default and only tested LLM backend in v1. Served over **real loopback HTTP** (binds `127.0.0.1:0`; the OS-assigned port is read back into `LlmConfig.base_url`), so the client code path is byte-identical to a real endpoint, and reached the same way whether embedded in a run or run standalone (`openteam mock serve`) — both mount the identical `build_router()`. **Stateless per request**: every response is a pure function of the request plus its identity channels (`user`, `X-OpenTeam-Call-Seq`, `X-OpenTeam-Seed`), so it holds no per-run state and concurrent runs are isolated purely by their seed (ADR 0019).
_Avoid_: simulator, stub, fake

**Behavior model**:
The mock's engine for deciding a chat response, reached through the **synchronous** `BehaviorModel::chat(req, identity) -> ChatDecision` seam. The built-in one is a **pure function of `(request, identity, seed)` with zero run-state** that reads the rendered world (board digest, claimed task, recent-activity window, fresh messages, directives) to drive any prompt through the Behavior arc, keying identity only on the `user` field and headers — never on content (ADR 0021); a Scenario player is the second adapter (#20), selected when `AppState`'s optional scenario is present. It decides only *what to say* — the mock server owns the schema-valid envelope (ADR 0019).
_Avoid_: script

**Prompt-legibility contract**:
The two-halves-of-one-contract agreement between what the harness renders and what the mock reads: keying **identity** on rendered content is forbidden (identity is the `user` field + `X-OpenTeam-*` headers, ADR 0008), while reading the rendered **world state** to choose the next plausible action is the mock's whole job and is not content-sniffing — the board rendered in the request *is* the arc's stateless memory. Its concrete surface is the **stable, machine-parseable line grammars** of the context sections (board digest, claimed task, recent-activity window, fresh messages, directives/outcomes) plus the `tools` array (ADR 0013): #18 pins the grammars, the #13 assembler renders to exactly them, and #23 tests the pairing. Carries **no version marker** in v1 (harness and mock build in lockstep; ADR 0021).
_Avoid_: prompt protocol, parser contract

**Behavior arc**:
The bounded decompose → work → converge state machine the built-in Behavior model re-derives from the rendered world each completion, terminating by construction. Orchestrator: seeded task budget `T = f(seed) ∈ [1, 8]` hard-capped by the visible board count (`n==0` decompose, pending directive resolve, non-terminal tasks yield, all-terminal `finish_run`). Team agent: claim an eligible Open task, do `W_task ∈ [1..3]` work-actions counted from its recent-activity window, then `complete_task` — completing when actions-seen ≥ `W_task` *or* the window is degraded (degradation is a shortcut to completion, never a block). Meta-agent: emit ≤1 seeded directive (bounded by its own directive-outcomes slot), else yield. Every role emits the mandatory no-tool-call yield when nothing plausible remains, so turns end (ADR 0021).
_Avoid_: script, storyline, playbook

**Chat decision**:
What the behavior seam returns — the assistant `ResponseMessage` (text or `tool_calls`) plus its `FinishReason`, and nothing else. The mock server wraps it into a valid `ChatCompletionResponse`, owning `id` (derived deterministically from `user`+call-seq+seed), `created` (from the injected `Clock`), the `model` echo, `choices[]`, and `usage` (wire free-fns) — so "every response is schema-valid OpenAI" is **structural**: the behavior model never touches the envelope, so it cannot emit an invalid one (ADR 0019).
_Avoid_: response, completion

**Mock embedding**:
The deterministic, seed-independent vector the mock returns for text at `/v1/embeddings` — a lexical-overlap projection, so text sharing tokens lands near in cosine space. Identical text always yields the identical vector. Computed directly as a fixed wire function, **bypassing the behavior seam** and any scenario override (ADR 0019).
_Avoid_: real embedding, semantic vector

**Scenario**:
A fixture file that overrides the built-in behavior model with scripted responses for tests.
_Avoid_: script, test case

**Seed**:
The run-level value from which all mock behavior derives, making responses deterministic per agent and call sequence.
_Avoid_: nonce

**Call sequence**:
A per-agent monotonic counter incremented on every `/v1/chat/completions` an agent issues, carried in an `X-OpenTeam-*` header. With the `user` field and the seed it forms the mock's determinism key, unique per completion even when one turn spans several completions — so a turn's up-to-eight completions never collide to a single response.
_Avoid_: turn number, request id, nonce

**Wire contract**:
Everything the harness and the mock must agree on — the OpenAI wire subset, the identity grammar, the auxiliary headers, the seed, and token counting. The mock's only knowledge of the harness is the request it is currently reading.
_Avoid_: shared types, common code

### Wire & transport

**Wire type**:
A Rust struct or enum in `openteam-wire` mirroring one shape of the OpenAI wire subset — the chat-completions request/response, the embeddings request/response, the error body — each deriving both `Serialize` and `Deserialize` because the harness and the mock sit on opposite ends of the same type. Request optional params are `Option` + `skip_serializing_if` (omit when absent); response required-but-nullable keys (`content`, `refusal`, `logprobs`) are plain `Option` that serialize as explicit `null`; `arguments` is a JSON `String`; unknown-field posture is asymmetric (embeddings request denies, chat and responses ignore).
_Avoid_: DTO, model, schema struct

**LLM client**:
The harness-side transport seam — `LlmClient`, an `#[async_trait]`, `Send + Sync` trait in `openteam-core` — that speaks the wire subset to an endpoint: the in-process mock by default, a real OpenAI-compatible endpoint via `--llm-base-url` (config-only, untested; ADR 0001). Transport-agnostic and shared as one stateless `Arc`; its two adapters are the reqwest HTTP client and an in-memory fake for runtime unit tests.
_Avoid_: LLM backend, provider, API client

**Agent channel**:
A cheap per-agent handle (`AgentChannel`) over the shared `LlmClient` transport that owns the agent's monotonic call-sequence counter (an `AtomicU64`, `fetch_add` per completion) and renders its `user` field. Monotonic for the whole run and never reset on respecialization — only the specialty slug in the rendered `user` field changes — so no two of an agent's completions ever collide to one determinism key.
_Avoid_: per-agent client, connection

**Wire identity**:
What an `AgentChannel` hands the transport per completion — the rendered `user` field (the ADR 0012 agent-handle grammar) plus the auxiliary `X-OpenTeam-Call-Seq` and `X-OpenTeam-Seed` header values — the whole of what ADR 0008 lets identity ride on. The adapter stamps `user` into the schema-pure body and the two channels into headers; embeddings carry none (mock embeddings are seed-independent).
_Avoid_: credentials, auth, session
