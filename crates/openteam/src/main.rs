//! `openteam` — the composition root (ADR 0024): parse the CLI, start the
//! in-process mock (unless `--llm-base-url`), wire core against it, run the
//! orchestrator, persist artifacts, and print the report.
//!
//! Output contract: stdout is the report (byte-identical to `report.md`),
//! stderr is tracing; `--quiet` silences tracing and never trims stdout.

// The bin owns stdout for the report and stderr for pre-run errors
// (ADR 0013's lint carve-out for the bin).
#![allow(clippy::print_stdout, clippy::print_stderr)]

mod cli;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use clap::{CommandFactory, Parser};
use openteam_core::{ReqwestLlmClient, RunConfig, SystemClock};
use openteam_mock::{AppState, Scenario, serve};
use url::Url;

use cli::{Cli, Command, MockCommand, RunArgs, ServeArgs};

/// The default real endpoint used when neither `--mock` nor `--llm-base-url`
/// is given (ADR 0026). The reqwest adapter joins the absolute `/v1/...`
/// paths, so a host-with-`/v1` base resolves correctly.
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
/// Default chat model on the real path (override with `--model` / `OPENTEAM_MODEL`).
const DEFAULT_MODEL: &str = "gpt-4o-mini";
/// Default embedding model on the real path (override with `--embedding-model`).
const DEFAULT_EMBEDDING_MODEL: &str = "text-embedding-3-small";
/// Under `--mock` the model string is cosmetic (the mock echoes any non-empty
/// value); keep the pinned `openteam-mock` so mock runs read as before.
const MOCK_MODEL: &str = "openteam-mock";

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Post-parse cross-field validation (ADR 0015/0024): clap-derive can't
    // express it, so report through clap for the uniform usage-error exit 2
    // — no run starts, no artifacts.
    if let Command::Run(args) = &cli.command
        && let Some(parallel) = args.parallel
        && parallel > args.agents
    {
        Cli::command()
            .error(
                clap::error::ErrorKind::ValueValidation,
                format!(
                    "--parallel {parallel} exceeds --agents {} (the pool size caps concurrency)",
                    args.agents
                ),
            )
            .exit();
    }

    init_tracing(cli.verbose, cli.quiet);

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("openteam: failed to start the async runtime: {error}");
            return ExitCode::from(1);
        }
    };
    match cli.command {
        Command::Run(args) => runtime.block_on(run_command(args)),
        Command::Mock {
            command: MockCommand::Serve(args),
        } => runtime.block_on(serve_command(args)),
    }
}

/// One tracing dial for both subcommands (ADR 0024): stderr only, default
/// INFO, `-v` DEBUG, `-vv` TRACE, `--quiet` no subscriber at all.
fn init_tracing(verbose: u8, quiet: bool) {
    if quiet {
        return;
    }
    let level = match verbose {
        0 => tracing::Level::INFO,
        1 => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    };
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .init();
}

/// Load and structurally validate a scenario fixture (ADR 0023): any
/// violation aborts before the run starts — a validation-phase exit 2 with
/// no artifacts, like the clap errors.
fn load_scenario(path: &std::path::Path) -> Result<Scenario, ExitCode> {
    match Scenario::from_path(path) {
        Ok(scenario) => Ok(scenario),
        Err(error) => {
            eprintln!("openteam: invalid scenario {}: {error}", path.display());
            Err(ExitCode::from(2))
        }
    }
}

