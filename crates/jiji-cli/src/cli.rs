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
        long = "config",
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
        #[arg(long, help = "Skip jiji-proxy route activation")]
        skip_proxy: bool,
        #[arg(
            short = 'y',
            long,
            help = "Auto-confirm the deployment plan; required when running non-interactively (e.g. CI/CD)"
        )]
        yes: bool,
        #[arg(long, value_name = "SECONDS", default_value_t = 300)]
        lock_timeout: u64,
        #[arg(long, help = "Replace an existing deployment lock")]
        force_lock: bool,
        #[arg(
            long,
            value_name = "N",
            help = "After a successful deploy, best-effort check up to N other peers' catalogs for the new deployment (never blocks past a short bound, never affects the exit code)"
        )]
        wait_for_peers: Option<u32>,
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
    #[command(about = "Jiji-proxy management")]
    Proxy {
        #[command(subcommand)]
        command: ProxyCommands,
    },
    #[command(about = "Service management")]
    Service {
        #[command(subcommand)]
        command: ServiceCommands,
    },
    #[command(about = "Secrets management")]
    Secrets {
        #[command(subcommand)]
        command: SecretsCommands,
    },
    #[command(about = "Deployment lock management")]
    Lock {
        #[command(subcommand)]
        command: LockCommands,
    },
    #[command(about = "Show the deployment audit trail (host-scoped; -S/--services is rejected)")]
    Audit {
        #[arg(
            short = 'n',
            long,
            value_name = "N",
            default_value_t = 20,
            conflicts_with = "stats",
            help = "Number of entries to show per server"
        )]
        lines: u32,
        #[arg(
            short = 'g',
            long,
            value_name = "PATTERN",
            help = "Filter entries by action or message"
        )]
        grep: Option<String>,
        #[arg(
            long,
            value_name = "STATUS",
            help = "Filter by status: success or failed"
        )]
        status: Option<String>,
        #[arg(
            long,
            help = "Output newline-delimited entries, or one structured stats object, as JSON"
        )]
        json: bool,
        #[arg(long, help = "Show aggregate success-rate and duration statistics")]
        stats: bool,
        #[arg(
            short = 's',
            long,
            value_name = "WINDOW",
            requires = "stats",
            help = "Only include entries from this relative window (for example 30m, 12h, or 7d)"
        )]
        since: Option<String>,
        #[arg(
            short = 'f',
            long,
            help = "Follow the audit trail as new entries are appended (requires exactly one host)"
        )]
        follow: bool,
    },
}

#[derive(Subcommand)]
pub enum LockCommands {
    #[command(about = "Acquire a deployment lock to prevent concurrent deployments")]
    Acquire {
        #[arg(help = "Lock message describing why the lock was acquired")]
        message: String,
        #[arg(
            long,
            value_name = "SECONDS",
            default_value_t = 300,
            help = "Wait up to this many seconds for an existing lock to be released before giving up"
        )]
        timeout: u64,
        #[arg(long, help = "Force acquire even if already locked (use with caution)")]
        force: bool,
    },
    #[command(
        about = "Release a lock (defaults to the project-maintenance lock; use --replica/--service/--scope to target a finer-grained one)"
    )]
    Release {
        #[arg(
            long,
            value_name = "REPLICA_ID",
            conflicts_with_all = ["service", "scope"],
            help = "Release the logical-replica lock for this replica ID instead"
        )]
        replica: Option<String>,
        #[arg(
            long,
            value_name = "SERVICE",
            conflicts_with_all = ["replica", "scope"],
            help = "Release the service-scale lock for this service instead"
        )]
        service: Option<String>,
        #[arg(
            long,
            value_name = "host-runtime|proxy",
            conflicts_with_all = ["replica", "service"],
            help = "Release the named host-scoped lock instead (host-runtime or proxy)"
        )]
        scope: Option<String>,
    },
    #[command(about = "Check current lock status")]
    Status {
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    #[command(about = "Show detailed lock information")]
    Show,
}

