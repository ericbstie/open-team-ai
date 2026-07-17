# PROTOTYPE — dry-run transcript of the canonical demo run

> **Throwaway primary source** for wayfinder ticket
> [#22](https://github.com/ericbstie/open-team-ai/issues/22). Hand-written (no Rust
> exists yet) to answer one question: **does the whole protocol hang together
> end-to-end?** It traces a canonical small run against the ADRs, rendered in the
> decided formats, and actively hunts cross-ticket contradictions. Per `/prototype`
> this file stays under `docs/prototypes/`; the *validated* line-grammar decisions
> get folded into ADR 0016/0021 (see the follow-ups flagged at the bottom).

**Invocation traced**

```
openteam run "Write a short onboarding guide for new contributors" \
  --seed 42 --agents 3 --meta-agents 1
```

Resolved: 1 orchestrator, 3 team agents (`agent-1..3`, boot `generalist`), 1
meta-agent (`meta-1`), one team `t1`. `--parallel` defaults to `--agents` = 3.
Seed 42 fixed by hand (a real run draws random-per-run, ADR 0024). Caps: none set.

**Seeded dial values** (chosen by hand to stand in for the `f`/`g` draws of ADR 0021):
`T = f(42) = 2` tasks; `W_task(agent-1, task 1) = 2`, `W_task(agent-2, task 2) = 1`.

