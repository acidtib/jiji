use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "jiji",
    about = "Jiji - Infrastructure management tool",
    disable_version_flag = true
)]
pub struct Cli {
    #[arg(short, long, global = true, help = "Detailed logging")]
    pub verbose: bool,

    #[arg(
        short,
        long,
        global = true,
        help = "Minimal output (suppress host headers and extra messages)"
    )]
    pub quiet: bool,

    #[arg(
        long = "version",
        value_name = "VERSION",
        global = true,
        help = "Run commands against a specific app version"
    )]
    pub version_arg: Option<String>,

    #[arg(
        short = 'c',
        long = "config-file",
        value_name = "CONFIG_FILE",
        global = true,
        help = "Path to config file"
    )]
    pub config_file: Option<String>,

    #[arg(
        short = 'e',
        long,
        value_name = "ENVIRONMENT",
        global = true,
        help = "Specify environment to be used for config file (staging -> jiji.staging.yml)"
    )]
    pub environment: Option<String>,

    #[arg(
        short = 'H',
        long,
        value_name = "HOSTS",
        global = true,
        help = "Run commands on these hosts instead of all (separate by comma, supports wildcards with *)"
    )]
    pub hosts: Option<String>,

    #[arg(
        short = 'S',
        long,
        value_name = "SERVICES",
        global = true,
        help = "Run commands on these services instead of all (separate by comma, supports wildcards with *)"
    )]
    pub services: Option<String>,

    #[arg(
        long = "host-env",
        global = true,
        help = "Fallback to host environment variables when secrets are not found in .env files"
    )]
    pub host_env: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    #[command(about = "Create config stub in .jiji/deploy.yml")]
    Init,
    #[command(about = "Show jiji version")]
    Version,
    #[command(about = "Server management")]
    Server {
        #[command(subcommand)]
        command: ServerCommands,
    },
}

#[derive(Subcommand)]
pub enum ServerCommands {
    #[command(about = "Check for and install the configured container engine on each server")]
    Setup,
}