#[derive(Subcommand)]
pub enum ProxyCommands {
    #[command(about = "Pull and recreate jiji-proxy on selected servers")]
    Restart,
    #[command(about = "View jiji-proxy logs on selected servers")]
    Logs {
        #[arg(short = 'n', long, value_name = "N", help = "Number of lines to show")]
        lines: Option<u32>,
        #[arg(
            short = 's',
            long,
            value_name = "TIMESTAMP",
            help = "Show logs since this timestamp or relative duration"
        )]
        since: Option<String>,
        #[arg(short = 'g', long, value_name = "PATTERN", help = "Filter log lines")]
        grep: Option<String>,
        #[arg(short = 'f', long, help = "Follow logs (requires exactly one host)")]
        follow: bool,
    },
}

#[derive(Subcommand)]
pub enum ServiceCommands {
    #[command(about = "View service logs")]
    Logs {
        #[arg(short = 'n', long, value_name = "N", help = "Number of lines to show")]
        lines: Option<u32>,
        #[arg(
            short = 's',
            long,
            value_name = "TIMESTAMP",
            help = "Show logs since this timestamp or relative duration"
        )]
        since: Option<String>,
        #[arg(short = 'g', long, value_name = "PATTERN", help = "Filter log lines")]
        grep: Option<String>,
        #[arg(
            long,
            value_name = "OPTIONS",
            help = "Extra flags passed to grep (e.g. -i for case-insensitive)"
        )]
        grep_options: Option<String>,
        #[arg(short = 'f', long, help = "Follow logs (requires exactly one target)")]
        follow: bool,
        #[arg(
            long,
            value_name = "ID",
            help = "Show logs for an arbitrary container name instead of a configured service"
        )]
        container_id: Option<String>,
    },
    #[command(about = "Restart running services with a zero-downtime replacement")]
    Restart {
        #[arg(long, value_name = "SECONDS", default_value_t = 300)]
        lock_timeout: u64,
        #[arg(long, help = "Replace an existing deployment lock")]
        force_lock: bool,
    },
    #[command(
        about = "Roll back services to a previously built image via a zero-downtime replacement (requires --version)"
    )]
    Rollback {
        #[arg(long, value_name = "SECONDS", default_value_t = 300)]
        lock_timeout: u64,
        #[arg(long, help = "Replace an existing deployment lock")]
        force_lock: bool,
    },
    #[command(about = "Change the desired replica count for one service")]
    Scale {
        #[arg(long, value_name = "N", conflicts_with = "reset")]
        replicas: Option<u32>,
        #[arg(
            long,
            conflicts_with = "replicas",
            help = "Reset to the configured replica count"
        )]
        reset: bool,
        #[arg(long, help = "Print the scale plan without changing state")]
        dry_run: bool,
        #[arg(short = 'y', long, help = "Skip the confirmation prompt")]
        yes: bool,
    },
    #[command(about = "Remove services from servers")]
    Remove {
        #[arg(short = 'y', long, help = "Skip the destructive confirmation prompt")]
        yes: bool,
        #[arg(
            long,
            help = "Also remove jiji-owned named volumes for selected services"
        )]
        volumes: bool,
        #[arg(long, value_name = "SECONDS", default_value_t = 300)]
        lock_timeout: u64,
        #[arg(long, help = "Replace an existing lock on an affected replica")]
        force_lock: bool,
    },
    #[command(about = "Clean up old container images")]
    Prune {
        #[arg(
            short = 'r',
            long,
            value_name = "N",
            help = "Number of image versions to keep (default: service's configured `retain`, normally 3)"
        )]
        retain: Option<u32>,
    },
    #[command(about = "Scheduled command management for services")]
    Cron {
        #[command(subcommand)]
        command: ServiceCronCommands,
    },
}

