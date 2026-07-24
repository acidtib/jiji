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
        help = "Run commands against a specific app version (e.g. `jiji deploy --version 1.2.3`)"
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
    #[command(about = "Deploy configured services across their target servers")]
    Deploy {
        #[arg(long, help = "Build images before deploying")]
        build: bool,
        #[arg(
            long,
            help = "Build without using the cache (only relevant with --build)"
        )]
        no_cache: bool,
        #[arg(long, help = "Skip kamal-proxy route activation")]
        skip_proxy: bool,
    },
    #[command(about = "Build and push images for services with `build:` configured")]
    Build {
        #[arg(long, help = "Build without using the cache")]
        no_cache: bool,
        #[arg(long, overrides_with = "no_push", help = "Push built images (default)")]
        push: bool,
        #[arg(
            long,
            overrides_with = "push",
            help = "Build without pushing (single-architecture only)"
        )]
        no_push: bool,
    },
    #[command(about = "Server management")]
    Server {
        #[command(subcommand)]
        command: ServerCommands,
    },
    #[command(about = "Private network management")]
    Network {
        #[command(subcommand)]
        command: NetworkCommands,
    },
    #[command(about = "Registry management")]
    Registry {
        #[command(subcommand)]
        command: RegistryCommands,
    },
}

#[derive(Subcommand)]
pub enum RegistryCommands {
    #[command(
        about = "Authenticate the local machine and/or configured servers to the configured registry"
    )]
    Login {
        #[arg(long, help = "Do not authenticate the local development machine")]
        skip_local: bool,
        #[arg(long, help = "Do not authenticate any configured server")]
        skip_remote: bool,
    },
    #[command(
        about = "Remove registry credentials from the local machine and/or configured servers"
    )]
    Logout {
        #[arg(long, help = "Do not log out the local development machine")]
        skip_local: bool,
        #[arg(long, help = "Do not log out any configured server")]
        skip_remote: bool,
    },
    #[command(about = "Remove the Jiji-managed local registry container")]
    Teardown {
        #[arg(short = 'y', long, help = "Skip the destructive confirmation prompt")]
        yes: bool,
        #[arg(long, help = "Show what would be removed without changing anything")]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
pub enum ServerCommands {
    #[command(about = "Install the container engine and complete private network on each server")]
    Setup,
    #[command(
        about = "Remove jiji-managed applications and the private network from selected servers"
    )]
    Teardown {
        #[arg(short = 'y', long, help = "Skip the destructive confirmation prompt")]
        yes: bool,
        #[arg(long, help = "Also remove jiji-owned named volumes for this project")]
        volumes: bool,
        #[arg(long, help = "Print the teardown plan without changing any host")]
        dry_run: bool,
    },
    #[command(about = "Run a command, or an interactive shell, on exactly one server")]
    Exec {
        #[arg(help = "Command to run (quote multi-word commands); omit for an interactive shell")]
        command: Option<String>,
        #[arg(long, help = "Attach a PTY even when a command is given")]
        interactive: bool,
    },
}

#[derive(Subcommand)]
pub enum NetworkCommands {
    #[command(about = "Install or repair the complete private network")]
    Setup,
    #[command(about = "Print the deterministic private network plan without changing hosts")]
    Plan,
}
