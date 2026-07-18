# The `/v1/` path prefix is the single version marker, evolution under it is additive-only, and the debug page at `/` is a non-contract surface

This ADR pins the compat posture of the streamed surface (#44) and the one
deliberately *unversioned* surface, the debug page (#43). The surface being
versioned is exactly what ADR 0028 fixed: the run list, the snapshot, the SSE
stream, and its control frames.

## The `/v1/` prefix: one version marker, on every contract route

All contract routes mount under `/v1/`:

- `GET /v1/runs`
- `GET /v1/runs/{run_id}/snapshot`
- `GET /v1/runs/{run_id}/events`

Two deciding arguments:

1. **Asymmetric cost.** Adding the prefix now is a single
   `Router::nest("/v1", ...)` line in the ADR 0019 `build_router()` precedent;
   retrofitting one later changes every client URL — the one genuinely
   expensive migration this surface could face. Loopback-only single-binary v1
   makes versioning ceremony *feel* unwarranted, but the wire contract outlives
   v1's binding posture (`docs/research/stream-transport.md` §4), and
   un-prefixed paths are the choice that can't be cheaply undone.
2. **The verbatim-payload pin forces it.** The SSE `data:` payload is the
   verbatim `events.jsonl` line (ADR 0028), so the stream *cannot* carry an
   in-band version field. The path is the only place the stream can carry a
   version at all.

**Carve-out:** the debug page at `GET /` (below) sits **outside** `/v1/` — a
non-contract surface, unversioned by design. Only contract routes live under
the prefix.

## The compat promise: additive-only evolution under `/v1/`

The promise to a GUI built against v1:

- The server **may add** — endpoints under `/v1/`, JSON fields to list/snapshot
  responses, and new id-less **named** SSE control-frame types (ADR 0028's
  carve-out sentence already reserves that space).
- The server **never removes, renames, or retypes** anything under `/v1/`.
- **Client-side forward-compat rule, pinned explicitly:** clients must ignore
  unknown JSON fields and unknown named SSE frames. This rule is what makes
  additive-only actually work — without it, additions are de facto breaking.
- **Breaking changes mount `/v2/` alongside** `/v1/`; `/v1/` is not mutated.

**Carve-out, verbatim:** SSE data payloads are ADR 0022 verbatim — their
evolution is ADR-0022 versioning, outside this promise (and taxonomy changes
are out of scope on map #36).

## No second version marker

No `schema_version` field in the snapshot or run list, no version response
header. Under additive-only evolution a per-payload version never changes
within `/v1/`, so it's dead weight — and **two version markers invite skew**
between them. The path prefix is the one and only version marker.

## The debug page: `GET /`, ships in v1, never part of the contract

`openteam serve` ships a debug page in v1: a single static HTML file embedded
via `include_str!` (matching the `openteam-mock` embedded-fixture idiom),
served at **`GET /`** — **zero dependencies, zero framework, zero build step**,
one axum route on the ADR 0019 `build_router()` precedent.

**Why ship it**: the page is the only cheap way to exercise **browser-only
`EventSource` semantics against the real server** — auto-reconnect,
`Last-Event-ID` arithmetic resume, the 204 terminal stop, and the id-less
`run_state` control frame (ADR 0028) are behaviors curl cannot reach. It
doubles as living documentation of the contract for the future GUI developer.

**What it renders** — all three contract surfaces, no interpretation:

- **Run list** from the list endpoint; click to select a run.
- **Snapshot** on select: fetch it and pretty-print the JSON verbatim.
- **Event tail**: a bare `EventSource` on the run's SSE stream; one `<pre>`
  line appended per event (`onmessage`), plus a named listener for the
  `run_state` control frame.

The hard line, pinned verbatim: **"the page never interprets, folds, or styles
domain data — it only tails and dumps."** Anything that renders the board or
agents as UI is rejected-by-construction — that is the web GUI, out of scope on
the map.

**Contract status**: the debug page is a **debug surface, not part of the
pinned wire contract — changeable or removable without any compatibility
promise**. It is excluded from the versioned surface above and from contract
tests; at most a "`GET /` returns 200 `text/html`" smoke assertion (ADR 0030).
Nothing else claims `/` — all contract surfaces live under `/v1/runs...` — so
there is no collision, and a bare `curl localhost:PORT/` or browser visit lands
somewhere useful.

## Rejected

- **Bare (un-prefixed) contract paths** — the one choice that can't be cheaply
  undone; retrofitting a prefix changes every client URL.
- **A version field in each JSON response, with bare paths** — cannot cover the
  stream (its payloads are ADR 0022 verbatim); versions only what's easiest to
  version.
- **A `schema_version` field or version header *in addition to* the prefix** —
  never changes under additive-only evolution, and two markers invite skew.
- **No-promise same-repo lockstep** — honest for the monorepo world but makes
  the spec worthless as an interface contract and silently strands any
  curl/third-party consumer.
- **No debug page** — leaves the browser-only `EventSource` semantics
  unexercisable without building the real GUI first.
- **A framework/build-step page, or one that renders domain UI** — that is the
  web GUI, out of scope; the `include_str!` dump is the whole point.