#[derive(Subcommand)]
pub enum ServiceCronCommands {
    #[command(about = "Show configured cron jobs and their installation state")]
    List,
    #[command(about = "Show durable run state from each job's assigned agent")]
    Status,
    #[command(about = "View logs for one cron job's runs (requires -S <service> and a cron name)")]
    Logs {
        #[arg(help = "Cron name (as defined under the service's 'crons' map)")]
        cron: String,
        #[arg(
            long,
            value_name = "ID",
            help = "Show logs for a specific run instead of the latest"
        )]
        run: Option<String>,
        #[arg(short = 'n', long, value_name = "N", help = "Number of lines to show")]
        lines: Option<u32>,
        #[arg(
            short = 's',
            long,
            value_name = "TIMESTAMP",
            help = "Show logs since this timestamp or relative duration"
        )]
        since: Option<String>,
        #[arg(short = 'f', long, help = "Follow logs (requires one active run)")]
        follow: bool,
    },
    #[command(
        about = "Request an immediate run of one cron job (requires -S <service> and a cron name)"
    )]
    Run {
        #[arg(help = "Cron name (as defined under the service's 'crons' map)")]
        cron: String,
        #[arg(long, help = "Stream output after the agent accepts the run")]
        follow: bool,
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
pub enum SecretsCommands {
    #[command(about = "Print resolved secrets for debugging")]
    Print {
        #[arg(long, help = "Show actual secret values (use with caution)")]
        show_values: bool,
    },
}

#[derive(Subcommand)]
pub enum ServerCommands {
    #[command(about = "Install the container engine and complete private network on each server")]
    Setup {
        #[arg(short = 'y', long, help = "Skip the confirmation prompt")]
        yes: bool,
        #[arg(
            long,
            help = "Force a fresh WireGuard keypair on the targeted hosts and fence out their old identity"
        )]
        rotate_key: bool,
        #[arg(
            long,
            help = "Assess each targeted host and import any pre-existing container as historical (Stopped) catalog history, once its agent is running"
        )]
        import: bool,
        #[arg(
            long,
            help = "With --import, report what would be imported without committing anything"
        )]
        import_dry_run: bool,
    },
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
    #[command(
        about = "Run a command on any number of servers, or attach an interactive shell to exactly one"
    )]
    Exec {
        #[arg(help = "Command to run (quote multi-word commands); omit for an interactive shell")]
        command: Option<String>,
        #[arg(
            long,
            help = "Attach a PTY even when a command is given (requires exactly one matched host)"
        )]
        interactive: bool,
        #[arg(
            long,
            help = "With multiple matched hosts, run one at a time instead of concurrently"
        )]
        sequential: bool,
    },
}

#[derive(Subcommand)]
pub enum NetworkCommands {
    #[command(about = "Install or repair the complete private network")]
    Setup,
    #[command(about = "Print the deterministic private network plan without changing hosts")]
    Plan,
    #[command(about = "Inspect the replicated service catalog on selected hosts")]
    Catalog,
    #[command(about = "Inspect self-healing, replication, quota, and component diagnostics")]
    Diagnostics {
        #[arg(long, help = "Emit one JSON object per server")]
        json: bool,
    },
    #[command(about = "Compact superseded replicated operation history")]
    Compact,
    #[command(about = "Export an encrypted operator-controlled control-plane backup")]
    Backup {
        #[arg(long, value_name = "PATH")]
        output: String,
        #[arg(long, value_name = "PATH")]
        passphrase_file: String,
    },
    #[command(about = "Restore state into surviving hosts in the same recovery epoch")]
    Restore {
        #[arg(long, value_name = "PATH")]
        input: String,
        #[arg(long, value_name = "PATH")]
        passphrase_file: String,
    },
    #[command(about = "Recover a lost control plane into a new fenced recovery epoch")]
    Recover {
        #[arg(long, value_name = "PATH")]
        input: String,
        #[arg(long, value_name = "PATH")]
        passphrase_file: String,
        #[arg(short = 'y', long, help = "Confirm destructive epoch advancement")]
        yes: bool,
    },
}
