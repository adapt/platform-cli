mod api;
mod auth;
mod backend;
mod cli;
mod commands;
mod config;
mod input;
mod local_runtime;
mod mcp;
mod output;
mod runtime;
mod shell;
mod skill;
mod stream;

use clap::{CommandFactory, Parser};
use cli::Cli;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Clap checks conflicts within each command before propagating global
    // arguments; also catch flags placed at different command levels.
    if cli.local_target && cli.cloud_target {
        Cli::command()
            .error(
                clap::error::ErrorKind::ArgumentConflict,
                "--local cannot be used with --cloud",
            )
            .exit();
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("Cannot start async runtime");
    let code = runtime.block_on(run(cli));
    // A terminal read can remain blocked after a remote command finishes.
    // It must not prevent the CLI from exiting.
    runtime.shutdown_background();
    code
}

async fn run(cli: Cli) -> ExitCode {
    let result = if matches!(
        &cli.command,
        cli::Command::Sandbox(cli::SandboxCommand::Sh(_))
    ) {
        // This command owns its sandbox and must finish cleanup before returning.
        commands::run(cli).await
    } else {
        // Stopping an endpoint request leaves no remote work behind.
        let remote_work = !matches!(
            &cli.command,
            cli::Command::Endpoints(cli::EndpointCommand::Request(_))
        );
        let local = cli.local_target;
        tokio::select! {
            result = commands::run(cli) => result,
            _ = tokio::signal::ctrl_c() => {
                if remote_work {
                    if local {
                        eprintln!("\nCancelled. Local operations may continue; inspect with `adapt --local sandbox list` and use `adapt --local sandbox stop ID` to stop a VM.");
                    } else {
                        eprintln!("\nCancelled. Remote executions continue; use `adapt execs cancel ID` to stop one.");
                    }
                }
                return ExitCode::from(130);
            }
        }
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            if error.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
            }) {
                return ExitCode::SUCCESS;
            }
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
