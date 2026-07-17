# The streamed wire contract: verbatim-event SSE with arithmetic resume, a server-folded snapshot, and a cheap run list

This ADR pins the stream server's wire contract (#38, #39): the transport, the
endpoints, and the JSON shapes a web GUI is built against. The substance is
**snapshot-then-deltas, delivered as composable endpoints** — a pure raw SSE
event stream plus a separate server-folded projection — rather than a stream
preamble. The GUI fetches the snapshot, then opens the stream from the
snapshot's `as_of`; a minimal GUI can poll the snapshot alone and skip client
folding entirely. Transport research with primary sources:
`docs/research/stream-transport.md`. Topology and run states: ADR 0027. Route
versioning (the `/v1/` prefix on every path below) and the compat promise:
ADR 0029. Exact field names, casings, and status codes:
`docs/implementation-pins.md` §9.

## Three contract endpoints

- **`GET /v1/runs`** — the run list (cheap fields, poll-only; below).
- **`GET /v1/runs/{run_id}/snapshot`** — the server-folded projection
  `{ as_of, run, board, agents, metrics }` (below).
- **`GET /v1/runs/{run_id}/events`** — the per-run SSE stream (below).

## The transport is SSE

The stream is **Server-Sent Events** (`text/event-stream`). For a one-way,
append-only stream into a browser it is not a close call — the browser's
`EventSource` gives automatic reconnection **with resume** (`Last-Event-ID`) as
a platform built-in, and that resume protocol is exactly the `EventId`
contract; axum 0.8 (the workspace pin) ships SSE in its **default** feature
set, slotting into the ADR 0019 `build_router()` precedent with zero new
dependencies. WebSocket buys bidirectionality ruled out of read-only v1 at the
cost of a non-default feature flag plus hand-rolled reconnect/resume; chunked
NDJSON has no browser client at all — every consumer would reimplement
`EventSource` badly. NDJSON stays the right *at-rest* format (ADR 0022); SSE is
the browser *wire* framing of the same JSON lines.

## The SSE stream: verbatim payloads, arithmetic resume

One SSE event per `events.jsonl` line:

- **`id:`** = the decimal `EventId`.
- **`data:`** = the **verbatim `events.jsonl` line** — the same JSON bytes, no
  re-serialization. The event schema on the wire *is* ADR 0022; its evolution
  is ADR 0022 versioning (carve-out in ADR 0029).
- **No `event:` field on log events** (default event type): clients dispatch on
  the payload's `kind`, same as every other reader of the log, and `kind` as an
  SSE event name would break `onmessage` consumers for no fold benefit.

**Resume is exact arithmetic** on ADR 0022's contiguous monotonic `EventId`:
the browser re-sends the last seen `id:` as the `Last-Event-ID` request header
(spec-mandated); the server parses it as u64 `n` and replays from `n + 1`. A
fresh connect carries no header and means "from `EventId` 0". A `?from=<id>`
query parameter is honored with the same semantics — the resume fallback for
page reloads (the spec keeps `Last-Event-ID` per-`EventSource`-object only) and
for curl. An unparseable resume id is a client bug: **400**, fail loudly, don't
guess. Replay is always serveable because `events.jsonl` is the durable
backlog: a client can never ask for an id the server has forgotten.

Defensive stream headers, pinned now though loopback-only v1 dodges the actual
pitfalls: `Cache-Control: no-cache`, `X-Accel-Buffering: no`, no compression on
the stream route. Keep-alive `:` comments every **15 s** (under nginx's 60 s
`proxy_read_timeout` default) and a **`retry: 2000`** ms reconnection hint —
both `ServeConfig` defaults (ADR 0030), pinned in `docs/implementation-pins.md`
§9. Known accepted limit: ~6 concurrent SSE streams per origin on HTTP/1.1;
the recorded mitigation, if a many-runs dashboard ever demands it, is one
additive multiplexed all-runs endpoint, not a transport change.

## Terminal runs: 204 on caught-up reconnect, `run_state` control frame on abort

- A caught-up connect or reconnect to **any terminal run — finished or
  aborted — gets 204 No Content**, the spec-blessed signal that stops
  `EventSource` permanently.
- **Live → finished needs no control frame**: the `run_finished` event is
  in-band.
- **Live → aborted** (tailer sees the flock freed with no `run_finished`
  bookend, ADR 0027): the server sends a **synthetic, id-less, named SSE
  control frame** — `event: run_state`, `data: {"state":"aborted"}` — then ends
  the stream. Id-less, it never perturbs `Last-Event-ID` resume arithmetic;
  named, it can't be mistaken for a log event by `onmessage` consumers.

The carve-out, pinned verbatim: **"id-less named frames are server-origin
control frames, not log events."**

## Slow consumers: bounded broadcast, disconnect on lag