async fn run_command(args: RunArgs) -> ExitCode {
    // The seed default is random-per-run, resolved here in the bin — the
    // deterministic core only ever receives a resolved u64 (ADR 0024).
    let seed = args.seed.unwrap_or_else(rand::random);
    tracing::info!("run seed: {seed}");

    let scenario = match &args.scenario {
        Some(path) => match load_scenario(path) {
            Ok(scenario) => Some(scenario),
            Err(code) => return code,
        },
        None => None,
    };

    // Resolve the transport target (ADR 0026): a real OpenAI-compatible
    // endpoint by default, the built-in offline mock only under `--mock`.
    // Real loopback keeps the reqwest client path byte-identical either way
    // (ADR 0019).
    let (base_url, mock_handle) = if args.mock {
        let state = match scenario {
            Some(scenario) => AppState::with_scenario(scenario),
            None => AppState::builtin(),
        };
        match serve(state, 0).await {
            Ok((addr, handle)) => {
                tracing::debug!(%addr, "in-process mock bound");
                match Url::parse(&format!("http://{addr}"))
                    .context("mock address did not form a URL")
                {
                    Ok(url) => (url, Some(handle)),
                    Err(error) => {
                        eprintln!("openteam: {error:#}");
                        return ExitCode::from(1);
                    }
                }
            }
            Err(error) => {
                eprintln!("openteam: failed to start the in-process mock: {error}");
                return ExitCode::from(1);
            }
        }
    } else {
        let url = match &args.llm_base_url {
            Some(url) => url.clone(),
            None => match Url::parse(DEFAULT_OPENAI_BASE_URL) {
                Ok(url) => url,
                Err(error) => {
                    eprintln!("openteam: bad default endpoint url: {error}");
                    return ExitCode::from(1);
                }
            },
        };
        (url, None)
    };

    // Bearer token: `--llm-api-key`/`OPENTEAM_LLM_API_KEY` (folded by clap),
    // falling back to the conventional `OPENAI_API_KEY` (ADR 0026).
    let api_key = args
        .llm_api_key
        .clone()
        .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        .filter(|key| !key.is_empty());

    // Fail fast (validation-phase exit 2, no artifacts) when hitting the
    // default OpenAI endpoint without a key — a plain 401 mid-run is a worse
    // first-run experience. A custom `--llm-base-url` may need no key (local
    // servers), so only guard the default.
    if !args.mock && args.llm_base_url.is_none() && api_key.is_none() {
        eprintln!(
            "openteam: no API key for the default OpenAI endpoint. Set OPENAI_API_KEY \
             (or OPENTEAM_LLM_API_KEY / --llm-api-key), point --llm-base-url at a \
             compatible endpoint, or pass --mock to use the offline mock."
        );
        return ExitCode::from(2);
    }

    // Model ids: real defaults on the network path, the pinned `openteam-mock`
    // echo string under `--mock` (both overridable via the flags/env vars).
    let (default_model, default_embedding) = if args.mock {
        (MOCK_MODEL, MOCK_MODEL)
    } else {
        (DEFAULT_MODEL, DEFAULT_EMBEDDING_MODEL)
    };
    let model = args
        .model
        .clone()
        .unwrap_or_else(|| default_model.to_string());
    let embedding_model = args
        .embedding_model
        .clone()
        .unwrap_or_else(|| default_embedding.to_string());

    let transport = Arc::new(ReqwestLlmClient::new(base_url, api_key));
    let config = RunConfig {
        goal: args.goal.clone(),
        agents: args.agents,
        meta_agents: args.meta_agents,
        parallel: args.parallel.unwrap_or(args.agents),
        seed,
        max_ticks: args.max_ticks,
        max_llm_calls: args.max_llm_calls,
        max_duration: args.max_duration.map(Duration::from_secs),
        max_tool_iters: args.max_tool_iters,
        model,
        embedding_model,
        out_dir: args.out_dir.clone(),
        scenario: args.scenario.as_ref().map(|p| p.display().to_string()),
        // Test-only knob (pins §6): not a CLI flag — ADR 0024's surface is
        // closed.
        assembly_budget: std::env::var("OPENTEAM_ASSEMBLY_BUDGET")
            .ok()
            .and_then(|v| v.parse().ok()),
    };

    let outcome = openteam_core::run(config, transport, Arc::new(SystemClock)).await;

    if let Some(handle) = mock_handle {
        handle.shutdown().await;
    }

    match outcome {
        Ok(outcome) => {
            // stdout == report.md, byte-identical (ADR 0022/0024).
            print!("{}", outcome.report);
            ExitCode::from(outcome.exit_code)
        }
        Err(error) => {
            eprintln!("openteam: harness error: {error}");
            ExitCode::from(1)
        }
    }
}

async fn serve_command(args: ServeArgs) -> ExitCode {
    let scenario = match &args.scenario {
        Some(path) => match load_scenario(path) {
            Ok(scenario) => Some(scenario),
            Err(code) => return code,
        },
        None => None,
    };
    let state = match scenario {
        Some(scenario) => AppState::with_scenario(scenario),
        None => AppState::builtin(),
    };
    let (addr, handle) = match serve(state, args.port).await {
        Ok(bound) => bound,
        Err(error) => {
            eprintln!("openteam: failed to bind the mock server: {error}");
            return ExitCode::from(1);
        }
    };
    println!("openteam mock listening on http://{addr}");
    tracing::info!(%addr, "mock serving; Ctrl-C to stop");

    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("openteam: failed to wait for Ctrl-C: {error}");
        return ExitCode::from(1);
    }
    handle.shutdown().await;
    ExitCode::SUCCESS
}
