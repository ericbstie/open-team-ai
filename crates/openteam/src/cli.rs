//! The CLI surface — pinned verbatim by ADR 0024: a two-command clap tree,
//! a random-per-run seed default resolved in the bin, and phase-scoped exit
//! codes.

use clap::{ArgAction, Args, Parser, Subcommand};
use std::path::PathBuf;
use url::Url;

#[derive(Parser)]
#[command(
    name = "openteam",
    version,
    about = "An LLM harness for parallelized agentic team working"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Increase stderr tracing verbosity (-v = DEBUG, -vv = TRACE).
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Silence stderr tracing entirely (stdout still receives the full report).
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,
}

#[derive(Subcommand)]
// The variant shapes are pinned verbatim by ADR 0024; a one-shot CLI enum
// gains nothing from boxing its largest variant.
#[allow(clippy::large_enum_variant)]
pub enum Command {
    /// Run the harness against a goal prompt.
    Run(RunArgs),
    /// Launch the simplified interactive TUI.
    Tui(TuiArgs),
    /// Mock LLM server tooling.
    Mock {
        #[command(subcommand)]
        command: MockCommand,
    },
}

#[derive(Subcommand)]
pub enum MockCommand {
    /// Serve the OpenAI-schema mock over loopback HTTP.
    Serve(ServeArgs),
}

#[derive(Args)]
pub struct RunArgs {
    /// The goal for the team to accomplish.
    pub goal: String,

    /// Team-agent pool size (created at run start, never destroyed).
    #[arg(long, default_value_t = 4, value_name = "N")]
    pub agents: usize,

    /// Max concurrently-active team agents (default = --agents; must be <= --agents).
    #[arg(long, value_name = "N")]
    pub parallel: Option<usize>,

    /// Number of meta-agents (0 disables the meta layer; metrics still emitted).
    #[arg(long, default_value_t = 1, value_name = "N")]
    pub meta_agents: usize,

    /// Seed for run determinism (default: random per run, logged and printed).
    #[arg(long, value_name = "U64")]
    pub seed: Option<u64>,

    /// Cap: max orchestrator ticks before forced termination.
    #[arg(long, value_name = "N")]
    pub max_ticks: Option<u64>,

    /// Cap: max LLM completions before forced termination.
    #[arg(long, value_name = "N")]
    pub max_llm_calls: Option<u64>,

    /// Cap: max wall-clock seconds before forced termination.
    #[arg(long, value_name = "SECS")]
    pub max_duration: Option<u64>,

    /// Per-turn tool-call iteration cap.
    #[arg(long, default_value_t = 8, value_name = "N")]
    pub max_tool_iters: u32,

    /// Run against the built-in offline mock instead of a real endpoint
    /// (ADR 0026): deterministic, seedable, no external network (loopback only).
    /// Used by the test suite and for offline local runs. Conflicts with
    /// --llm-base-url.
    #[arg(long, conflicts_with = "llm_base_url")]
    pub mock: bool,

    /// External OpenAI-compatible endpoint, overriding the default
    /// (https://api.openai.com/v1). Conflicts with --mock (ADR 0026). Taken as
    /// the full API base *including the path prefix* (a trailing slash is added
    /// if absent): `https://host/v1/` for an OpenAI-schema server, or
    /// `https://host/api/` for Open WebUI (whose chat route is
    /// `/api/chat/completions`). Relative endpoints resolve against it.
    #[arg(long, value_name = "URL")]
    pub llm_base_url: Option<Url>,

    /// Bearer token for the endpoint; falls back to OPENAI_API_KEY. Prefer an
    /// env var — a CLI-passed key can leak via shell history / `ps`.
    #[arg(
        long,
        env = "OPENTEAM_LLM_API_KEY",
        value_name = "KEY",
        hide_env_values = true
    )]
    pub llm_api_key: Option<String>,

    /// Chat-completion model id (default: gpt-4o-mini; openteam-mock under --mock).
    #[arg(long, env = "OPENTEAM_MODEL", value_name = "ID")]
    pub model: Option<String>,

    /// Embedding model id (default: text-embedding-3-small; openteam-mock under --mock).
    #[arg(long, env = "OPENTEAM_EMBEDDING_MODEL", value_name = "ID")]
    pub embedding_model: Option<String>,

    /// Embed locally by feature hashing instead of calling the endpoint's
    /// `/embeddings` route — for endpoints without one, e.g. Open WebUI.
    #[arg(long)]
    pub local_embeddings: bool,

    /// Scenario fixture overriding the built-in behavior arc (ADR 0023). Only
    /// the mock consumes scenarios, so this requires --mock.
    #[arg(long, value_name = "FILE", requires = "mock")]
    pub scenario: Option<PathBuf>,

    /// Run-artifacts directory (default: .openteam/runs/<uuidv7>/).
    #[arg(long, value_name = "DIR")]
    pub out_dir: Option<PathBuf>,
}

#[derive(Args)]
pub struct TuiArgs {
    /// Team-agent pool size (created at run start, never destroyed).
    #[arg(long, default_value_t = 4, value_name = "N")]
    pub agents: usize,

    /// Number of meta-agents (0 disables the meta layer; metrics still emitted).
    #[arg(long, default_value_t = 1, value_name = "N")]
    pub meta_agents: usize,

    /// Seed for deterministic mock behavior (default: random per run).
    #[arg(long, value_name = "U64")]
    pub seed: Option<u64>,
}

#[derive(Args)]
pub struct ServeArgs {
    /// Port to bind (0 = OS-assigned ephemeral port, printed on startup).
    #[arg(long, default_value_t = 0, value_name = "PORT")]
    pub port: u16,

    /// Scenario fixture overriding the built-in behavior arc (ADR 0023).
    #[arg(long, value_name = "FILE")]
    pub scenario: Option<PathBuf>,
}
