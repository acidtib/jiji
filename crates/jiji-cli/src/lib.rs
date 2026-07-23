mod cli;
mod commands;
mod engine;
mod ssh_adapter;

use clap::{CommandFactory, Parser};
use cli::{Cli, Commands, ServerCommands};
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
        None => {
            Cli::command().print_help().ok();
            println!();
            std::process::exit(0);
        }
    }
}
