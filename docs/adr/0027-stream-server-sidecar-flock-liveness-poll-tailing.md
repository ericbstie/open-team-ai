# The stream server is a sidecar log-tailer: `run.lock` flock liveness, three run states, and poll-based complete-line tailing

The **stream server** (`openteam serve`) is the read-only server that streams run
information — live and historical — out of `.openteam/runs/` so a web GUI can be
built against it (map #36). This ADR pins its topology, its liveness signal, its
tail mechanics, and its CLI surface (#37). Companion ADRs: 0028 (the streamed
wire contract), 0029 (versioning and the debug page), 0030 (crate placement and
test posture).

## Sidecar-only: one server process, tailing the filesystem

The stream server is a **separate sidecar process** that discovers and tails run
directories under one filesystem root. It is not part of `openteam run`, and
there is no in-process listener variant.

The charting decisions already lock **multi-run discovery and historical
serving** — so the log-tailer code path must exist regardless of topology. From
there the alternatives collapse:

- An **in-process second listener** inside `openteam run` can only ever serve
  its *own* run — structurally insufficient alone (no historical runs, no other
  live runs).
- **Both behind one abstraction** would be a redundant second live path bought
  at the cost of runtime coupling — extra tasks and world-lock reads inside the
  deterministic runtime, and a server dying with the run, exactly when you want
  to inspect it — for **no capability gain**: the live-tail fidelity concern
  dissolves on inspection. `EventsWriter::append`
  (`crates/openteam-core/src/artifacts.rs`) flushes the `BufWriter` per event on
  the serialized write path (ADR 0022's append+flush-per-event), so each event
  reaches the page cache via `write(2)` immediately — a same-host tailer sees it
  essentially instantly. fsync (absent today) matters only for power-loss
  durability, not cross-process visibility, so the sidecar's live latency is
  ~instant without any change to the writer.

## Liveness: an exclusive flock on `run.lock`, and three run states

Today the filesystem distinguishes only *finished* vs *not-finished*: the
`run_finished` bookend is written on all three in-process termination paths
(ADR 0022), but nothing distinguishes a run that is still going from one that
died on `SIGKILL` or a panic — no lock or pid file exists.

**The one granted exception to "zero change to `openteam run`"**: `openteam run`
is amended to hold an **exclusive OS advisory lock (`flock`) on
`<run-dir>/run.lock`** for the run's lifetime. The kernel releases the lock on
*any* process death, including `SIGKILL`; the change has zero determinism impact
and touches no event schema. This is a deliberate, minimal exception — and the
*only* one granted (ADR 0030 grants two further changes to `openteam-core`'s
*library surface*; this is the only change to the run *process*'s behavior).

The bookend × lock trichotomy pins **three run states**, used across the whole
streamed surface (ADR 0028):

| state | `run_finished` bookend | `run.lock` |
|---|---|---|
| **`finished`** | present | (irrelevant) |
| **`live`** | absent | held |
| **`aborted`** | absent | free |

## Tail mechanics: 100 ms poll, complete newline-terminated lines only

Tailing is **poll-based**, at a 100 ms interval (the number is pinned in
`docs/implementation-pins.md` §9 as a `ServeConfig` default, ADR 0030).
`notify`-crate/inotify watching is **rejected-for-now**: platform quirks against
an imperceptible latency win, while polling is portable and trivially
deterministic to test per the ADR 0025/0026 posture.

Hard rule (the **complete-line rule**): the tailer **only parses complete
newline-terminated lines**. `BufWriter`'s 8 KiB buffer can tear a >8 KiB event
line across `write(2)` calls, so a torn final line is observable; the partial
tail is left in place and re-read on the next poll, and is never emitted or
parsed.

## CLI: `openteam serve --dir --port`; no co-launch

- **`openteam serve`** is a new top-level subcommand (the name was deliberately
  kept free by ADR 0024; the bin's clap tree gains a `Serve(...)` variant in
  ADR 0024's style — ADR 0030). It is a thin wrapper over the `openteam-serve`
  library crate.
- **`--dir <runs-root>`**: exactly **one discovery root per server instance**,
  default `.openteam/runs`. Runs redirected via `run --out-dir` are **not
  discovered** in v1 — a documented limitation, not fog. Multiple roots and
  individual run-dir arguments are out of scope for v1.
- **`--port <PORT>`**: loopback-only binding in v1; `0` = OS-assigned ephemeral
  port, matching `mock serve` (ADR 0024).
- On startup the server **prints its bound address in a parseable form** — one
  line, exact format pinned in `docs/implementation-pins.md` §9 — load-bearing
  for scripting and for the ephemeral-port e2e test (ADR 0030).
- **No co-launch/announce flag on `openteam run`** in v1: the server is a
  multi-run singleton, and per-run co-launch has muddy ownership (port fights;
  the server outliving the run is the point). Shell composition works because
  discovery is filesystem-based. A static one-line stderr hint on `run` is
  implementer's taste, **not spec** — unpinned.

## Rejected

- **An in-process second listener inside `openteam run`** — can only serve its
  own run; multi-run and historical serving (charting-locked) demand the tailer
  anyway.
- **Both topologies behind one abstraction** — a redundant second live path,
  runtime coupling inside the deterministic core, and a server that dies with
  the run, for no capability the flushed log doesn't already give a sidecar.
- **inotify/`notify`-based tailing** — platform quirks vs. an imperceptible
  latency win over a 100 ms poll; rejected-for-now, revisit only on demonstrated
  need.
- **A pid file (or bookend absence alone) as the liveness signal** — a pid file
  lies after `SIGKILL` (stale file, reused pid); the kernel-released flock
  cannot.
- **A co-launch flag on `openteam run`** — muddy ownership for a multi-run
  singleton; composition via the filesystem is already sufficient.
- **Multiple discovery roots / per-run-dir arguments** — out of scope for v1;
  one root keeps discovery trivial.
