# Stream transport: SSE vs WebSocket vs chunked NDJSON for the event stream

Research for the stream server's browser-facing transport — a **one-way,
append-only** stream of ADR 0022 events (live tail + historical replay) out of
`.openteam/runs/<run-id>/events.jsonl`, with resume keyed on the contiguous
monotonic `EventId`. Resolves wayfinder ticket
[#39](https://github.com/ericbstie/open-team-ai/issues/39).

**Verdict: SSE.** The standing leaning survives contact with the primary
sources — and it is not a close call for this shape of problem. The browser's
`EventSource` gives automatic reconnection **with resume** (`Last-Event-ID`)
as a platform built-in, and that resume protocol is *exactly* our
`EventId` contract; axum 0.8 ships SSE in its default feature set; WebSocket
buys bidirectionality we've ruled out of v1 at the cost of a non-default
feature flag and hand-rolled reconnect/resume; chunked NDJSON is SSE with
every convenience removed.

**Primary sources** (retrieved 2026-07-17):

- WHATWG HTML Standard, §9.2 Server-sent events —
  <https://html.spec.whatwg.org/multipage/server-sent-events.html>. The
  normative spec for `EventSource`, the `text/event-stream` format, and
  reconnection. All `Last-Event-ID` claims below are from here.
- MDN, "Using server-sent events" —
  <https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events/Using_server-sent_events>
  (browser connection limits, required headers).
- axum 0.8.9 API docs (the version in this workspace's `Cargo.lock`;
  workspace pin `axum = "0.8"`) —
  <https://docs.rs/axum/0.8.9/axum/response/sse/index.html> and
  <https://docs.rs/axum/0.8.9/axum/extract/ws/index.html>, plus the crate
  feature list at <https://docs.rs/crate/axum/0.8.9/features>.
- nginx `ngx_http_proxy_module` docs —
  <https://nginx.org/en/docs/http/ngx_http_proxy_module.html>
  (`proxy_buffering`, `X-Accel-Buffering`, `proxy_read_timeout`) as the
  canonical example of intermediary buffering.
- tokio `sync::broadcast` docs —
  <https://docs.rs/tokio/latest/tokio/sync/broadcast/index.html> (bounded
  ring buffer, `RecvError::Lagged` semantics) for the slow-consumer
  mechanics. Workspace already depends on `tokio 1.42` with the `sync`
  feature.

---

## 1. The three candidates

| | SSE (`text/event-stream`) | WebSocket | Chunked NDJSON |
|---|---|---|---|
| Direction | server→client only — **matches read-only v1 exactly** | bidirectional — capability we ruled out | server→client only |
| Browser API | `EventSource`: built-in parse, named events, **auto-reconnect + `Last-Event-ID` resume** | `WebSocket`: framing only; reconnect/resume is app code | none — hand-roll `fetch` + `ReadableStream` line splitting, reconnect, resume |
| axum 0.8.9 | `axum::response::sse::{Sse, Event, KeepAlive}` — **in the default features**, no flag | requires non-default `ws` feature (+ `tokio-tungstenite`, `sha1`, `base64` deps) | plain `Body::from_stream`, trivial but featureless |
| Wire format | line fields `data:` / `id:` / `event:` / `retry:`, UTF-8 only, `:` comment lines for keep-alive | binary/text frames, ping/pong built in | bare JSON lines; no keep-alive or comment convention |
| Resume protocol | **in the spec**: browser re-sends last seen `id` as `Last-Event-ID` request header | none — invent your own | none — invent your own |
| End-of-stream | HTTP 204 on reconnect stops `EventSource` for good (spec) | close frame | connection close (client must guess: done or drop?) |
| Ops profile | plain HTTP: works through proxies *if* they don't buffer (§4) | Upgrade handshake; some intermediaries block it | plain HTTP, same buffering caveats as SSE, minus the ecosystem awareness |

Why the alternatives lose, honestly stated:

- **WebSocket** is the right transport when the client *talks back* —
  precisely the control-plane seam the map notes and defers. For v1 it costs
  a new non-default cargo feature and three transitive deps, and throws away
  the one hard part SSE gives us free: reconnect-with-resume. When (if) the
  control plane arrives, adding a WebSocket endpoint *alongside* the SSE
  stream is additive; nothing decided here blocks it.
- **Chunked NDJSON** is seductive because `events.jsonl` *is* NDJSON — the
  server could nearly `sendfile` it. But the browser has no NDJSON client:
  every consumer hand-rolls incremental line parsing, reconnection, resume
  signalling, and keep-alive detection — i.e. reimplements `EventSource`
  badly. The server-side saving is negligible: the SSE framing of an event is
  the same JSON line prefixed with `data:` and an `id:` line. NDJSON remains
  the right *at-rest* format (ADR 0022); it is the wrong *browser wire*
  format.

## 2. `Last-Event-ID` ↔ `EventId`: a perfect fit

The WHATWG spec's resume mechanism maps onto ADR 0022's ordering key with no
adapter logic at all:

