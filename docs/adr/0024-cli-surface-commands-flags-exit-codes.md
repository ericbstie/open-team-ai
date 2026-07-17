# The CLI surface: a two-command clap tree, a random-per-run seed default resolved in the bin, and phase-scoped exit codes

This ADR consolidates the CLI surface previewed across the map — every command,
flag, default, type, env var, and output/exit behavior of the `openteam` binary —
and pins it at the clap-derive type level. Nearly every flag was decided upstream
(ADRs 0001/0006/0015/0018/0019/0020/0022/0023); this ADR is the single place the
exact surface lives, plus the three genuinely-new decisions: the `--seed` default
policy, the exit-code phase distinction, and the `--quiet`/stdout contract.

## The command tree — `run` and a `mock` group

`openteam` is a clap-derive `Parser` with two subcommands. `mock` is a **command
group** (`openteam mock serve`), not a bare `openteam serve`, so future mock
tooling has a home (ADR 0019).

```rust
use clap::{Parser, Subcommand, Args, ArgAction};
use std::path::PathBuf;
use url::Url;

#[derive(Parser)]
#[command(name = "openteam", version, about =
    "An offline, deterministic harness for parallelized agentic teams")]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Increase stderr tracing verbosity (-v = DEBUG, -vv = TRACE).
    #[arg(short, long, global = true, action = ArgAction::Count)]
    verbose: u8,

    /// Silence stderr tracing entirely (stdout still receives the full report).
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    quiet: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Run the harness against a goal prompt.
    Run(RunArgs),
    /// Mock LLM server tooling.
    Mock {
        #[command(subcommand)]
        command: MockCommand,
    },
}

#[derive(Subcommand)]
enum MockCommand {
    /// Serve the OpenAI-schema mock over loopback HTTP.
    Serve(ServeArgs),
}

#[derive(Args)]
struct RunArgs {
    /// The goal for the team to accomplish.
    goal: String, // required positional

    /// Team-agent pool size (created at run start, never destroyed).
    #[arg(long, default_value_t = 4, value_name = "N")]
    agents: usize,

    /// Max concurrently-active team agents (default = --agents; must be <= --agents).
    #[arg(long, value_name = "N")]
    parallel: Option<usize>,

    /// Number of meta-agents (0 disables the meta layer; metrics still emitted).
    #[arg(long, default_value_t = 1, value_name = "N")]
    meta_agents: usize,

    /// Seed for deterministic mock behavior (default: random per run, logged and printed).
    #[arg(long, value_name = "U64")]
    seed: Option<u64>,

    /// Cap: max orchestrator ticks before forced termination.
    #[arg(long, value_name = "N")]
    max_ticks: Option<u64>,

    /// Cap: max LLM completions before forced termination.
    #[arg(long, value_name = "N")]
    max_llm_calls: Option<u64>,

    /// Cap: max wall-clock seconds before forced termination.
    #[arg(long, value_name = "SECS")]
    max_duration: Option<u64>,

    /// Per-turn tool-call iteration cap.
    #[arg(long, default_value_t = 8, value_name = "N")]
    max_tool_iters: u32,

    /// External OpenAI-compatible endpoint; if set, the in-process mock is NOT
    /// started (config-only, untested — ADR 0001).
    #[arg(long, value_name = "URL")]
    llm_base_url: Option<Url>,

    /// Bearer token for --llm-base-url. Prefer the env var; a CLI-passed key can
    /// leak via shell history / `ps`.
    #[arg(long, env = "OPENTEAM_LLM_API_KEY", value_name = "KEY", hide_env_values = true)]
    llm_api_key: Option<String>,

    /// Scenario fixture overriding the built-in behavior arc (ADR 0023).
    #[arg(long, value_name = "FILE")]
    scenario: Option<PathBuf>,

    /// Run-artifacts directory (default: .openteam/runs/<uuidv7>/).
    #[arg(long, value_name = "DIR")]
    out_dir: Option<PathBuf>,
}

#[derive(Args)]
struct ServeArgs {
    /// Port to bind (0 = OS-assigned ephemeral port, printed on startup).
    #[arg(long, default_value_t = 0, value_name = "PORT")]
    port: u16,

    /// Scenario fixture overriding the built-in behavior arc (ADR 0023).
    #[arg(long, value_name = "FILE")]
    scenario: Option<PathBuf>,
}
```

**Types.** Pool sizes and counts (`--agents`, `--parallel`, `--meta-agents`) are
`usize`; safety caps (`--max-ticks`, `--max-llm-calls`, `--max-duration`) are
`u64` (they fold against the `u64` `EventId` / tick counts of ADR 0022);
`--max-tool-iters` is `u32` (matches `turn_completed.tool_iters`, ADR 0022);
`--seed` is `u64` (the wire seed, ADR 0018); `--port` is `u16`. `--max-duration`
is **whole seconds** (`u64`), converted to milliseconds internally for
`run_started.caps.max_duration_ms` — no `humantime` dependency for one safety cap.

**Flag placement.** `-v/-vv` and `--quiet` are **global** (`global = true`): one
tracing dial accepted by both `run` and `mock serve`, since the mock server emits
tracing too. Everything else is subcommand-local. `--llm-api-key` lives only on
`run` — the mock server *is* the endpoint and calls no LLM. `--scenario` appears on
both `run` and `mock serve` (ADR 0023), setting `AppState.scenario` before the
shared `build_router()` (ADR 0019).

## The seed default is random-per-run, resolved in the bin, logged and printed

