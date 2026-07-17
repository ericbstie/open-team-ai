//! `openteam` — the composition root (ADR 0024): parse the CLI, start the
//! in-process mock (unless `--llm-base-url`), wire core against it, run the
//! orchestrator, persist artifacts, and print the report.
//!
//! Output contract: stdout is the report (byte-identical to `report.md`),
//! stderr is tracing; `--quiet` silences tracing and never trims stdout.

// The bin owns stdout for the report and stderr for pre-run errors
// (ADR 0013's lint carve-out for the bin).
#![allow(clippy::print_stdout, clippy::print_stderr)]
// Test modules lean on unwrap/expect/panic for terse assertions, matching the
// library crates' pattern.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod cli;
mod tui;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use clap::{CommandFactory, Parser};
use openteam_core::{ReqwestLlmClient, RunConfig, SystemClock};
use openteam_mock::{AppState, Scenario, serve};
use url::Url;

use cli::{Cli, Command, MockCommand, RunArgs, ServeArgs};

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

    // The TUI owns the terminal via an alternate screen; stderr tracing would
    // paint over its rendering, so it runs without a subscriber, as `--quiet`
    // does. The in-transcript activity feed surfaces run progress instead.
    let quiet = cli.quiet || matches!(cli.command, Command::Tui(_));
    init_tracing(cli.verbose, quiet);

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("openteam: failed to start the async runtime: {error}");
            return ExitCode::from(1);
        }
    };
    match cli.command {
        Command::Run(args) => runtime.block_on(run_command(args)),
        // The TUI drives a synchronous crossterm loop on the main thread and
        // spawns harness sessions onto the runtime, so it owns the runtime
        // rather than blocking on a single future.
        Command::Tui(args) => tui::run(runtime, args),
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

    // Start the in-process mock unless an external endpoint is configured
    // (ADR 0001/0019). Real loopback: the client path is byte-identical.
    let (base_url, mock_handle) = match &args.llm_base_url {
        Some(url) => (url.clone(), None),
        None => {
            let state = match scenario {
                Some(scenario) => AppState::with_scenario(scenario),
                None => AppState::builtin(),
            };
            match serve(state, 0).await {
                Ok((addr, handle)) => {
                    tracing::debug!(%addr, "in-process mock bound");
                    // The mock serves the OpenAI-schema routes under `/v1/`;
                    // the base carries that prefix so the client joins
                    // `chat/completions` / `embeddings` relative to it.
                    match Url::parse(&format!("http://{addr}/v1/"))
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
        }
    };

    let transport = Arc::new(ReqwestLlmClient::new(base_url, args.llm_api_key.clone()));
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
        model: args.model.clone(),
        embedding_model: args.embedding_model.clone(),
        local_embeddings: args.local_embeddings,
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
