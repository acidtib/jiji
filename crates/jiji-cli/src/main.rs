mod cli;
mod commands;

use clap::{CommandFactory, Parser};
use cli::{Cli, Commands};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
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
        None => {
            Cli::command().print_help().ok();
            println!();
            std::process::exit(0);
        }
    }
}