**Verdict at a glance.** The protocol is **COHERENT and map-closure-ready**, *modulo*
one ruling and four ratifications (see [§9](#9-contradictions--ambiguities-found)):
- **1 genuine contradiction** — the built-in meta arc emits exactly **one** directive
  per meta-agent per run, so a `--meta-agents 1` demo **cannot** show both a judgment
  *and* a mechanical directive. (The brief asked for both.)
- **1 spec naming drift** — `meta-0` (ADR 0020) vs `meta-1` (ADR 0012/0022/0023).
- **1 spec ambiguity** — one shared id counter vs three, which changes what `events.jsonl` renders.
- **2 format pins** the transcript had to make — the section **line-grammars** and the
  arc's **within-turn** behaviour — that ADR 0016/0021 left open.

Everything else in the trace re-derives cleanly and statelessly from the rendered
requests. The stateless-arc replay check ([§10](#10-stateless-arc-replay-check)) passes.

---

## Table of contents

1. [The pinned line-grammars (legibility-contract surface)](#1-the-pinned-line-grammars)
2. [The three tool registries (`tools` arrays)](#2-the-three-tool-registries)
3. [The trace, tick by tick](#3-the-trace-tick-by-tick)
4. [Full request/response pair A — orchestrator decompose](#pair-a)
5. [Full request/response pair B — agent-3 lost-claim race (the `rejected` path)](#pair-b)
6. [Full request/response pair C — meta emits the judgment directive](#pair-c)
7. [Full request/response pair D — orchestrator resolves the directive with a cite](#pair-d)
8. [The complete `events.jsonl`](#8-the-complete-eventsjsonl) · [`board.json`](#board-json) · [`knowledge.jsonl`](#knowledge-jsonl)
9. [Contradictions & ambiguities found](#9-contradictions--ambiguities-found)
10. [Stateless-arc replay check](#10-stateless-arc-replay-check)
11. [`report.md` (== stdout)](#11-reportmd--stdout)

---

## 1. The pinned line-grammars

ADR 0021 says #18 *pins* the section line-grammars, #13 renders to them, #23 tests
them — and that ADR 0016 was left rendering-agnostic and is "retroactively
constrained." **No ADR actually writes the grammars down.** This transcript pins
them (concrete proposal below). They are the surface the stateless mock parses; the
mock reads *world state* from here (allowed) but never *identity* (forbidden — identity
is `user` + headers, ADR 0008/0021).

Each parseable section is one `##`-headed markdown block inside the single assembled
`user` message; presentation order is fixed per policy (ADR 0016).

### 1.1 `## Goal` (all roles)
```
## Goal
Write a short onboarding guide for new contributors.
```

### 1.2 `## Board digest`
Orchestrator = full board; team agent = claimed-plus-eligible slice. One task per line:
```
- task <id> [<state>] team:<tag|->  "<title>"
```
where `<state> ∈ Open | Claimed by <agent> | Done | Cancelled`. The orchestrator's
digest ends with the folded **run-health line** (ADR 0016/0020), prefixed `run-health:`
so the mock never mistakes it for a `- task` line:
```
## Board digest
- task 1 [Claimed by agent-1] team:t1  "Draft the setup section"
- task 2 [Done] team:t1  "Draft the architecture overview"
run-health: done 1/2 · agents 1W/2I/0S · mailbox depth 0 (max 1) · ticks-since-done 0
```
Mock reads per task: **id, state, claimant (if Claimed), team tag** — exactly ADR 0021's
"minimally must read."

### 1.3 `## Claimed task` (team agent only)
Present ⟺ Working. Empty/omitted when Idle.
```
## Claimed task
task 2 — "Draft the architecture overview" (team t1)
```
Mock reads: **presence** (⇒ Working) + **task id** (to key `W_task = g(seed,agent,task)`).

### 1.4 `## Recent activity` (team agent's private sliding window)
The agent's own recent turns' verbs + outcomes, oldest first, one per line:
```
- [turn N] <verb>{<args-gist>} -> <ok|rejected|invalid>
```
Mock **counts work-actions** = lines whose `<verb> ∈ {write_knowledge, post_message,
search_knowledge}`. `claim_task` / `complete_task` / `release_task` / `sleep` are *not*
work-actions and are not counted. Window resets at each assignment boundary; wiped on
respecialization (ADR 0016). Example after one work-action on task 1:
```
## Recent activity
- [turn 4] claim_task{task:1} -> ok
- [turn 6] write_knowledge{"Setup: install mise…"} -> ok
```
⇒ **1** work-action counted.

### 1.5 `## Fresh messages` (drained mailbox, oldest first)
```
- msg <id> from <sender> (<direct|team:<t>|broadcast>): "<body>"
```
```
## Fresh messages
- msg 11 from orchestrator (direct): "Prioritize the setup section; the guide leads with it."
```

### 1.6 `## Directives` (orchestrator only — never dropped, ADR 0016)
Each pending judgment directive, one per line. **Must render kind + args, not just the
id** — the orchestrator arc has to know *which* agent and *which* specialty to act on
(ADR 0021 said "read the id"; the trace shows the id alone is insufficient to *act*):
```
- directive <id> [<tier>, <state>] <kind>{<args>} from <meta-handle>
```
```
## Directives
- directive 1 [judgment, pending] propose_respecialize{agent:agent-3, specialty:doc-reviewer} from meta-1
```

### 1.7 `## Directive outcomes` (meta-agent only — its "already-issued" bound)
```
- directive <id> [<tier>] <kind>{<args>} — <pending|fulfilled by <h>|declined by <h>: "<reason>">
```
The meta arc reads this to decide "none issued by me ⇒ emit ≤1" (ADR 0021). Empty on the
meta's first turn.

### 1.8 `## Knowledge retrievals` (auto-retrieval, cosine top-k, ADR 0016)
```
- entry <id> (<kind> by <author>, cos <score>): "<text>"
```

### 1.9 `## Metrics digest` (meta-agent only, ADR 0020 projection)
Full process view. The **utilization** line must render per-agent **state + specialty**
so the arc can find "an Idle generalist" (ADR 0021) — the raw Idle/Working/Asleep split
of ADR 0020 is not enough on its own:
```
## Metrics digest
throughput: 1 task_completed / 18 EventIds · latency: work median 6 EventIds
utilization:
  - agent-1: Working (task 1), generalist
  - agent-2: Idle, generalist (idle 0)
  - agent-3: Idle, generalist (idle 12)
mailbox: depth 0, max 1, oldest-pending-age 0
tokens: run 3.2k · faults: parks 0, malformed[a1:0 a2:0 a3:0] · directives: issued 0/ful 0/dec 0
```

> **The meta's four context slots** (ADR 0020: "observes purely through its four
> context slots") are nowhere enumerated. This transcript pins them as
> **`[Goal, Metrics digest, Directive outcomes, Recent-events window]`** — the two named
> by ADR 0021 plus Goal and a coarse recent-events window. Flagged in §9.

---

## 2. The three tool registries

Rendered verbatim into every request's `tools` array (ADR 0013/0017). Schemas are
schemars 1.2 draft-2020-12, `additionalProperties:false` (from `deny_unknown_fields`),
`strict:false`, top-level `$schema` stripped, cached at startup. Full defs shown for
one verb per role; the rest listed by name + one-line intent (they render identically in
shape). Map decision line for #14 pins the counts: **team 7, orchestrator 14, meta 6**.

### 2.1 Team-agent registry (7)
`claim_task`, `complete_task`, `release_task`, `post_message`, `write_knowledge`,
`search_knowledge`, `sleep`.

```json
{
  "type": "function",
  "function": {
    "name": "claim_task",
    "description": "Take exclusive ownership of an Open task your team is eligible for. First claim wins; a lost race returns a rejected outcome.",
    "parameters": {
      "type": "object",
      "properties": { "task": { "type": "integer", "description": "TaskId to claim." } },
      "required": ["task"],
      "additionalProperties": false
    },
    "strict": false
  }
}
```
```json
{
  "type": "function",
  "function": {
    "name": "complete_task",
    "description": "Mark your claimed task Done and record its result into the knowledge store.",
    "parameters": {
      "type": "object",
      "properties": { "result": { "type": "string", "description": "The completion result text." } },
      "required": ["result"],
      "additionalProperties": false
    },
    "strict": false
  }
}
```

### 2.2 Orchestrator registry (14)
`create_task`, `cancel_task`, `unassign_task`, `form_team`, `dissolve_team`,
`set_team_members`, `respecialize`, `sleep_agent`, `wake_agent`, `decline_directive`,
`post_message`, `write_knowledge`, `search_knowledge`, `finish_run`.

```json
{
  "type": "function",
  "function": {
    "name": "create_task",
    "description": "Author a new Open task on the board, optionally tagged to a team for claim-eligibility.",
    "parameters": {
      "type": "object",
      "properties": {
        "title": { "type": "string" },
        "description": { "type": "string" },
        "team": { "type": ["string", "null"], "description": "TeamId tag, or null for untagged." }
      },
      "required": ["title", "description"],
      "additionalProperties": false
    },
    "strict": false
  }
}
```
```json
{
  "type": "function",
  "function": {
    "name": "respecialize",
    "description": "Swap an Idle agent's specialty and system prompt, wiping its recent-activity window; identity preserved. Cite a directive with in_response_to to fulfill it.",
    "parameters": {
      "type": "object",
      "properties": {
        "agent": { "type": "string" },
        "specialty": {
          "type": "object",
          "properties": {
            "name": { "type": "string", "description": "Slug." },
            "description": { "type": "string" },
            "focus": { "type": "string" }
          },
          "required": ["name", "description", "focus"],
          "additionalProperties": false
        },
        "in_response_to": { "type": ["integer", "null"], "description": "DirectiveId being fulfilled." }
      },
      "required": ["agent", "specialty"],
      "additionalProperties": false
    },
    "strict": false
  }
}
```
```json
{
  "type": "function",
  "function": {
    "name": "finish_run",
    "description": "End the run. Rejected (enumerating blockers) if any task is Open or Claimed.",
    "parameters": {
      "type": "object",
      "properties": { "report": { "type": "string", "description": "The final markdown report." } },
      "required": ["report"],
      "additionalProperties": false
    },
    "strict": false
  }
}
```
> `in_response_to` (optional) also rides on `unassign_task` and `set_team_members` — the
> orchestrator's other action verbs a judgment directive can be fulfilled through
> (ADR 0020).

### 2.3 Meta-agent registry (6, emitters-only — no messages/knowledge/search, ADR 0020)
Mechanical: `set_parallelism`, `sleep_agent`, `wake_agent`. Judgment:
`propose_respecialize`, `propose_reallocate`, `propose_rebalance`.

```json
{
  "type": "function",
  "function": {
    "name": "propose_respecialize",
    "description": "Propose the orchestrator respecialize an agent (judgment directive; returns a directive id).",
    "parameters": {
      "type": "object",
      "properties": {
        "agent": { "type": "string" },
        "specialty": { "type": "string", "description": "Proposed specialty slug/hint; the orchestrator authors the full 3-field specialty." }
      },
      "required": ["agent", "specialty"],
      "additionalProperties": false
    },
    "strict": false
  }
}
```
```json
{
  "type": "function",
  "function": {
    "name": "set_parallelism",
    "description": "Mechanical: retune the effective team-agent permit count within [1, --parallel]. Applied by the runtime.",
    "parameters": {
      "type": "object",
      "properties": { "target": { "type": "integer" } },
      "required": ["target"],
      "additionalProperties": false
    },
    "strict": false
  }
}
```

---

## 3. The trace, tick by tick

Time flows top-to-bottom on the single serial write path. **EventIds, MessageIds and
KnowledgeEntryIds are drawn from one shared monotonic counter** (ADR 0014/0022 "the same
allocator"; TaskId and DirectiveId are separate per-run counters from 1, ADR 0023). That
is why the `events.jsonl` EventIds have **gaps** — the missing numbers were minted as
Message/Knowledge ids in the same write-path step. *(This shared-vs-separate reading is
finding **F3**; if the ruling is three independent counters, renumber events 0,1,2,… with
no gaps.)*

Tokio interleaving is nondeterministic, so this is **one valid serialization**; #23
asserts only logical invariants + per-agent event order, never this exact global order
(map Out-of-scope: byte-identical global determinism).

Legend: `cs` = per-agent `X-OpenTeam-Call-Seq` (0-based). Each turn assembles context
**once**, then loops completion→tools→feed-back until a no-tool-call yield (ADR 0015).

| # | EventId(s) | actor (cs) | what happens | board after |
|---|---|---|---|---|
| boot | 0 | system | `run_started{seed:42, goal, agents:3, meta_agents:1, parallel:3}` | — |
| **Tick 1** | 1–4 | orch (0→1) | **decompose**: `form_team(t1,[a1,a2,a3])`, `create_task×2` (batch = full T), then yield | t1: `1[Open]`, `2[Open]` |
| claim | 5–6 | agent-1 (0→1) | sees `1[Open] 2[Open]`, claims lowest id → `claim_task{1}`→ok; yield | `1[Claimed a1]` |
| **race** | 7 | agent-3 (0→1) | context assembled *concurrently* (still saw `1[Open]`) → `claim_task{1}`→**rejected** (`task_not_open`); sees reject in turn-local → yield. **No world event, no park** (`rejected`≠`invalid`) | unchanged |
| claim | 8–9 | agent-2 (0→1) | assembled after a1's claim → sees `1[Claimed] 2[Open]` → `claim_task{2}`→ok; yield | `2[Claimed a2]` |
| **Tick 2** | 10,13 | orch (2→3) | non-terminal tasks ⇒ yield **+ seeded DIRECT steer** `post_message(→agent-1)` (msg 11) | unchanged |
| work | 14,15,17 | agent-1 (2→3) | drains msg 11 (`messages_delivered`); window=0<2 → **`write_knowledge`** Note (entry 16); yield | unchanged |
| work | 18 | agent-2 (2→3) | window=0<1 → **`search_knowledge`**"setup deps" → hits entry 16; yield | unchanged |
| **Meta 1** | 19–20 | meta-1 (0→1) | outcomes empty ⇒ emit ≤1 → **judgment** `propose_respecialize{agent-3, doc-reviewer}` → `directive 1` pending; yield | unchanged |
| complete | 21,23 | agent-2 (4→5) | window=1≥1 → **`complete_task`** (result → entry 22); yield | `2[Done]` |
| **Tick 3** | 24,25,26,29 | orch (4→5) | pending judgment directive ⇒ **`respecialize{agent-3, doc-reviewer, in_response_to:1}`**→ok (`agent_respecialized`+`directive_fulfilled`); then yield **+ seeded BROADCAST steer** (msg 27) | unchanged |
| work | 30,31,34 | agent-1 (4→5) | drains msg 27; window=1<2 → **`post_message(team:t1)`** (msg 32); yield | unchanged |
| complete | 35,37 | agent-1 (6→7) | window=2≥2 → **`complete_task`** (result → entry 36); yield | `1[Done]` |
| **Tick 4** | 38,39 | orch (6→7) | all tasks terminal ⇒ **`finish_run`**→validates no Open/Claimed→ok → `run_finished{CleanFinish, exit 0}` | done |

Four full request/response pairs follow (A–D). Every state change is in the
`events.jsonl` of [§8](#8-the-complete-eventsjsonl).

### Board digest evolution (as the orchestrator saw it)
```
Tick 1 (pre):   (empty — n=0 ⇒ decompose)
Tick 2:         - task 1 [Claimed by agent-1] team:t1  "Draft the setup section"
                - task 2 [Claimed by agent-2] team:t1  "Draft the architecture overview"
                run-health: done 0/2 · agents 2W/1I/0S · mailbox depth 0 (max 0) · ticks-since-done 1
Tick 3:         - task 1 [Claimed by agent-1] team:t1  "Draft the setup section"
                - task 2 [Done] team:t1  "Draft the architecture overview"
                run-health: done 1/2 · agents 1W/2I/0S · mailbox depth 0 (max 1) · ticks-since-done 0
Tick 4 (final): - task 1 [Done] team:t1  "Draft the setup section"
                - task 2 [Done] team:t1  "Draft the architecture overview"
                run-health: done 2/2 · agents 0W/3I/0S · mailbox depth 2 (max 2) · ticks-since-done 0
                → all terminal ⇒ finish_run
```
*(Tick 4's mailbox depth 2 = the broadcast + team messages queued on the two idle agents
that never took another turn — losslessly queued, never delivered before the run ended.
ADR 0011: undelivered ≠ dropped; finish_run validates tasks, not mailboxes.)*

---

<a name="pair-a"></a>
## 4. Full pair A — orchestrator decompose (Tick 1, `cs 0`)

### Request → `POST /v1/chat/completions`
```
X-OpenTeam-Call-Seq: 0
X-OpenTeam-Seed: 42
Content-Type: application/json
```
```json
{
  "model": "openteam-mock",
  "user": "orchestrator",
  "messages": [
    { "role": "system", "content": "You are the orchestrator of an offline agentic team. You decompose the goal into board tasks, form teams, steer via messages and directives, and alone end the run with finish_run. (harness-owned skeleton — inert to the mock)" },
    { "role": "user", "content": "## Goal\nWrite a short onboarding guide for new contributors.\n\n## Board digest\n(empty)\nrun-health: done 0/0 · agents 0W/3I/0S · mailbox depth 0 (max 0) · ticks-since-done 0\n\n## Knowledge retrievals\n(none)\n\n## Fresh messages\n(none)\n\n## Directives\n(none)" }
  ],
  "tools": [ /* the 14 orchestrator ToolDefs of §2.2, verbatim */ ],
  "tool_choice": "auto",
  "parallel_tool_calls": true
}
```
**Arc derivation (stateless):** identity `orchestrator` (from `user`). Board digest
count `n == 0` ⇒ **decompose**. `T = f(42) = 2`; emit a full-`T` batch plus a team.
Titles weave goal words for plausibility (the realism dial, ADR 0021).

### Response (server wraps the `ChatDecision`; envelope owned by the mock, ADR 0019)
```json
{
  "id": "chatcmpl-42-orchestrator-0",
  "object": "chat.completion",
  "created": 1752710400,
  "model": "openteam-mock",
  "choices": [
    {
      "index": 0,
      "message": {
        "role": "assistant",
        "content": null,
        "refusal": null,
        "tool_calls": [
          { "id": "call_a0_1", "type": "function", "function": { "name": "form_team", "arguments": "{\"team\":\"t1\",\"members\":[\"agent-1\",\"agent-2\",\"agent-3\"]}" } },
          { "id": "call_a0_2", "type": "function", "function": { "name": "create_task", "arguments": "{\"title\":\"Draft the setup section\",\"description\":\"Install + build/test steps for a new contributor.\",\"team\":\"t1\"}" } },
          { "id": "call_a0_3", "type": "function", "function": { "name": "create_task", "arguments": "{\"title\":\"Draft the architecture overview\",\"description\":\"One-paragraph crate map.\",\"team\":\"t1\"}" } }
        ]
      },
      "logprobs": null,
      "finish_reason": "tool_calls"
    }
  ],
  "usage": { "prompt_tokens": 611, "completion_tokens": 78, "total_tokens": 689 }
}
```
Harness dispatches the 3 calls in array order on the serial write path → events **1,2,3**
(`team_formed`, `task_created`×2), each returning `{"status":"ok","result":{…}}` fed back
as one `role:"tool"` per `tool_call_id`. **Completion `cs 1`** then re-derives from the
turn-local tool results (2 creates == T, tasks non-terminal) → **yield**:
```json
{ "id": "chatcmpl-42-orchestrator-1", "object": "chat.completion", "created": 1752710400,
  "model": "openteam-mock",
  "choices": [ { "index": 0, "message": { "role": "assistant", "content": "Team t1 formed; two tasks on the board. Handing off to the team.", "refusal": null }, "logprobs": null, "finish_reason": "stop" } ],
  "usage": { "prompt_tokens": 690, "completion_tokens": 16, "total_tokens": 706 } }
```
⇒ event **4** `turn_completed{first_call_seq:0, last_call_seq:1, tool_iters:1, outcome:Yielded, malformed:false, on_task:null}`.

---

<a name="pair-b"></a>
## 5. Full pair B — agent-3 lost-claim race (the `rejected` path, `cs 0` then `cs 1`)

This is the run's `rejected` tool-outcome. It also exercises the arc's
"don't-hammer-after-a-lost-claim" rule and shows the inner loop feeding a tool result
back **within one turn** (ADR 0015).

### Completion `cs 0` request (context assembled while task 1 still read `Open`)
```
X-OpenTeam-Call-Seq: 0
X-OpenTeam-Seed: 42
```
```json
{
  "model": "openteam-mock",
  "user": "team-agent:agent-3:generalist",
  "messages": [
    { "role": "system", "content": "You are a team agent. Specialty: generalist. Claim eligible work, do it over one or more turns, then complete_task. (skeleton — inert to the mock)" },
    { "role": "user", "content": "## Goal\nWrite a short onboarding guide for new contributors.\n\n## Board digest\n- task 1 [Open] team:t1  \"Draft the setup section\"\n- task 2 [Open] team:t1  \"Draft the architecture overview\"\n\n## Claimed task\n(none)\n\n## Recent activity\n(none)\n\n## Knowledge retrievals\n(none)\n\n## Fresh messages\n(none)" }
  ],
  "tools": [ /* the 7 team-agent ToolDefs of §2.1 */ ],
  "tool_choice": "auto",
  "parallel_tool_calls": true
}
```
**Arc:** identity `team-agent:agent-3:generalist`. No `## Claimed task` ⇒ Idle. Eligible
Open task in digest ⇒ `claim_task` on the **lowest-id** Open eligible task = task 1.
```json
{ "id": "chatcmpl-42-agent-3-0", "object": "chat.completion", "created": 1752710400, "model": "openteam-mock",
  "choices": [ { "index": 0, "message": { "role": "assistant", "content": null, "refusal": null,
    "tool_calls": [ { "id": "call_c0_1", "type": "function", "function": { "name": "claim_task", "arguments": "{\"task\":1}" } } ] },
    "logprobs": null, "finish_reason": "tool_calls" } ],
  "usage": { "prompt_tokens": 402, "completion_tokens": 12, "total_tokens": 414 } }
```
On the serial write path task 1 is **already `Claimed by agent-1`** (event 5 committed
first). The domain guard refuses → **`rejected`** tool-outcome, **no world event** (no
state changed):
```json
{ "status": "rejected", "code": "task_not_open", "message": "task 1 is not Open (Claimed by agent-1)" }
```

### Completion `cs 1` request (same turn — the inner loop fed the result back)
```
X-OpenTeam-Call-Seq: 1
X-OpenTeam-Seed: 42
```
```json
{
  "model": "openteam-mock",
  "user": "team-agent:agent-3:generalist",
  "messages": [
    { "role": "system", "content": "You are a team agent. Specialty: generalist. …" },
    { "role": "user", "content": "## Goal\n…\n## Board digest\n- task 1 [Open] team:t1  \"Draft the setup section\"\n- task 2 [Open] team:t1  \"Draft the architecture overview\"\n## Claimed task\n(none)\n## Recent activity\n(none)\n## Fresh messages\n(none)" },
    { "role": "assistant", "content": null, "tool_calls": [ { "id": "call_c0_1", "type": "function", "function": { "name": "claim_task", "arguments": "{\"task\":1}" } } ] },
    { "role": "tool", "tool_call_id": "call_c0_1", "content": "{\"status\":\"rejected\",\"code\":\"task_not_open\",\"message\":\"task 1 is not Open (Claimed by agent-1)\"}" }
  ],
  "tools": [ /* 7 team-agent defs */ ],
  "tool_choice": "auto",
  "parallel_tool_calls": true
}
```
**Arc:** the `## Board digest` (assembled at turn start) *still* shows task 1 `Open` — but
the **turn-local** `role:"tool"` message shows this turn already lost a claim. Per ADR
0021 "if the turn-local messages already show a lost claim this turn, **yield** rather
than hammer." ⇒ no-tool-call **yield**:
```json
{ "id": "chatcmpl-42-agent-3-1", "object": "chat.completion", "created": 1752710400, "model": "openteam-mock",
  "choices": [ { "index": 0, "message": { "role": "assistant", "content": "Lost the race for task 1; nothing else eligible. Yielding.", "refusal": null }, "logprobs": null, "finish_reason": "stop" } ],
  "usage": { "prompt_tokens": 470, "completion_tokens": 14, "total_tokens": 484 } }
```
⇒ event **7** `turn_completed{first_call_seq:0, last_call_seq:1, tool_iters:1,
outcome:Yielded, malformed:false, on_task:null}`. **`malformed:false`** because the sole
call was `rejected`, not `invalid` — the K=3 counter is **not** incremented (ADR
0015/0017). agent-3 stays **Idle** (a generalist with no work — the meta's respecialize
target).

---

<a name="pair-c"></a>
## 6. Full pair C — meta emits the judgment directive (Meta turn 1, `cs 0`)

### Request
```
X-OpenTeam-Call-Seq: 0
X-OpenTeam-Seed: 42
```
```json
{
  "model": "openteam-mock",
  "user": "meta-agent:meta-1",
  "messages": [
    { "role": "system", "content": "You are a meta-agent. You observe metrics and improve the process through directives only. (skeleton — inert to the mock)" },
    { "role": "user", "content": "## Goal\nWrite a short onboarding guide for new contributors.\n\n## Metrics digest\nthroughput: 0 task_completed / 18 EventIds · latency: work n/a\nutilization:\n  - agent-1: Working (task 1), generalist\n  - agent-2: Working (task 2), generalist\n  - agent-3: Idle, generalist (idle 12)\nmailbox: depth 0, max 1, oldest-pending-age 0\ntokens: run 2.6k · faults: parks 0, malformed[a1:0 a2:0 a3:0] · directives: issued 0/ful 0/dec 0\n\n## Directive outcomes\n(none issued)\n\n## Recent events\n… task_claimed×2, message_sent(direct), knowledge_written, turn_completed×… " }
  ],
  "tools": [ /* the 6 meta ToolDefs of §2.3 */ ],
  "tool_choice": "auto",
  "parallel_tool_calls": false
}
```
**Arc:** identity `meta-agent:meta-1`. `## Directive outcomes` shows **none issued by me**
⇒ emit ≤1 seeded directive. The utilization line shows **agent-3: Idle, generalist** ⇒
`propose_respecialize` on that idle generalist.
```json
{ "id": "chatcmpl-42-meta-1-0", "object": "chat.completion", "created": 1752710400, "model": "openteam-mock",
  "choices": [ { "index": 0, "message": { "role": "assistant", "content": null, "refusal": null,
    "tool_calls": [ { "id": "call_m0_1", "type": "function", "function": { "name": "propose_respecialize", "arguments": "{\"agent\":\"agent-3\",\"specialty\":\"doc-reviewer\"}" } } ] },
    "logprobs": null, "finish_reason": "tool_calls" } ],
  "usage": { "prompt_tokens": 553, "completion_tokens": 22, "total_tokens": 575 } }
```
Runtime enqueues to the orchestrator, returns `{"status":"ok","result":{"directive_id":1}}`
⇒ event **19** `directive_issued{directive:1, tier:Judgment, kind:propose_respecialize,
args:{agent:agent-3, specialty:doc-reviewer}}` (source `meta-1`). **Completion `cs 1`**:
outcomes slot now shows directive 1 issued-by-me ⇒ **yield** (event **20**).

> **This is the single directive of the whole run** (finding **F1**). The arc's meta
> phase is "outcomes shows none issued by me → emit ≤1; **else yield**" — read
> statelessly from the outcomes slot, which never re-empties. So `--meta-agents 1` +
> built-in arc = **exactly one directive, ever**. The mechanical `set_parallelism`
> directive the brief also asked for **cannot appear in this run**. See §9/F1 and the
> appendix at §11.1.

---

<a name="pair-d"></a>
## 7. Full pair D — orchestrator resolves the directive with a cite (Tick 3, `cs 4`)

### Request
```
X-OpenTeam-Call-Seq: 4
X-OpenTeam-Seed: 42
```
```json
{
  "model": "openteam-mock",
  "user": "orchestrator",
  "messages": [
    { "role": "system", "content": "You are the orchestrator … (skeleton)" },
    { "role": "user", "content": "## Goal\nWrite a short onboarding guide for new contributors.\n\n## Board digest\n- task 1 [Claimed by agent-1] team:t1  \"Draft the setup section\"\n- task 2 [Done] team:t1  \"Draft the architecture overview\"\nrun-health: done 1/2 · agents 1W/2I/0S · mailbox depth 0 (max 1) · ticks-since-done 0\n\n## Knowledge retrievals\n- entry 22 (TaskCompletion by agent-2, cos 0.74): \"Architecture overview: openteam (bin) → core + mock + leaf wire; mock depends on wire only.\"\n- entry 16 (Note by agent-1, cos 0.63): \"Setup: install mise; then `mise run build` / `test`. Rust 1.94 via mise.\"\n\n## Fresh messages\n(none)\n\n## Directives\n- directive 1 [judgment, pending] propose_respecialize{agent:agent-3, specialty:doc-reviewer} from meta-1" }
  ],
  "tools": [ /* 14 orchestrator defs */ ],
  "tool_choice": "auto",
  "parallel_tool_calls": true
}
```
**Arc:** a **pending judgment directive is present** ⇒ resolve it. The `## Directives`
line carries the id (**1**), kind (`propose_respecialize`), and args
(`agent-3`, `doc-reviewer`) — the arc needs all three to *act*, which is why the grammar
(§1.6) renders args, not just the id. agent-3 is Idle ⇒ **act-with-cite**:
```json
{ "id": "chatcmpl-42-orchestrator-4", "object": "chat.completion", "created": 1752710400, "model": "openteam-mock",
  "choices": [ { "index": 0, "message": { "role": "assistant", "content": null, "refusal": null,
    "tool_calls": [ { "id": "call_a4_1", "type": "function", "function": { "name": "respecialize", "arguments": "{\"agent\":\"agent-3\",\"specialty\":{\"name\":\"doc-reviewer\",\"description\":\"Reviews drafted docs for accuracy and tone.\",\"focus\":\"onboarding guide review\"},\"in_response_to\":1}" } } ] },
    "logprobs": null, "finish_reason": "tool_calls" } ],
  "usage": { "prompt_tokens": 742, "completion_tokens": 61, "total_tokens": 803 } }
```
agent-3 is `Idle && !in-flight` ⇒ respecialize legal (ADR 0003). Runtime emits **two**
events (ADR 0022 keeps both): event **24** `agent_respecialized{agent:agent-3,
from:generalist, to:doc-reviewer, via_directive:1}` and event **25**
`directive_fulfilled{directive:1, by:orchestrator}`. agent-3's channel now renders
`team-agent:agent-3:doc-reviewer` — but its **call-seq keeps climbing** (never resets on
respecialize, ADR 0018); its recent-activity window is wiped (already empty, it was Idle).

**Completion `cs 5`:** the directive is cleared from `## Directives`; task 1 still
non-terminal ⇒ yield, with a seeded **BROADCAST** steer (event 26 `message_sent`,
address `Broadcast`) → event **29** `turn_completed`.

---

## 8. The complete `events.jsonl`

One `Event` per line, streamed append+flush (ADR 0022), in **EventId order**. Gaps in the
id sequence (11,12,16,22,27,28,32,33,36) are the MessageIds / KnowledgeEntryIds minted in
the same write-path step under the **shared-counter** reading (finding **F3**). `at` is a
frozen-Clock breadcrumb, never the ordering key. Source is the actor; non-actor subjects
ride in `data`.

```jsonl
{"id":0,"at":"2026-07-17T00:00:00Z","source":"system","kind":"run_started","data":{"run_id":"0192f1a0-7e3c-7abc-9def-000000000000","seed":42,"goal":"Write a short onboarding guide for new contributors","agents":3,"meta_agents":1,"parallel":3,"scenario":null,"caps":{}}}
{"id":1,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"team_formed","data":{"team":"t1","members":["agent-1","agent-2","agent-3"]}}
{"id":2,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"task_created","data":{"task":1,"title":"Draft the setup section","description":"Install + build/test steps for a new contributor.","team":"t1"}}
{"id":3,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"task_created","data":{"task":2,"title":"Draft the architecture overview","description":"One-paragraph crate map.","team":"t1"}}
{"id":4,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"turn_completed","data":{"first_call_seq":0,"last_call_seq":1,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":1301,"completion":94,"total":1395},"on_task":null}}
{"id":5,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"task_claimed","data":{"task":1,"team":"t1"}}
{"id":6,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"turn_completed","data":{"first_call_seq":0,"last_call_seq":1,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":812,"completion":24,"total":836},"on_task":1}}
{"id":7,"at":"2026-07-17T00:00:00Z","source":"agent-3","kind":"turn_completed","data":{"first_call_seq":0,"last_call_seq":1,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":872,"completion":26,"total":898},"on_task":null}}
{"id":8,"at":"2026-07-17T00:00:00Z","source":"agent-2","kind":"task_claimed","data":{"task":2,"team":"t1"}}
{"id":9,"at":"2026-07-17T00:00:00Z","source":"agent-2","kind":"turn_completed","data":{"first_call_seq":0,"last_call_seq":1,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":815,"completion":24,"total":839},"on_task":2}}
{"id":10,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"message_sent","data":{"message":11,"address":{"Direct":{"to":"agent-1"}},"body":"Prioritize the setup section; the guide leads with it.","knowledge_ref":12}}
{"id":13,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"turn_completed","data":{"first_call_seq":2,"last_call_seq":3,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":1180,"completion":40,"total":1220},"on_task":null}}
{"id":14,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"messages_delivered","data":{"delivered":[11]}}
{"id":15,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"knowledge_written","data":{"entry":16,"text":"Setup: install mise; then `mise run build` / `test`. Rust 1.94 via mise."}}
{"id":17,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"turn_completed","data":{"first_call_seq":2,"last_call_seq":3,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":905,"completion":38,"total":943},"on_task":1}}
{"id":18,"at":"2026-07-17T00:00:00Z","source":"agent-2","kind":"turn_completed","data":{"first_call_seq":2,"last_call_seq":3,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":898,"completion":33,"total":931},"on_task":2}}
{"id":19,"at":"2026-07-17T00:00:00Z","source":"meta-1","kind":"directive_issued","data":{"directive":1,"tier":"Judgment","kind":"propose_respecialize","args":{"agent":"agent-3","specialty":"doc-reviewer"}}}
{"id":20,"at":"2026-07-17T00:00:00Z","source":"meta-1","kind":"turn_completed","data":{"first_call_seq":0,"last_call_seq":1,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":1128,"completion":36,"total":1164},"on_task":null}}
{"id":21,"at":"2026-07-17T00:00:00Z","source":"agent-2","kind":"task_completed","data":{"task":2,"result":"Architecture overview: openteam (bin) → core + mock + leaf wire; mock depends on wire only.","result_ref":22}}
{"id":23,"at":"2026-07-17T00:00:00Z","source":"agent-2","kind":"turn_completed","data":{"first_call_seq":4,"last_call_seq":5,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":902,"completion":41,"total":943},"on_task":2}}
{"id":24,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"agent_respecialized","data":{"agent":"agent-3","from":"generalist","to":"doc-reviewer","via_directive":1}}
{"id":25,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"directive_fulfilled","data":{"directive":1,"by":"orchestrator"}}
{"id":26,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"message_sent","data":{"message":27,"address":"Broadcast","body":"Team: once setup lands, the guide is complete — no further tasks planned.","knowledge_ref":28}}
{"id":29,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"turn_completed","data":{"first_call_seq":4,"last_call_seq":5,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":1495,"completion":72,"total":1567},"on_task":null}}
{"id":30,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"messages_delivered","data":{"delivered":[27]}}
{"id":31,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"message_sent","data":{"message":32,"address":{"Team":{"team":"t1"}},"body":"Setup section drafted; see knowledge notes.","knowledge_ref":33}}
{"id":34,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"turn_completed","data":{"first_call_seq":4,"last_call_seq":5,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":940,"completion":37,"total":977},"on_task":1}}
{"id":35,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"task_completed","data":{"task":1,"result":"Setup section: install mise; `mise run build/test/lint/fmt`; Rust 1.94 via mise.","result_ref":36}}
{"id":37,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"turn_completed","data":{"first_call_seq":6,"last_call_seq":7,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":951,"completion":44,"total":995},"on_task":1}}
{"id":38,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"turn_completed","data":{"first_call_seq":6,"last_call_seq":6,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":1210,"completion":180,"total":1390},"on_task":null}}
{"id":39,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"run_finished","data":{"reason":"CleanFinish","exit_code":0}}
```

**Kind coverage (17 of 26 fired on this happy path):** `run_started`, `team_formed`,
`task_created`, `turn_completed`, `task_claimed`, `message_sent`, `messages_delivered`,
`knowledge_written`, `directive_issued`, `task_completed`, `agent_respecialized`,
`directive_fulfilled`, `run_finished`. **Correctly 0 on the happy path:**
`liveness_nudge`, `context_degraded`, `agent_parked`, `cap_hit`. **Not exercised by this
particular run** (need cancellation / release / unassign / sleep-wake / team-change /
mechanical directive / decline): `task_released`, `task_unassigned`, `task_cancelled`,
`agent_slept`, `agent_parked`, `agent_woke`, `parallelism_changed`, `team_members_set`,
`team_dissolved`, `directive_declined`. Note **`knowledge_written` fired once** — for the
lone Note; the Message and TaskCompletion entries (12, 22, 28, 33, 36) carry **no**
`knowledge_written` (ADR 0022: it is Notes-only; their `source_event` points at the
`message_sent` / `task_completed` that caused them).

<a name="board-json"></a>
### `board.json` (final snapshot)
```json
{
  "run_id": "0192f1a0-7e3c-7abc-9def-000000000000",
  "goal": "Write a short onboarding guide for new contributors",
  "seed": 42,
  "tasks": [
    { "id": 1, "title": "Draft the setup section", "description": "Install + build/test steps for a new contributor.", "created_by": "orchestrator", "origin_event": 2, "team": "t1", "state": { "Done": { "result": "Setup section: install mise; `mise run build/test/lint/fmt`; Rust 1.94 via mise.", "result_ref": 36 } } },
    { "id": 2, "title": "Draft the architecture overview", "description": "One-paragraph crate map.", "created_by": "orchestrator", "origin_event": 3, "team": "t1", "state": { "Done": { "result": "Architecture overview: openteam (bin) → core + mock + leaf wire; mock depends on wire only.", "result_ref": 22 } } }
  ],
  "teams": [ { "id": "t1", "members": ["agent-1","agent-2","agent-3"], "dissolved": false } ]
}
```

<a name="knowledge-json"></a>
### `knowledge.jsonl` (final — embeddings omitted, recomputable, ADR 0022)
```jsonl
{"id":12,"kind":"Message","author":"orchestrator","source_event":10,"text":"Prioritize the setup section; the guide leads with it."}
{"id":16,"kind":"Note","author":"agent-1","source_event":15,"text":"Setup: install mise; then `mise run build` / `test`. Rust 1.94 via mise."}
{"id":22,"kind":"TaskCompletion","author":"agent-2","source_event":21,"text":"Architecture overview: openteam (bin) → core + mock + leaf wire; mock depends on wire only."}
{"id":28,"kind":"Message","author":"orchestrator","source_event":26,"text":"Team: once setup lands, the guide is complete — no further tasks planned."}
{"id":33,"kind":"Message","author":"agent-1","source_event":31,"text":"Setup section drafted; see knowledge notes."}
{"id":36,"kind":"TaskCompletion","author":"agent-1","source_event":35,"text":"Setup section: install mise; `mise run build/test/lint/fmt`; Rust 1.94 via mise."}
```
6 entries: 3 `Message`, 1 `Note`, 2 `TaskCompletion`. `source_event` back-references are
coherent because all ids came off one write-path step (ADR 0014).

---

## 9. Contradictions & ambiguities found

The gold of this gate. Ordered by severity; **F1–F5** are the batch put to main.

### F1 — CONTRADICTION (brief vs ADR 0021): a `--meta-agents 1` built-in run can show only ONE directive, so it cannot exhibit both directive tiers.
The built-in meta arc (ADR 0021) is: *"Directive-outcomes shows none issued by me → emit
≤1 seeded directive; else → yield. The already-issued bound is read statelessly from the
outcomes slot."* Once the meta issues **any** directive it appears in the outcomes slot
forever, so "none issued by me" is permanently false ⇒ **exactly one directive per
meta-agent per run**. The brief (and #22's scope) asks the canonical `--meta-agents 1`
demo to exercise **both** a judgment directive (`propose_respecialize`, needed to drive
the required respecialization round-trip) **and** a mechanical one (`set_parallelism` /
`sleep_agent`). Under the built-in arc that is impossible with one meta.
This trace therefore shows the **judgment** directive (it double-counts as the required
respecialization) and **cannot** show the mechanical one. *Ruling needed*, three ways
out: (a) accept — the canonical demo shows one tier, and the mechanical tier is shown by
the `meta-directive`/a mechanical **scenario** fixture (ADR 0023) — no ADR change; (b) the
canonical demo runs `--meta-agents 2` (`meta-1` judgment, `meta-2` mechanical — diverse
panel, ADR 0020) — updates the demo recipe only; (c) relax the meta arc to emit up to one
directive **per tier** — **reopens #17/#18 (ADR 0020/0021)**. (A worked mechanical-path
inset is in [§11.1](#111-appendix--the-mechanical-directive-path-not-in-the-1-meta-run) so
the mechanism is validated regardless.)

### F2 — SPEC DRIFT (ADR 0020 vs ADR 0012/0022/0023): meta-agent handle indexing.
ADR 0020 writes meta handles as **`meta-0`, `meta-1`, …** (0-based), but the canonical
positional-handle ADR 0012 says **`meta-1`…`meta-M`** (1-based), and ADR 0022's example
source is `"meta-1"`, and ADR 0023's selectors use `meta-1`. Team agents are unambiguously
1-based (`agent-1…agent-N`). This trace uses **`meta-1`** (canonical). *Ruling:* confirm
1-based and fix ADR 0020's `meta-0, meta-1` to `meta-1, meta-2` (one-line **reopen of
ADR 0020**), or declare metas 0-based and fix the others.

### F3 — SPEC AMBIGUITY (ADR 0014/0022): one shared id counter, or three?
ADR 0014/0022 say EventId/MessageId/KnowledgeEntryId are "monotonic counters off the
single serial write path" / "the **same allocator**." Two readings, and they render
`events.jsonl` differently:
- **Shared single counter** (this trace): a message-send step mints 3 consecutive ids
  (event, message, knowledge) from one counter ⇒ **EventIds have gaps** (…10, *13*, … the
  11/12 were the message/knowledge ids). Cross-refs live in one global id space.
- **Three independent counters** on the one write path: EventIds are contiguous 0,1,2,…;
  MessageIds 1,2,3; KnowledgeEntryIds 1,2,3. Cross-refs are per-space (typed).
Both satisfy "coherent because allocated up-front in one step." The trace picks *shared*
as the more literal reading of "same allocator," but this is a real fork. *Ruling:*
pin which, and add one sentence to ADR 0022. (If "three independent," renumber §8 with no
gaps.)

### F4 — FORMAT PIN (ADR 0016/0021 left this open): the section line-grammars.
ADR 0021 asserts #18 pins the grammars and #13 renders to them, but **no ADR writes them
down**. [§1](#1-the-pinned-line-grammars) proposes the concrete grammars for **board
digest** (incl. the folded run-health line), **claimed task**, **recent activity**,
**fresh messages**, **directives**, **directive outcomes**, **knowledge retrievals**, and
the **metrics digest** (incl. per-agent state+specialty in utilization). Two pins go
*beyond* ADR 0021's stated minimum and are load-bearing for the arc to *act*:
1. the **`## Directives`** line must carry the directive's **kind + args**, not just its
   id — the orchestrator arc reads `{agent, specialty}` from it to emit the respecialize
   (Pair D); "read the id" alone is insufficient to fulfill.
2. the **metrics-digest utilization** must render each agent's **specialty**, not just
   Idle/Working/Asleep — the meta arc needs "an Idle **generalist**" to pick a
   `propose_respecialize` target (Pair C).
Also unpinned and pinned here: the meta's **four context slots** (proposed
`[Goal, Metrics digest, Directive outcomes, Recent-events window]`). *Ratify* these and
fold into **ADR 0016 (rendering) + ADR 0021 (what the mock reads)**.

### F5 — FORMAT PIN (ADR 0021 under-specifies the arc's within-turn shape).
ADR 0021 pins the *cross-turn* completion rule (count window work-actions vs `W_task`) and
mandates multi-turn work as the default, but doesn't say how a turn *ends* on a work turn.
The trace requires three within-turn rules to produce the transcript statelessly, all
consistent with ADR 0021's spirit but not written:
1. **One verb per turn, then yield.** After the arc's single action of the turn (claim, or
   one work-action, or complete) returns `ok`/`rejected` in turn-local, the next completion
   **yields** — generalizing ADR 0021's stated lost-claim "don't hammer" rule to all
   actions. Without this a Working turn would loop emitting work-actions to `MAX_TOOL_ITERS`
   (the window doesn't update mid-turn), *or* collapse to the single-turn claim→work→
   complete ADR 0021 explicitly rejects.
2. **Count work-actions from the recent-activity window only** (the cross-turn memory), not
   from turn-local — so each Working turn does exactly one work-action, giving genuine
   multi-turn spread across `W_task` turns.
3. **Claim the lowest-id eligible Open task** when several are visible (ADR 0021 just says
   "an eligible Open task"). Needed for the race in Pair B to be deterministic.
*Ratify* and fold into **ADR 0021**.

### Minor notes (not in the batch)
- **`~27` vs `26` kinds.** The map's #19 line says "~27-kind"; ADR 0022 pins exactly **26**
  (verified by count). The `~` hedges it; cosmetic, no ruling — but the map gist could drop
  the `~` when it's next touched. *(Do not edit the map here — flag only.)*
- **Default `model` string** is unpinned (ADR 0018 has `LlmConfig.model` but no default
  value). Trace uses `"openteam-mock"`. Harmless; the mock only echoes it.
- **`DirectiveId` counter** is untyped as to origin. Trace treats it as its own per-run
  counter from 1 (like `TaskId`, ADR 0023). If it should come off the shared allocator,
  that folds under F3.
- **`run_finished` source.** ADR 0022 pins `run_started` = `system` but not `run_finished`.
  Trace attributes the clean finish to `orchestrator` (the `finish_run` caller = the
  actor) and a cap-hit finish to `system`. Reasonable under the "source = actor" rule;
  worth one confirming word in ADR 0022 but not a blocker.
- **`on_task` on a claiming turn.** The turn starts Idle and ends Working; trace sets
  `on_task` to the task claimed by turn-end (e.g. event 6 `on_task:1`). Consistent enough
  for the per-done-task token fold; noting the choice.

---

## 10. Stateless-arc replay check

The deepest consistency test: can the built-in `BehaviorModel` — seeing only
`(request, identity, seed)` with **zero run-state** — re-derive every action in §3 from
the *rendered request alone*? Walking the transcript:

| Action | What the mock reads (content) | Identity (wire) | Re-derivable statelessly? |
|---|---|---|---|
| decompose (Pair A) | `## Board digest` empty ⇒ n==0 | `user:orchestrator` | ✅ n==0 ⇒ decompose, `T=f(42)` |
| claim lowest id | `## Board digest` shows Open tasks; `## Claimed task` empty | `team-agent:agent-1:generalist` | ✅ Idle + eligible Open ⇒ claim (F5.3) |
| lost-claim yield (Pair B) | turn-local `role:"tool"` = `rejected` | `…:agent-3:generalist` | ✅ turn-local reject ⇒ yield |
| one work-action/turn | `## Recent activity` count vs `W_task=g(42,agent,task)` | agent + task id from `## Claimed task` | ✅ recompute `g`, compare window count (F5.1–2) |
| complete | window count ≥ `W_task` | agent + task id | ✅ |
| meta emits 1 (Pair C) | `## Directive outcomes` empty; util shows idle generalist | `meta-agent:meta-1` | ✅ (needs specialty in util — F4.2) |
| resolve directive (Pair D) | `## Directives` id+kind+args | `user:orchestrator` | ✅ (needs args in grammar — F4.1) |
| finish_run | `## Board digest` all terminal, n>0 | `user:orchestrator` | ✅ all-terminal ⇒ finish |

**Every row re-derives** with **no memory of prior turns** — the board rendered in the
request *is* the arc's memory (ADR 0021), and identity never leaks from content (ADR
0008). The two places the naive "minimal read" of ADR 0021 falls short (F4.1 directive
args, F4.2 utilization specialty) are exactly the line-grammar refinements this gate
surfaced; with them pinned, the arc produces this transcript **statelessly**. Termination
holds by construction: bounded decomposition (`T≤8`, capped by the visible board),
every task converges (`W_task≤3`, degradation-safe), every turn yields.

**Conclusion:** the protocol **hangs together** end-to-end. The one thing the canonical
run *cannot* do (both directive tiers under one meta, F1) is a demo-scope ruling, not a
structural break. Map-closure-ready pending F1's ruling + F2–F5 ratification.

---

## 11. `report.md` (== stdout)

Rendered once, written byte-identically to `.openteam/runs/<uuidv7>/report.md` **and**
stdout (ADR 0022/0024). The `finish_run` report body first, then the `## Run summary`
from `Metrics::summary()`.

```markdown
# Onboarding guide for new contributors

## Setup
Install [mise](https://mise.jdx.dev/), then use the canonical tasks: `mise run build`,
`mise run test`, `mise run lint`, `mise run fmt`. The toolchain (Rust 1.94) is pinned by
mise, so no manual rustup step is needed.

## Architecture
`openteam` is a 4-crate workspace: the `openteam` binary depends on `openteam-core` and
`openteam-mock`; both sit above the leaf `openteam-wire` contract crate. The mock depends
on `openteam-wire` only, which is what keeps it a faithful general OpenAI server.

---

## Run summary
- Outcome: CleanFinish (exit 0)
- Duration: 0.41s wall · 4 ticks
- Agents: 3 team + 1 meta · specialties used: generalist, doc-reviewer
- Tasks: created 2 · completed 2 · cancelled 0
- Messages: 3 (direct 1, team 1, broadcast 1)
- Knowledge: 6 entries (Message 3, Note 1, TaskCompletion 2) · 486 bytes
- Sleeps 0 · Wakes 0 · Parks 0
- Respecializations: 1 (agent-3: generalist → doc-reviewer)
- Tokens: 15.9k total — orchestrator 5.6k, agent-1 2.8k, agent-2 2.7k, agent-3 1.4k, meta-1 1.2k
- Meta interventions: issued 1 · fulfilled 1 · declined 0
- Liveness nudges: 0
```

*(Numbers are illustrative — usage is informational only, ADR 0001/0018. The **structure**
is the point: every field is a fold over the §8 event log, so a `--meta-agents 0` run would
render the identical block minus the meta line — metrics are runtime-owned, ADR 0020.)*

### 11.1 Appendix — the mechanical directive path (NOT in the 1-meta run; validates F1)
For completeness, here is the mechanical tier traced end-to-end, as it would occur under
**`--meta-agents 2`** (a second meta, `meta-2`, on its first turn) or a `set_parallelism`
scenario. It is **not** part of the canonical §3–§8 run (F1):

```
meta-2 (cs 0), user "meta-agent:meta-2", outcomes empty ⇒ emit ≤1:
  → tool_call set_parallelism{target:2}
  runtime clamps to [1,--parallel=3], applies (add/forget permits), returns ok{applied, effective:2}
  events (shared-counter numbering, illustrative):
    {"source":"meta-2","kind":"directive_issued","data":{"directive":2,"tier":"Mechanical","kind":"set_parallelism","args":{"target":2}}}
    {"source":"meta-2","kind":"parallelism_changed","data":{"requested":2,"effective":2,"via_directive":2}}
    {"source":"meta-2","kind":"turn_completed","data":{"…":"…","outcome":"Yielded"}}
  metric fold: mechanical `fulfilled = count(directive_issued where tier==Mechanical)` (ADR 0022);
  a guard-failed mechanical (e.g. sleep_agent on a non-Idle target) would instead return
  `rejected` and emit NO directive_issued at all.
```
This confirms the mechanical mechanism is coherent; the only question F1 raises is whether
the **canonical single-meta demo** is the right place to show it.
