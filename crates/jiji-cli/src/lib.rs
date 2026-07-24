mod audit;
mod build_engine;
mod build_plan;
mod cli;
mod commands;
mod container_ops;
mod container_runtime;
mod deploy_transaction;
mod engine;
mod env_resolution;
mod health_check;
mod image_teardown;
mod local_exec;
mod lock;
mod mounts;
mod network_guard;
mod network_teardown;
mod proxy;
mod proxy_routes;
mod proxy_teardown;
mod registry;
pub mod service_network;
mod ssh_adapter;
mod teardown_plan;
mod version_tag;
mod volume_teardown;

use clap::{CommandFactory, Parser};
use cli::{
    Cli, Commands, LockCommands, NetworkCommands, ProxyCommands, RegistryCommands, SecretsCommands,
    ServerCommands, ServiceCommands,
};
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
        Some(Commands::Deploy {
            build,
            no_cache,
            skip_proxy,
        }) => {
            if let Err(err) = commands::deploy::run(
                cli.environment.as_deref(),
                cli.config_file.as_deref(),
                cli.hosts.as_deref(),
                cli.services.as_deref(),
                cli.version_arg.as_deref(),
                *build,
                *no_cache,
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
        Some(Commands::Build {
            no_cache,
            push: _,
            no_push,
        }) => {
            if let Err(err) = commands::build::run(
                cli.environment.as_deref(),
                cli.config_file.as_deref(),
                cli.services.as_deref(),
                cli.version_arg.as_deref(),
                *no_cache,
                *no_push,
                cli.host_env,
            )
            .await
            {
                println!();
                jiji_tui::Ui::error(&format!("Build failed: {err}"));
                jiji_tui::Ui::say("Please check the error above and try again", 1);
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
            ServerCommands::Teardown {
                yes,
                volumes,
                dry_run,
            } => {
                if let Err(err) = commands::server::teardown::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                    cli.hosts.as_deref(),
                    cli.services.as_deref(),
                    *yes,
                    *volumes,
                    *dry_run,
                )
                .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Server teardown failed: {err}"));
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
            ServerCommands::Exec {
                command,
                interactive,
            } => {
                if let Err(err) = commands::server::exec::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                    cli.hosts.as_deref(),
                    cli.services.as_deref(),
                    command.as_deref(),
                    *interactive,
                )
                .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Server exec failed: {err}"));
                    jiji_tui::Ui::say("Fix the reported host or connection error and retry", 1);
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
        Some(Commands::Registry { command }) => match command {
            RegistryCommands::Login {
                skip_local,
                skip_remote,
            } => {
                if let Err(err) = commands::registry::login::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                    cli.hosts.as_deref(),
                    cli.services.as_deref(),
                    cli.host_env,
                    *skip_local,
                    *skip_remote,
                )
                .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Registry login failed: {err}"));
                    jiji_tui::Ui::say("Fix the reported registry or connection error and retry", 1);
                    std::process::exit(1);
                }
            }
            RegistryCommands::Logout {
                skip_local,
                skip_remote,
            } => {
                if let Err(err) = commands::registry::logout::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                    cli.hosts.as_deref(),
                    cli.services.as_deref(),
                    *skip_local,
                    *skip_remote,
                )
                .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Registry logout failed: {err}"));
                    jiji_tui::Ui::say("Fix the reported registry or connection error and retry", 1);
                    std::process::exit(1);
                }
            }
            RegistryCommands::Teardown { yes, dry_run } => {
                if let Err(err) = commands::registry::teardown::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                    *yes,
                    *dry_run,
                )
                .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Registry teardown failed: {err}"));
                    jiji_tui::Ui::say("Fix the reported local registry error and retry", 1);
                    std::process::exit(1);
                }
            }
        },
        Some(Commands::Proxy { command }) => match command {
            ProxyCommands::Restart => {
                if let Err(err) = commands::proxy::restart::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                    cli.hosts.as_deref(),
                    cli.services.as_deref(),
                )
                .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Proxy restart failed: {err}"));
                    jiji_tui::Ui::say("Fix the reported host or connection error and retry", 1);
                    std::process::exit(1);
                }
            }
            ProxyCommands::Logs {
                lines,
                since,
                grep,
                follow,
            } => {
                if let Err(err) = commands::proxy::logs::run(commands::proxy::logs::LogsOptions {
                    environment: cli.environment.as_deref(),
                    config_file: cli.config_file.as_deref(),
                    hosts: cli.hosts.as_deref(),
                    services: cli.services.as_deref(),
                    lines: *lines,
                    since: since.as_deref(),
                    grep: grep.as_deref(),
                    follow: *follow,
                })
                .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Proxy logs failed: {err}"));
                    jiji_tui::Ui::say("Fix the reported host or connection error and retry", 1);
                    std::process::exit(1);
                }
            }
        },
        Some(Commands::Service { command }) => match command {
            ServiceCommands::Logs {
                lines,
                since,
                grep,
                grep_options,
                follow,
                container_id,
            } => {
                if let Err(err) =
                    commands::service::logs::run(commands::service::logs::LogsOptions {
                        environment: cli.environment.as_deref(),
                        config_file: cli.config_file.as_deref(),
                        hosts: cli.hosts.as_deref(),
                        services: cli.services.as_deref(),
                        lines: *lines,
                        since: since.as_deref(),
                        grep: grep.as_deref(),
                        grep_options: grep_options.as_deref(),
                        follow: *follow,
                        container_id: container_id.as_deref(),
                    })
                    .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Service logs failed: {err}"));
                    jiji_tui::Ui::say("Fix the reported host or connection error and retry", 1);
                    std::process::exit(1);
                }
            }
            ServiceCommands::Restart => {
                if let Err(err) = commands::service::restart::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                    cli.hosts.as_deref(),
                    cli.services.as_deref(),
                    cli.host_env,
                )
                .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Service restart failed: {err}"));
                    jiji_tui::Ui::say("Fix the reported host or connection error and retry", 1);
                    std::process::exit(1);
                }
            }
            ServiceCommands::Rollback => {
                if let Err(err) = commands::service::rollback::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                    cli.hosts.as_deref(),
                    cli.services.as_deref(),
                    cli.version_arg.as_deref(),
                    cli.host_env,
                )
                .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Service rollback failed: {err}"));
                    jiji_tui::Ui::say("Fix the reported host or connection error and retry", 1);
                    std::process::exit(1);
                }
            }
            ServiceCommands::Remove { yes, volumes } => {
                if let Err(err) = commands::service::remove::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                    cli.hosts.as_deref(),
                    cli.services.as_deref(),
                    *yes,
                    *volumes,
                )
                .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Service remove failed: {err}"));
                    jiji_tui::Ui::say("Fix the reported host or connection error and retry", 1);
                    std::process::exit(1);
                }
            }
            ServiceCommands::Prune { retain } => {
                if let Err(err) = commands::service::prune::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                    cli.hosts.as_deref(),
                    cli.services.as_deref(),
                    *retain,
                )
                .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Service prune failed: {err}"));
                    jiji_tui::Ui::say("Fix the reported host or connection error and retry", 1);
                    std::process::exit(1);
                }
            }
        },
        Some(Commands::Secrets { command }) => match command {
            SecretsCommands::Print { show_values } => {
                if let Err(err) = commands::secrets::print::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                    cli.services.as_deref(),
                    cli.host_env,
                    *show_values,
                ) {
                    println!();
                    jiji_tui::Ui::error(&format!("Secrets print failed: {err}"));
                    jiji_tui::Ui::say("Fix the configuration error above and retry", 1);
                    std::process::exit(1);
                }
            }
        },
        Some(Commands::Lock { command }) => match command {
            LockCommands::Acquire {
                message,
                timeout,
                force,
            } => {
                if let Err(err) = commands::lock::acquire::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                    cli.hosts.as_deref(),
                    cli.services.as_deref(),
                    message,
                    *timeout,
                    *force,
                )
                .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Lock acquire failed: {err}"));
                    std::process::exit(1);
                }
            }
            LockCommands::Release => {
                if let Err(err) = commands::lock::release::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                    cli.hosts.as_deref(),
                    cli.services.as_deref(),
                )
                .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Lock release failed: {err}"));
                    std::process::exit(1);
                }
            }
            LockCommands::Status { json } => {
                if let Err(err) = commands::lock::status::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                    cli.hosts.as_deref(),
                    cli.services.as_deref(),
                    *json,
                )
                .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Lock status failed: {err}"));
                    std::process::exit(1);
                }
            }
            LockCommands::Show => {
                if let Err(err) = commands::lock::show::run(
                    cli.environment.as_deref(),
                    cli.config_file.as_deref(),
                    cli.hosts.as_deref(),
                    cli.services.as_deref(),
                )
                .await
                {
                    println!();
                    jiji_tui::Ui::error(&format!("Lock show failed: {err}"));
                    std::process::exit(1);
                }
            }
        },
        Some(Commands::Audit {
            lines,
            grep,
            status,
            json,
            follow,
        }) => {
            if let Err(err) = commands::audit::run(
                cli.environment.as_deref(),
                cli.config_file.as_deref(),
                cli.hosts.as_deref(),
                cli.services.as_deref(),
                *lines,
                grep.as_deref(),
                status.as_deref(),
                *json,
                *follow,
            )
            .await
            {
                println!();
                jiji_tui::Ui::error(&format!("Audit failed: {err}"));
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