`--seed` defaults to **a fresh value drawn from OS entropy at startup, per run**.
Determinism is a property of a *seed*, not of an *invocation* (ADR 0021), so a
random default weakens no guarantee — it picks a fresh seed each run, showcasing
the seed→variety dial and keeping demo runs alive, while the test suite (#23)
always passes an explicit `--seed`.

The resolution happens **in the binary, never in the deterministic core** — the
core only ever receives an already-resolved `u64`, so it stays a pure function of
its inputs. The bin then:

1. records the resolved seed in the `run_started { seed, … }` event (ADR 0022), and
2. prints `run seed: <n>` to **stderr at INFO**,

so any interesting run is reproducible on demand with `--seed <n>`. This is the
standard seeded-tool pattern: fresh yet reproducible.

## Exit codes are phase-scoped; ADR 0006's 0/2/1 is unchanged

ADR 0006's run-outcome codes ride `run_finished.exit_code`: **0** clean
`finish_run`, **2** cap hit, **1** harness error. Those govern runs that *began*
(a `run_started` was emitted, an artifacts dir exists).

A **CLI usage or validation error** — a clap parse failure, or the post-parse
cross-field check that **errors when `--parallel > --agents`** (mandated by
ADR 0015) — exits **2** to stderr via clap's conventional usage-error code, with
**no run starting**: no `run_started`, no artifacts dir. This overlaps numerically
with cap-hit=2 but not harmfully — the two 2s live in **different phases** and are
trivially distinguishable: a usage-error 2 leaves no artifacts directory and no
`run_started` event; a cap-hit 2 has both. Fighting clap to remap its conventional
2 would be worse than this benign, standard overlap (most CLIs have exactly it).
The `--parallel > --agents` check is a post-parse validation (clap-derive can't
express cross-field constraints), reported as a clap error so it exits 2 uniformly
with parse failures.

## The output contract: stdout is the report, stderr is tracing, `--quiet` silences tracing

- **stdout** receives the assembled `report.md` verbatim — the orchestrator's
  `finish_run` report followed by the `## Run summary` block — **byte-identical to
  the persisted `report.md`** (ADR 0022: the finalize step renders once, to both
  the file and stdout). This holds on the cap-hit path too (stub report + summary).
- **stderr** receives all `tracing` output (default level INFO; `-v` DEBUG,
  `-vv` TRACE), so stdout stays pipeable.
- **`--quiet`** silences the stderr tracing subscriber entirely so the *only*
  output is the report on stdout. It **never trims stdout** — the report is always
  the full report, preserving the stdout==`report.md` invariant, so
  `openteam run --quiet | …` produces exactly the persisted report. `--quiet`
  `conflicts_with` `--verbose`. (`mock serve` has no report; there `--quiet` simply
  means quiet logs.)

## Environment

`OPENTEAM_LLM_API_KEY` is the only env var, wired via clap's `env` on the visible
`--llm-api-key` flag (ADR 0018) — discoverable in `--help`, settable either way,
its value masked by `hide_env_values`. Sent as `Authorization: Bearer <key>` only
when talking to a `--llm-base-url` endpoint (untested real-endpoint escape hatch,
ADR 0001). There is **no config file** (map Out-of-scope) — flags + env only.

## Rejected

- **A fixed default seed (`--seed 0`, reproducible-by-default)** — every no-flag
  run would be identical, hiding the seed→variety dial the product exists to show;
  reproducibility is already available on demand (the resolved seed is logged and
  printed), and tests pin seeds explicitly, so the random default costs nothing.
- **Resolving the random seed inside the deterministic core** — would make the core
  impure (reading OS entropy); the bin resolves it and hands the core a `u64`.
- **Remapping clap's usage-error exit code off 2** (to keep 2 == cap-hit
  exclusively) — fights the clap convention for a distinction already carried by
  phase (no `run_started` / no artifacts on a usage error); the benign overlap is
  what most CLIs have.
- **`--quiet` trimming stdout to the bare `finish_run` answer** (dropping the
  `## Run summary`) — breaks ADR 0022's stdout==`report.md` invariant and makes a
  piped run diverge from the persisted report; `--quiet` is a stderr concern only.
- **A bare `openteam serve`** (no `mock` group) — leaves no room for future mock
  tooling; the group is ADR 0019's call.
- **`humantime` for `--max-duration`** — a whole dependency for one safety cap;
  whole `u64` seconds suffice (revisit if demand appears).
- **A config file (`openteam.toml`)** — map Out-of-scope; flags + env only in v1.

**Amended by ADR 0026 (2026-07-17).** `run` gains `--mock` (bool; starts the
built-in mock; `conflicts_with = "llm_base_url"`), `--model <ID>` (`OPENTEAM_MODEL`,
`Option<String>`), and `--embedding-model <ID>` (`OPENTEAM_EMBEDDING_MODEL`,
`Option<String>`); `--scenario` now carries `requires = "mock"`. The default
`base_url` is the constant `https://api.openai.com/v1` (used when neither `--mock`
nor `--llm-base-url` is given), so the real endpoint is the default and the mock is
the opt-in. API-key resolution adds a fallback to the conventional `OPENAI_API_KEY`
after `--llm-api-key`/`OPENTEAM_LLM_API_KEY`; hitting the default OpenAI URL with no
resolved key fails fast in the validation phase (exit 2, no artifacts, friendly
message), while a custom `--llm-base-url` with no key is allowed (local servers).
Model ids are now flags/defaults (real path: `gpt-4o-mini` chat,
`text-embedding-3-small` embeddings; under `--mock`: both `openteam-mock`) rather
than the hardcoded `openteam-mock`.