Per live run, one bounded `tokio::sync::broadcast` channel (capacity **1024**,
a `ServeConfig` default) carries freshly committed events; `events.jsonl` on
disk is the unbounded-but-durable backlog. A connection's stream is
*subscribe first → file catch-up → live tail*, deduping the overlap by
`EventId` (cheap because contiguous). On `RecvError::Lagged` the server **ends
that connection's stream** — continuing past a lag would deliver a gap, which
the contiguous-`EventId` contract forbids — and the client reconnects and
replays losslessly from the file. The bounded buffer costs overflow victims a
reconnect round-trip, never data.

## The snapshot: `{ as_of, run, board, agents, metrics }`

`GET /v1/runs/{run_id}/snapshot` is the server-folded projection — the server
folds, so the client never replays from event 0 and never reimplements the
metrics fold. Five blocks, minimizing genuinely new contract surface:

- **`as_of: EventId`** — the id the fold has consumed through; doubles as the
  stream resume point (`?from=as_of`).
- **`run`** — the `run_started` header (run_id, goal, seed, agents,
  meta_agents, parallel, scenario, caps) plus current state
  `live | finished | aborted` (ADR 0027).
- **`board`** — **the already-pinned `board.json` `BoardSnapshot` shape,
  verbatim** (tasks with inline Done results + `result_ref`, teams — ADR 0022).
  No new board schema.
- **`agents`** — per-agent `{ handle, specialty, state }` with the four-state
  vocabulary below. (The one genuinely new surface.)
- **`metrics`** — **`RunSummary` serialized**: the wire snapshot is the
  *fourth view* of ADR 0020's one-computation-N-views, not a new computation
  (the `Serialize` derive is one of ADR 0030's two granted core changes).

**Load-bearing grounding fact**: the server must build a reader-side fold over
`events.jsonl` *regardless* of what the GUI does — `board.json` is written only
at finalize, so **live** runs don't have one yet and **aborted** runs (no
finalize) never get one. **One uniform fold code path serves all three run
states** — the server folds `events.jsonl` and never reads `board.json`.
Corollary: for finished runs, *folded snapshot ≡ board.json* is a free
equivalence contract test (adopted at two tiers by ADR 0030).

## Agent-state vocabulary: four states

`idle | working{task} | asleep | parked`. The K=3-malformed park is exactly the
fault a dashboard must surface; collapsing it into asleep hides it. The
defining fold, pinned: `task_claimed` → working; `task_released` /
`task_unassigned` / `task_completed` → idle; `agent_slept` → asleep;
`agent_parked` → parked; `agent_woke` → the event's restored `Working{task}` /
`Idle`. The internal `MeterState` (which collapses parked into Asleep) is
**not** to be changed — the serve fold defines its own four-state wire
vocabulary (ownership: ADR 0030).

## The run list: cheap fields, poll-only

`GET /v1/runs` returns, per run: `run_id`, `state` (live/finished/aborted),
`goal`, `seed`, `started_at` (informational RFC3339 from event 0's `at`),
`last_event_id`, and for finished runs `{ reason, exit_code }` from
`run_finished`. **Counts are excluded** — they'd force a full metrics fold of
every historical run just to render a list; they live one click away in the
per-run snapshot. **Poll-only in v1** (no list stream; the recorded mitigation
above — one multiplexed all-runs SSE endpoint — covers a live dashboard if ever
demanded). UUIDv7 run ids give chronological order for free (ADR 0022).

## Rejected

- **WebSocket** — bidirectionality v1 ruled out, a non-default axum feature +
  three transitive deps, and it throws away the one hard part SSE gives free:
  reconnect-with-resume. When the control-plane seam opens, a WebSocket or
  plain-POST endpoint *beside* this stream is additive; nothing here precludes
  it.
- **Chunked NDJSON** — no browser client; every consumer hand-rolls
  incremental parsing, reconnect, resume, and keep-alive — i.e. reimplements
  `EventSource` badly — to save the server a `data:` prefix.
- **Raw-stream-only (no snapshot endpoint)** — forces the GUI to reimplement
  the full metrics fold (utilization, EventId-delta latencies, per-task token
  spend) in a second language: large, drift-prone duplication of
  `crates/openteam-core/src/metrics.rs`, plus whole-log replay on every fresh
  connect.
- **An in-stream snapshot preamble** — muddies the SSE semantics (what `id:`
  does the snapshot frame carry? is it re-sent on resume?) and makes the
  snapshot unreachable without opening a stream; a separate GET is
  independently refetchable and curl-able.
- **`kind` as the SSE `event:` name** — breaks plain `onmessage` consumers for
  no fold benefit; clients dispatch on the payload's `kind`.
- **Bare EOF for abort (client infers by absence)** — indistinguishable from a
  server restart until reconnect; explicit beats inference for a terminal fact.
- **Counts in the run list** — a full fold of every historical run per list
  render; the snapshot is one click away.
- **Continuing a lagged broadcast subscriber past the gap** — would deliver a
  gap, forbidden by `EventId` contiguity; disconnect-and-replay is lossless.