- The server emits each event with an SSE `id:` field set to the decimal
  `EventId`. Spec constraint: the id value must not contain U+0000 NULL
  (a decimal u64 trivially satisfies this; the field is otherwise an opaque
  string).
- The browser tracks the "last event ID string" and, on every automatic
  reconnection, sends it back: *"Set (`Last-Event-ID`, lastEventIDValue) in
  request's header list."* The header is only sent when the string is
  non-empty — so a **fresh connect carries no header** and cleanly means
  "from the beginning" (EventId 0, `run_started`).
- Because `EventId` is **contiguous** (0-based, no gaps — ADR 0022 as amended
  by the #22 dry-run gate), resume is exact arithmetic: parse the header as
  u64 `n`, replay from `n + 1`. No gap detection, no sequence-repair
  protocol, no client-side dedupe beyond "ids are increasing".
- Replay is always serveable: `events.jsonl` is streamed append+flush per
  event (ADR 0022), so the file on disk *is* the authoritative backlog up to
  the last committed event. A reconnecting client can never ask for an id the
  server has forgotten — the durable spine makes the bounded live buffer safe
  (§5).
- `retry:` lets the server pin the reconnection delay; without it the spec
  leaves the initial retry implementation-defined ("probably in the region of
  a few seconds", possibly with exponential backoff).
- **Terminal runs get a spec-blessed ending**: an HTTP **204 No Content**
  response tells `EventSource` to stop reconnecting permanently. So: client
  is fully caught up on a run whose `run_finished` has been sent → its next
  reconnect (with `Last-Event-ID` = final id) gets 204 → the browser stops.
  The GUI can also just `.close()` on seeing `run_finished`; 204 is the
  server-side backstop for clients that don't.

One spec caveat worth recording: the last event ID string persists across
reconnections *per `EventSource` object*, not across page loads. A GUI that
wants resume across a refresh must persist the last id itself and pass it
back explicitly (e.g. a `?from=<id>` query parameter the endpoint honors with
the same semantics as the header). Supporting `?from=` also makes the stream
trivially curl-able from a given offset.

## 3. What axum 0.8.9 gives us natively

- **SSE: everything needed, zero new features/deps.**
  `axum::response::sse::Sse<S>` wraps any `Stream<Item = Result<Event, E>>`;
  `Event` is a builder with `.data()`, `.json_data()` (serde-serializes the
  payload — one call per ADR 0022 `Event`), `.id()`, `.event()`, `.retry()`,
  `.comment()`; `Sse::keep_alive(KeepAlive::...)` injects periodic `:` 
  comment lines with a configurable interval and text. This slots straight
  into the ADR 0019 `build_router()` precedent — an SSE route is an ordinary
  handler returning `Sse<impl Stream<...>>`.
- **WebSocket: gated behind the non-default `ws` cargo feature** (axum 0.8.9
  default features are `form, http1, json, matched-path, original-uri, query,
  tokio, tower-log, tracing`), pulling `tokio-tungstenite` + `sha1` +
  `base64`. Nothing in the docs offers reconnect or resume; that would all be
  our code and the GUI's code.
- **NDJSON: nothing dedicated** — it's just a streaming body; all framing,
  keep-alive, and resume conventions would be ours to invent on both ends.
- **Backpressure hook, all three cases**: the response stream is only polled
  when hyper can write to the socket, so a slow client simply stops the
  stream being polled — which is exactly where the bounded broadcast buffer
  (§5) absorbs, then overflows, then disconnects.

## 4. Proxy / buffering pitfalls (and why v1 mostly dodges them)

v1 binds loopback only (charting decision), so no intermediary sits on the
path and none of these can bite *yet*. But the streamed wire contract
outlives v1's binding posture, so the defensive posture is cheap to pin now:

- **Reverse-proxy response buffering.** nginx defaults to `proxy_buffering
  on`, which holds response bytes until buffers fill — an SSE/NDJSON stream
  appears to hang. nginx honors an **`X-Accel-Buffering: no`** response
  header to disable buffering per-response ("Buffering can also be enabled or
  disabled by passing `yes` or `no` in the X-Accel-Buffering response header
  field"). Emit it unconditionally on stream responses; it's inert
  elsewhere.
- **Idle timeouts.** nginx `proxy_read_timeout` defaults to **60s** "between
  two successive read operations" — an idle stream (agents thinking, no
  events) gets its connection closed. SSE's `:` keep-alive comments defeat
  this by construction; axum's `KeepAlive` exists for exactly this. Pin the
  interval well under 60s (§5). NDJSON has no comment syntax — it would need
  in-band no-op lines the client must filter, another point to SSE.
- **Compression.** `Content-Encoding` transforms (gzip et al.) buffer output
  in compressor-window-sized chunks, destroying per-event flush granularity.
  Don't negotiate compression on the stream route (and don't wrap it in a
  compression tower layer). `Cache-Control: no-cache` on the response keeps
  caches out of the path (MDN lists it as a required header alongside
  `Content-Type: text/event-stream`).
- **Browser connection limit.** Per MDN: over HTTP/1.1 a browser allows only
  **6** connections per origin (marked "Won't fix" in Chrome and Firefox);
  over HTTP/2 the limit is ~100 negotiated streams. Our axum server speaks
  HTTP/1.1 (`http1` feature; browsers won't do cleartext h2, and TLS is out
  of scope for v1), so a GUI holding **more than ~6 concurrent live streams
  to the same origin will stall**. Acceptable for v1 (a GUI watches one or a
  few runs); the recorded mitigation, if a many-runs dashboard ever needs it,
  is one multiplexed "all runs" SSE endpoint rather than N per-run
  connections — an additive endpoint, not a transport change.

## 5. Slow-consumer mechanics — the numbers to pin

Charting decided the posture (never buffer unboundedly; connect-from-
`EventId`; bounded buffer; on overflow disconnect so the client reconnects
from its last id). The mechanics that make it concrete:

**Shape.** Per live run, one bounded `tokio::sync::broadcast` channel carries
freshly committed events; `events.jsonl` on disk is the unbounded-but-durable
backlog. A connection's stream is: *subscribe first → file catch-up → live
tail*:

1. Resolve the resume point: `Last-Event-ID` header, else `?from=` param,
   else "from 0". Next wanted id `n = resume + 1` (or 0 on fresh connect).
2. Subscribe to the run's broadcast channel **before** reading the file
   (subscribe-then-catch-up closes the gap race).
3. Stream file events with `id >= n` to the client.
4. Switch to the subscription, discarding anything with `id <=` the last id
   already sent (overlap dedupe by `EventId` — cheap because contiguous).
5. For a **finished** run: serve the remaining file and end the response; a
   later reconnect whose resume point is at-or-past the final event gets
   **204 No Content** (stops `EventSource` permanently, §2).

**Overflow.** tokio's broadcast is a fixed-capacity ring: a send never
blocks, the oldest retained value is overwritten, and a receiver that missed
messages gets `RecvError::Lagged(missed)` with its cursor advanced past the
gap. Continuing from the advanced cursor would deliver a **gap** — which the
contiguous-`EventId` contract forbids — so on `Lagged` the server **ends that
connection's stream**. `EventSource` auto-reconnects with `Last-Event-ID`,
and the file catch-up path replays the missed events losslessly. This is the
charting posture falling out of the primitives: the bounded buffer costs
overflow victims a reconnect round-trip, never data.

**Numbers** (pin in the ADR; all per-connection cost is O(capacity) worst
case):

| knob | value | rationale |
|---|---|---|
| broadcast capacity | **1024** events | power of two (tokio rounds up anyway); deep enough that only a genuinely stalled client laps it, shallow enough to bound memory (~payload-sized × 1024 per run) |
| keep-alive comment interval | **15 s** | well under nginx's 60 s `proxy_read_timeout` default and typical LB idle timeouts; negligible traffic (`:` + newline) |
| `retry:` hint | **2000 ms** | prompt reconnection after an overflow disconnect without hammering; overrides the spec's vaguer implementation-defined default |
| resume parse failure | **400** | a non-u64 `Last-Event-ID`/`?from=` is a client bug; fail loudly, don't guess |

## 6. Recommendation

**SSE**, specifically:

- `Content-Type: text/event-stream`; one SSE event per ADR 0022 event:
  `id:` = decimal `EventId`, `data:` = the same JSON object as the
  `events.jsonl` line (single line, so exactly one `data:` field; UTF-8 —
  which JSONL already guarantees). Default event type (no `event:` field) in
  v1 — clients dispatch on the payload's `kind`, same as every other reader
  of the log, and `kind` as an SSE `event:` name would break `onmessage`
  consumers for no fold benefit.
- Resume: `Last-Event-ID` header, `?from=` query fallback; contiguous
  arithmetic resume from `n + 1`; 204 for caught-up-on-finished-run.
- Response headers: `Cache-Control: no-cache`, `X-Accel-Buffering: no`; no
  compression on the stream route.
- Keep-alive comments @ 15 s via axum `KeepAlive`; `retry: 2000`.
- Slow consumers: per-run bounded broadcast (capacity 1024),
  subscribe-then-catch-up connect sequence, disconnect on `Lagged`,
  `events.jsonl` as the lossless replay source.
- No `ws` feature, no new dependencies: axum 0.8.9's default features
  already cover everything above.

Known, accepted limits: ~6 concurrent streams per origin on HTTP/1.1
(mitigation if ever needed: a multiplexed all-runs endpoint); UTF-8-only
transport (moot — the log is UTF-8 JSON); resume across a page reload needs
the GUI to persist the last id and use `?from=` (spec keeps `Last-Event-ID`
per-object only). The control-plane seam, when it opens, points at a future
WebSocket or plain-POST endpoint *beside* this stream — nothing here
precludes it.
