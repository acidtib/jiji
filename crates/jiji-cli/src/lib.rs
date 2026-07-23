mod cli;
mod commands;
mod container_ops;
mod container_runtime;
mod deploy_transaction;
mod engine;
mod env_resolution;
mod health_check;
mod mounts;
mod network_guard;
mod proxy;
mod proxy_routes;
pub mod service_network;
mod ssh_adapter;

use clap::{CommandFactory, Parser};
use cli::{Cli, Commands, NetworkCommands, ServerCommands};
use tracing_subscriber::EnvFilter;

/// Shared entrypoint for both the `jiji` and `jiji_dev` binaries (see `src/main.rs` and
/// `src/bin/jiji_dev.rs`) so a local test build never has to overwrite the installed `jiji`.
pub async fn run() {
    let cli = Cli::parse();

    let filter = if cli.verbose {
        "debug"
    } else if cli.quiet {
        "warn"
    } else {
        "info"
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .without_time()
        .try_init();

    match &cli.command {
        Some(Commands::Init) => {
            if let Err(err) = commands::init::run(cli.environment.as_deref()) {
                println!();
                jiji_tui::Ui::error(&format!("Initialization failed: {err}"));
                if err.downcast_ref::<jiji_config::ConfigError>().is_some() {
                    jiji_tui::Ui::say(
                        "Configuration validation failed. Please check the template or try again.",
                        1,
                    );
                } else {
                    jiji_tui::Ui::say("Please check the error above and try again", 1);
                }
                std::process::exit(1);
            }
        }
        Some(Commands::Version) => {
            commands::version::run();
        }
        Some(Commands::Deploy { build, skip_proxy }) => {
            if let Err(err) = commands::deploy::run(
                cli.environment.as_deref(),
                cli.config_file.as_deref(),
                cli.hosts.as_deref(),
                cli.services.as_deref(),
                cli.version_arg.as_deref(),
                *build,
                *skip_proxy,
                cli.host_env,
            )
            .await
            {
                println!();
                jiji_tui::Ui::error(&format!("Deploy failed: {err}"));
                if err.downcast_ref::<jiji_config::ConfigError>().is_some() {
                    jiji_tui::Ui::say(
                        "Configuration validation failed. Please check your deploy config and try again.",
                        1,
                    );
                } else {
                    jiji_tui::Ui::say("Please check the error above and try again", 1);
                }
                std::process::exit(1);
            }
        }
        Some(Commands::Server { command }) => match command {
            ServerCommands::Setup => {
                if let Err(err) = commands::server::setup::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                    cli.hosts.as_deref(),
                )
                .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Server setup failed: {err}"));
                    if err.downcast_ref::<jiji_config::ConfigError>().is_some() {
                        jiji_tui::Ui::say(
                            "Configuration validation failed. Please check your deploy config and try again.",
                            1,
                        );
                    } else {
                        jiji_tui::Ui::say("Please check the error above and try again", 1);
                    }
                    std::process::exit(1);
                }
            }
        },
        Some(Commands::Network { command }) => match command {
            NetworkCommands::Setup => {
                if let Err(err) = commands::network::setup::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                    cli.hosts.as_deref(),
                )
                .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Network setup failed: {err}"));
                    jiji_tui::Ui::say("Fix the reported host or configuration error and retry", 1);
                    std::process::exit(1);
                }
            }
            NetworkCommands::Plan => {
                if let Err(err) = commands::network::plan::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                ) {
                    jiji_tui::Ui::error(&format!("Network planning failed: {err}"));
                    std::process::exit(1);
                }
            }
        },
        None => {
            Cli::command().print_help().ok();
            println!();
            std::process::exit(0);
        }
    }
}
