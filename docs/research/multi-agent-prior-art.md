# Prior art in multi-agent orchestration harnesses

Research for [#5](https://github.com/ericbstie/open-team-ai/issues/5). Surveyed against
primary sources: AutoGen/AG2, LangGraph (+ langgraph-supervisor), CrewAI, MetaGPT,
ChatDev, CAMEL, OpenAI Swarm and Agents SDK, smolagents, Magentic-One, Anthropic's
production research system, Cognition's Devin post-mortem, and Rust-native efforts
(rig, swarms-rs). Organized by **design question**, not by product; each section ends
with the implication for openteam. Vocabulary follows [CONTEXT.md](../../CONTEXT.md).

Date: 2026-07-16.

---

## 1. How do frameworks model teams and roles?

**Static personas dominate.** Nearly every framework fixes agents at construction time
as (system prompt + tool set + model): CrewAI `Agent`s with role/goal/backstory joined
into a `Crew` ([docs](https://docs.crewai.com/en/concepts/processes)); AutoGen
participants in a `RoundRobinGroupChat`/`SelectorGroupChat`
([teams tutorial](https://microsoft.github.io/autogen/stable/user-guide/agentchat-user-guide/tutorial/teams.html));
smolagents `managed_agents` with mandatory `name` and `description` attributes so a
manager can call them
([multi-agent example](https://huggingface.co/docs/smolagents/en/examples/multiagents));
OpenAI Agents SDK `Agent(instructions, tools, handoffs)`
([agents doc](https://openai.github.io/openai-agents-python/agents/)).

**Roles as process, not just persona.** MetaGPT's core claim is that role
specialization only pays off when roles are bound to Standardized Operating Procedures
— "encodes SOPs into prompt sequences" so each role produces *structured artifacts*
(PRD, design doc, code) rather than free chat
([MetaGPT paper](https://arxiv.org/abs/2308.00352)). ChatDev similarly splits every
phase into "atomic chats" between exactly two agents (instructor + assistant) with
inception-prompted personas ([ChatDev paper](https://arxiv.org/abs/2307.07924)).

**Dynamic re-roling is rare.** No surveyed framework pools workers and swaps their
specialties mid-run the way openteam's respecialization does. The closest analogues
are Swarm/Agents SDK handoffs — where "control" moves to a different persona but the
*agent instances* are all pre-declared — and LangGraph subgraphs compiled per node.
CAMEL generates role pairs per task at start, then keeps them fixed
([CAMEL paper](https://arxiv.org/abs/2303.17760)).

**Implication for openteam.** The fixed-pool + respecialization design (ADR-0003) is
genuinely novel territory — no prior art to copy, but two lessons transfer: (a) a
specialty must carry more than a persona name; MetaGPT shows the win comes from the
specialty prescribing *what artifact shape the agent produces*, so specialty prompts
should specify output expectations, not just identity; (b) smolagents' mandatory
`name`/`description` pair is the minimum a caller needs — the orchestrator's view of a
team agent should always include a one-line capability description of its current
specialty, refreshed on respecialization.

---

## 2. Who decides who acts next? (turn-taking / scheduling)

Four schemes recur:

- **Fixed rotation**: AutoGen `RoundRobinGroupChat` — deterministic, cheap, no
  intelligence
  ([teams](https://microsoft.github.io/autogen/stable/user-guide/agentchat-user-guide/tutorial/teams.html)).
- **LLM-selected speaker**: AutoGen `SelectorGroupChat` — a model reads shared history
  plus a `selector_prompt` (`{participants}`, `{roles}`, `{history}`) and names the
  next speaker; escape hatches `selector_func` (hard override) and `candidate_func`
  (narrow the candidate set), plus `allow_repeated_speaker` to control consecutive
  turns ([selector doc](https://microsoft.github.io/autogen/stable/user-guide/agentchat-user-guide/selector-group-chat.html)).
  Documented caveat: keep the selector prompt *simple*, especially for reasoning
  models — speaker selection is an easy thing to make flaky.
- **Supervisor routing**: LangGraph's supervisor pattern — a central node returns a
  `Command` (goto + state update) or calls a `create_handoff_tool` per worker;
  hierarchical = supervisors managing compiled supervisors
  ([langgraph-supervisor](https://github.com/langchain-ai/langgraph-supervisor-py),
  [LangChain multi-agent doc](https://docs.langchain.com/oss/python/langchain/multi-agent)).
  The full-mesh "network" pattern (anyone can call anyone) is documented but
  effectively warned against — supervisor sits between "network (chaos)" and
  "hierarchical (overkill)".
- **Peer handoff**: Swarm/Agents SDK — the current agent unilaterally transfers
  control by returning another `Agent` from a tool call; decentralized, one agent
  active at a time ([Swarm README](https://github.com/openai/swarm)).

Notably, **all of these are turn-based conversation schedulers** — the "scheduler" is
really "who speaks next in one shared chat". None schedules independently-running
agents against a work queue; CrewAI's hierarchical process comes closest, with a
`manager_llm`/`manager_agent` that "allocates tasks based on agent capabilities rather
than pre-assignment" and validates outputs
([processes](https://docs.crewai.com/en/concepts/processes)).

**Implication for openteam.** openteam's event-driven ticks (ADR-0007) — orchestrator
turns fired by pending input / unassigned work / idle agents, with team agents running
concurrently against the task board — is a real scheduler, not a speaker-selector, and
that is the right divergence: the speaker-selection literature exists precisely because
one-shared-chat systems have no other steering surface. Two things still transfer:
`allow_repeated_speaker`-style guards (don't let the same agent monopolize consecutive
scheduling decisions without cause) and AutoGen's caveat that any LLM-mediated
scheduling decision must be prompt-simple and have a mechanical override — in openteam
terms, keep tick-time decisions verb-shaped and let the runtime, not prose, enforce
eligibility.

---

## 3. How do messages route between agents?

Three routing models, in increasing discipline:

- **Broadcast-everything**: AutoGen AgentChat group chats — "each agent, during its
  turn, broadcasts its response to all other agents, ensuring that the entire team
  maintains a consistent context"
  ([teams](https://microsoft.github.io/autogen/stable/user-guide/agentchat-user-guide/tutorial/teams.html)).
  Simple, and the direct cause of context bloat: every agent pays tokens for every
  message.
- **Pub-sub with subscription filtering**: AutoGen Core routes by topic
  (`Topic_Type/Topic_Source`) with `TypeSubscription` mapping topics to agent IDs, plus
  direct RPC by agent ID for one-to-one
  ([topics doc](https://microsoft.github.io/autogen/stable/user-guide/core-user-guide/core-concepts/topic-and-subscription.html)).
  MetaGPT does the same at the cognitive level: a shared message pool that agents
  publish structured outputs into, with role-relevance subscription, because "sharing
  all information with every agent can lead to information overload"
  ([paper](https://arxiv.org/abs/2308.00352)).
- **Structured artifacts over chat**: MetaGPT explicitly avoids "unconstrained natural
  language as a communication interface" between roles — agents exchange documents
  with schemas, not dialogue. ChatDev constrains every exchange to a two-agent chat
  with a defined subtask, so messages can't fan out at all
  ([paper](https://arxiv.org/abs/2307.07924)).

Cognition's counterpoint: message-passing of *conclusions* is not enough — "share
context, and share full agent traces, not just individual messages", because "actions
carry implicit decisions, and conflicting decisions carry bad results"
([Don't Build Multi-Agents](https://cognition.com/blog/dont-build-multi-agents)).
Their failure examples are parallel subagents making incompatible implicit choices.

**Implication for openteam.** The bus design (direct / team / broadcast addressing,
ADR-0008) plus mandatory ingestion of every message into the knowledge store is the
right synthesis: MetaGPT's subscription-filtering validates *not* pushing every message
into every context, and the knowledge store answers Cognition's objection by making
full traces *retrievable* even when not *pushed*. Design rule to adopt: broadcast
should be the rare, expensive verb (team-scoped and direct messages the norm), and the
context policy — not the sender — decides what a recipient actually sees. Consider
MetaGPT's stronger move for task hand-offs: results posted to the board should be
structured (fields, artifacts) rather than prose summaries, so downstream agents
consume schema, not vibes.

---

## 4. How is orchestrator/agent context growth handled?

This is where practice diverged hardest from framework defaults:

- **Framework default = append-only transcript.** AutoGen teams keep "conversation
  history accumulat[ing] across runs unless explicitly reset" — the documented remedy
  is a manual `reset()`
  ([teams](https://microsoft.github.io/autogen/stable/user-guide/agentchat-user-guide/tutorial/teams.html)).
  ChatDev found the token limit was the direct cause of ~50% of its failures
  ([paper](https://arxiv.org/abs/2307.07924)).
- **Scoped forwarding.** langgraph-supervisor lets you choose per-worker whether the
  supervisor keeps `full_history` or only `last_message`, and provides
  `create_forward_message_tool` to pass a worker's answer through *without
  reprocessing* it — token control by structure
  ([README](https://github.com/langchain-ai/langgraph-supervisor-py)). LangChain's own
  benchmark of architectures credits subagent context isolation with "67% fewer tokens
  overall" versus keeping everything in one context
  ([multi-agent doc](https://docs.langchain.com/oss/python/langchain/multi-agent)).
- **Ledgers instead of transcripts.** Magentic-One's orchestrator maintains a **Task
  Ledger** (facts, guesses, plan) and a **Progress Ledger** (per-step self-reflection),
  and *re-derives* its working state from them rather than from raw history
  ([Magentic-One doc](https://microsoft.github.io/autogen/stable/user-guide/agentchat-user-guide/magentic-one.html)).
- **Compression as a first-class component.** Cognition runs "a dedicated LLM whose
  job is to compress action history and decisions" for long horizons
  ([blog](https://cognition.com/blog/dont-build-multi-agents)). Anthropic's research
  system hit "context overflow: reaching token limits (200,000) without plan
  retention" and fixed it by persisting the plan to external memory so it survives
  truncation ([engineering post](https://www.anthropic.com/engineering/multi-agent-research-system)).
- **ChatDev's version evolution**: across phases, discard earlier code versions and
  retain only the latest, deliberately restricting visibility to current state to
  prevent hallucination accumulation ([paper](https://arxiv.org/abs/2307.07924)).

**Implication for openteam.** ADR-0004 (rebuild the orchestrator's context every turn
from token-budgeted, relevance-ranked sections) is the aggressive, designed-in version
of what every production system retrofitted after failure. Magentic-One's ledger split
maps cleanly onto openteam's sections: the board digest *is* the task ledger; consider
adding an explicit "progress digest" section (what changed since last tick, current
stall assessment) as its progress-ledger counterpart. ChatDev's version-evolution
supports a "latest-wins" rule in context assembly: rank the current state of an
artifact far above its history. The knowledge store plays Cognition's compressor role
— but only if entries are written as *decisions and rationale*, not raw chatter, so
retrieval surfaces the implicit decisions Cognition warns get lost.

---

## 5. How is shared memory / RAG done?

- **AutoGen**: a `Memory` protocol (`add`, `query`, `update_context`, `clear`) where
  `update_context` injects retrieved entries into the agent's model context before
  each step via a `SystemMessage`; backends range from `ListMemory` (chronological) to
  `ChromaDBVectorMemory`/`RedisMemory`
  ([memory doc](https://microsoft.github.io/autogen/stable/user-guide/agentchat-user-guide/memory.html)).
  Memory is per-agent; sharing means handing agents the same store instance.
- **CrewAI**: one crew-level `Memory` shared by all agents (`memory=True`), with
  LLM-inferred scope/importance on save and composite scoring (semantic similarity +
  recency + importance) on recall; hierarchical scopes (`/project/alpha`,
  `/agent/researcher`) let agents get scoped views of shared memory
  ([memory doc](https://docs.crewai.com/en/concepts/memory)).
- **LangGraph**: `checkpointer` (thread-scoped state persistence) vs `store`
  (cross-thread long-term memory), both injected at compile time
  ([langgraph-supervisor](https://github.com/langchain-ai/langgraph-supervisor-py)).
- **MetaGPT**: the shared message pool doubles as shared memory — structured artifacts
  are globally published, subscription-filtered on read
  ([paper](https://arxiv.org/abs/2308.00352)).
- **Anthropic**: external memory as *overflow protection* — the lead agent writes its
  plan to memory so it survives context truncation
  ([post](https://www.anthropic.com/engineering/multi-agent-research-system)).

**Implication for openteam.** The run-scoped knowledge store is well-supported by
prior art, and two scoring details transfer directly: (a) CrewAI's retrieval formula —
similarity *plus recency plus importance*, not cosine alone — matters in a fast-moving
run where a stale-but-similar entry can beat a fresh-and-critical one; provenance
(agent, turn, task) should feed ranking. (b) AutoGen's `update_context` placement —
retrieved knowledge enters as a distinct labeled section of assembled context, which
is exactly openteam's context-assembly model; keep retrievals clearly attributed so
agents can weigh them against fresh messages.

---

## 6. How is work delegated and tracked?

- **Chat-mediated delegation** (AutoGen group chats, CAMEL, ChatDev): the "task" lives
  in conversation; nothing tracks it. This is where livelock and drift originate.
- **Task-object delegation** (CrewAI): `Task` objects with `context` dependencies,
  executed sequentially or allocated by a manager agent that "reviews outputs and
  assesses task completion" ([processes](https://docs.crewai.com/en/concepts/processes)).
- **Ledger-tracked delegation** (Magentic-One): the orchestrator's Task Ledger + plan
  assigns steps to specialist agents; the Progress Ledger checks completion per step
  ([doc](https://microsoft.github.io/autogen/stable/user-guide/agentchat-user-guide/magentic-one.html)).
- **Contract-quality task descriptions** (Anthropic): early versions gave subagents
  vague instructions ("research the semiconductor shortage") and got duplicated,
  divergent work; the fix was task descriptions carrying "objectives, output formats,
  tool guidance, and clear boundaries", plus explicit **effort scaling rules** —
  "simple fact-finding needs one agent with 3-10 tool calls; direct comparisons 2-4
  subagents with 10-15 calls each"
  ([post](https://www.anthropic.com/engineering/multi-agent-research-system)).

**Implication for openteam.** The task board (with team claim-eligibility) is already
the strongest pattern in this space. Adopt Anthropic's two refinements as board
schema, not prompt lore: every task should carry an objective, an expected output
form, and an effort budget hint; and the orchestrator's decomposition prompt should
encode effort-scaling norms so a trivial goal doesn't fan out into a full team. The
manager-validates-output step (CrewAI, Magentic-One) supports making task completion
reviewable — a completed task's result posted to the board is something the
orchestrator can reopen, which openteam already implies via reassign/cancel verbs.

---

## 7. How do runs terminate?

- **Conversational stop-words**: AutoGen's `TextMentionTermination("TERMINATE")` — the
  canonical example — is always documented *paired with* `MaxMessageTermination`
  "to prevent infinite loops"; eleven composable conditions exist
  (`TokenUsageTermination`, `TimeoutTermination`, `HandoffTermination`,
  `SourceMatchTermination`, `ExternalTermination`, `FunctionCallTermination`, …)
  combinable with `&`/`|`
  ([termination tutorial](https://microsoft.github.io/autogen/stable/user-guide/agentchat-user-guide/tutorial/termination.html)).
  The sheer size of that menu is the tell: no single signal is trusted.
- **Prompt-embedded conditions**: CAMEL bakes termination conditions and constraints
  into both system prompts, and still documents infinite loops as a failure mode
  ([paper](https://arxiv.org/abs/2303.17760),
  [NeurIPS version](https://proceedings.neurips.cc/paper_files/paper/2023/file/a3621ee907def47c1b952ade25c67698-Paper-Conference.pdf)).
- **Structural exhaustion**: Swarm ends when "no new function calls" occur, with
  `max_turns` as cap ([README](https://github.com/openai/swarm)); Agents SDK uses
  `max_turns` plus `tool_use_behavior` stop rules and even auto-resets `tool_choice`
  to prevent "infinite loops" of forced tool calls
  ([agents doc](https://openai.github.io/openai-agents-python/agents/)); smolagents
  caps `max_steps` per agent.
- **Completion-checked**: Magentic-One's inner loop explicitly "checks whether the
  task is completed" against the ledger before stopping
  ([doc](https://microsoft.github.io/autogen/stable/user-guide/agentchat-user-guide/magentic-one.html)).

**Implication for openteam.** ADR-0006 (verb-gated `finish_run` that the runtime
*validates against the board* — refusing while open/claimed tasks remain — plus hard
caps with distinct exit codes) is strictly stronger than everything surveyed: it makes
termination a checked state transition rather than a detected utterance. The prior art
adds one nuance worth keeping: AutoGen's `ExternalTermination` stops "after the current
agent completes its turn, maintaining state consistency" — openteam's cap-triggered
force-termination should likewise land on turn boundaries so partial artifacts are
always coherent.

---

## 8. Parallelism: limits and pitfalls

- Anthropic runs subagents in parallel and documents the cost: multi-agent uses
  "approximately 15× more tokens than chat", over-spawning ("agents created 50
  subagents for simple queries") and duplicated work were real production failures,
  fixed by prompted effort budgets and explicit division of labor
  ([post](https://www.anthropic.com/engineering/multi-agent-research-system)).
- Cognition argues parallel subagents are unreliable *when their work interacts*,
  because they can't see each other's implicit decisions; they accept parallelism only
  where tasks are truly independent
  ([blog](https://cognition.com/blog/dont-build-multi-agents)).
- LangChain's architecture comparison treats parallelism as a router-pattern property
  and prices each architecture in calls/tokens
  ([doc](https://docs.langchain.com/oss/python/langchain/multi-agent)).
- swarms-rs exposes `ConcurrentWorkflow` for fan-out but leaves interaction discipline
  to the user ([repo](https://github.com/The-Swarm-Corporation/swarms-rs)).

**Implication for openteam.** The two-sided rule from prior art: parallelism pays only
when (a) tasks are decision-independent and (b) each carries an explicit contract.
openteam's board is the natural enforcement point — the orchestrator should only leave
tasks simultaneously claimable when they don't share an artifact, and the meta-agent's
mechanical "tune effective parallelism" directive (ADR-0005) is exactly the throttle
Anthropic had to bolt on via prompts. Token multiplication (15×) also justifies the
mock-first testing strategy: parallelism bugs are cheap to find offline.

---

## 9. Self-monitoring / meta layers

Thin across all frameworks — this is openteam's most differentiated area:

- **Magentic-One** is the only real prior art: the orchestrator's outer loop is a
  monitor — the Progress Ledger self-reflects each step, and "if the Orchestrator
  finds that progress is not being made for enough steps, it can update the Task
  Ledger and create a new plan" — count-based stall detection triggering a replan
  ([doc](https://microsoft.github.io/autogen/stable/user-guide/agentchat-user-guide/magentic-one.html)).
  But monitor and commander are the *same agent* — no independent observer.
- **OpenAI Agents SDK guardrails** run "on user input in parallel to the agent
  running, and on the agent's output" — independent, but stateless per-call validators,
  not process observers ([doc](https://openai.github.io/openai-agents-python/agents/)).
- **CrewAI's** hierarchical manager validates task outputs — quality control, not
  process improvement.
- **MetaGPT's executable feedback** — run the code, feed failures back, retry to a
  limit — is self-monitoring grounded in an external oracle rather than an LLM opinion
  ([paper](https://arxiv.org/abs/2308.00352)).

**Implication for openteam.** Persistent meta-agents observing events/metrics with
two-tier directive authority (ADR-0005) goes beyond all surveyed systems. Steal from
Magentic-One the *shape* of stall detection: cheap, count-based ("no board progress in
N ticks", "same agent pair exchanged M messages without a task-state change") rather
than asking an LLM "are we stuck?" — the counters trigger, the meta-agent reasons.
MetaGPT's lesson: wherever a mechanical oracle exists (tests, schema validation, board
state), prefer it to LLM judgment as the directive trigger. And the guardrails model
supports keeping mechanical directives runtime-applied — validation that must always
happen shouldn't wait on any agent's turn.

---

## 10. Documented failure modes — the catalogue

| Failure mode | Documented where | Mitigation used there |
|---|---|---|
| **Chat-loop livelock** — agents thanking each other / saying goodbye forever, "aware they are stuck in a loop but unable to break out" | [CAMEL paper](https://arxiv.org/abs/2303.17760) | Prompt-level termination conditions + message caps |
| **Role flipping** — assistant starts instructing the user-agent; instruction repetition, flake replies | [CAMEL paper](https://arxiv.org/abs/2303.17760) | Inception prompting; forbid the assistant to ask questions |
| **Cascading hallucination** — errors compound down a naive agent chain | [MetaGPT paper](https://arxiv.org/abs/2308.00352) | SOPs, structured artifacts, intermediate verification, executable feedback |
| **Context collapse / token exhaustion** — ~50% of ChatDev's failures; Anthropic's 200k overflow losing the plan | [ChatDev paper](https://arxiv.org/abs/2307.07924), [Anthropic post](https://www.anthropic.com/engineering/multi-agent-research-system) | Version evolution (latest-only); external plan memory; compression |
| **Runaway delegation** — 50 subagents for a simple query; duplicated searches | [Anthropic post](https://www.anthropic.com/engineering/multi-agent-research-system) | Effort-scaling rules; detailed task contracts |
| **Conflicting implicit decisions** in parallel branches | [Cognition blog](https://cognition.com/blog/dont-build-multi-agents) | Share full traces; single decision-maker; restrict parallelism to independent work |
| **Message floods / information overload** from broadcast-everything | [MetaGPT paper](https://arxiv.org/abs/2308.00352), AutoGen broadcast model | Subscription filtering by role relevance |
| **Stalls without progress** (agent keeps acting, nothing advances) | [Magentic-One](https://microsoft.github.io/autogen/stable/user-guide/agentchat-user-guide/magentic-one.html) | Progress Ledger; stall counter → replan |
| **Forced-tool infinite loops** (`tool_choice` re-triggering the same call) | [Agents SDK](https://openai.github.io/openai-agents-python/agents/) | Automatic `tool_choice` reset after a tool call |
| **Endless searching for nonexistent results** | [Anthropic post](https://www.anthropic.com/engineering/multi-agent-research-system) | Effort budgets; interleaved self-evaluation |

**Implication for openteam.** Each row has a structural answer already in the ADRs;
the design work is making sure the mock's scenario suite *reproduces each row* as a
test: a scripted livelock (two agents messaging without board changes), a scripted
runaway decomposition (orchestrator floods the board), a scripted stall (claimed task,
idle agent), a context-pressure scenario (knowledge store much larger than any token
budget). Prior art supplies the failure taxonomy; the mock makes it a regression
suite — something no surveyed framework has.

---

## 11. Rust-native prior art

- **rig** ([0xPlaygrounds/rig](https://github.com/0xPlaygrounds/rig)): the mature
  building-block layer — unified `CompletionClient` over 20+ providers, agent
  abstraction (preamble + tools), 10+ vector-store integrations under one interface
  (LanceDB, Qdrant, SQLite, in-memory) for RAG. Explicitly *no* multi-agent
  orchestration — "the primitives compose cleanly" but orchestration is yours.
- **swarms-rs** ([The-Swarm-Corporation/swarms-rs](https://github.com/The-Swarm-Corporation/swarms-rs)):
  claims multi-agent orchestration (`ConcurrentWorkflow`, `SequentialWorkflow`, graph
  workflows, `max_loops`, `enable_autosave` state checkpointing) but the model is
  workflow fan-out/aggregation, not persistent coordinating agents; no realtime bus,
  no shared memory beyond aggregated results.
- Smaller efforts (e.g. [rs-graph-llm](https://github.com/a-agmon/rs-graph-llm),
  [fcn06/swarm](https://github.com/fcn06/swarm)) are graph-workflow or A2A/MCP-standard
  experiments.

**Implication for openteam.** Nothing in Rust occupies openteam's niche (persistent
orchestrator + meta-agents + pooled respecialized workers + bus + shared vector store,
offline-testable). The ecosystem lesson is about layering: rig succeeds by staying a
primitives crate. openteam should keep its own layers separable in the same spirit —
runtime/scheduling, coordination verbs, and the mock as distinct modules — and could
plausibly consume a crate like rig for the (non-v1) real-LLM backend rather than
hand-rolling providers.

---

## Top transferable lessons (summary)

1. **Termination must be structural, not conversational.** Every chat-based framework
   pairs its stop-word with hard caps because utterance-detection fails. openteam's
   board-validated `finish_run` + caps is stronger than anything surveyed; keep it.
2. **Never broadcast everything.** MetaGPT's subscription-filtered message pool is the
   proven answer to context bloat and message floods; openteam's addressed bus +
   knowledge-store ingestion is the same shape — make broadcast rare and let context
   policy, not senders, decide what agents see.
3. **Rebuild or compress context; never just append.** Ledgers (Magentic-One),
   compressor models (Cognition), external plan memory (Anthropic), latest-version-only
   visibility (ChatDev) all converge on ADR-0004's rebuilt context; add a
   progress-digest section and rank current artifact state above history.
4. **Delegation needs contracts and effort budgets.** Vague task handoffs produced
   duplicated and divergent work at Anthropic; put objective, expected output form,
   and effort hints in the task schema, and only allow parallel claims on
   decision-independent tasks.
5. **Monitoring works when it's cheap counters triggering an empowered reasoner.**
   Magentic-One's stall-counter → replan loop is the proven core; openteam's two-tier
   directives extend it with an independent observer — trigger on mechanical signals
   (board progress, message/turn ratios), prefer external oracles to LLM judgment, and
   turn the whole failure catalogue (§10) into deterministic mock scenarios.
