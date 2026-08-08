use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::net::Ipv4Addr;
use std::time::Duration;

/// Top-level jiji configuration (`.jiji/deploy.yml`).
///
/// This mirrors the full schema documented in `jiji.yml` so existing configs parse, even though
/// this slice only validates a subset of it (see `crate::validation`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub project: String,
    pub builder: Builder,
    pub servers: HashMap<String, NamedServer>,
    pub services: HashMap<String, Service>,
    #[serde(default)]
    pub ssh: Option<Ssh>,
    #[serde(default)]
    pub network: Option<Network>,
    #[serde(default)]
    pub secrets_path: Option<String>,
    #[serde(default)]
    pub secrets: Option<SecretsAdapter>,
    #[serde(default)]
    pub environment: Option<Environment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerEngine {
    Docker,
    Podman,
}

impl fmt::Display for ContainerEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContainerEngine::Docker => write!(f, "docker"),
            ContainerEngine::Podman => write!(f, "podman"),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Builder {
    pub engine: ContainerEngine,
    #[serde(default = "default_true")]
    pub local: bool,
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default = "default_true")]
    pub cache: bool,
    #[serde(default)]
    pub registry: Registry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RegistryType {
    #[default]
    Local,
    Remote,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Registry {
    #[serde(rename = "type", default)]
    pub kind: RegistryType,
    #[serde(default = "default_registry_port")]
    pub port: u16,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

impl Default for Registry {
    fn default() -> Self {
        Registry {
            kind: RegistryType::Local,
            port: default_registry_port(),
            server: None,
            username: None,
            password: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NamedServer {
    pub host: String,
    #[serde(default)]
    pub arch: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub key_passphrase: Option<String>,
    #[serde(default)]
    pub keys: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    #[default]
    Error,
    Fatal,
}

/// `ssh.config`: `false` (default) / `true` (load default files) / a single path / multiple paths.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SshConfigFiles {
    Enabled(bool),
    Single(String),
    Multiple(Vec<String>),
}

impl Default for SshConfigFiles {
    fn default() -> Self {
        SshConfigFiles::Enabled(false)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Ssh {
    /// Not `String`: kept optional at the schema level so a config missing `ssh.user` still
    /// parses. `crate::validation` reports the clean "missing configuration" error instead of a
    /// raw serde message.
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    #[serde(default)]
    pub key_passphrase: Option<String>,
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout: u32,
    #[serde(default = "default_command_timeout")]
    pub command_timeout: u32,
    #[serde(default)]
    pub options: HashMap<String, String>,
    #[serde(default)]
    pub proxy: Option<String>,
    #[serde(default)]
    pub proxy_command: Option<String>,
    #[serde(default)]
    pub keys: Option<Vec<String>>,
    #[serde(default)]
    pub keys_only: bool,
    #[serde(default = "default_max_concurrent_starts")]
    pub max_concurrent_starts: u32,
    #[serde(default = "default_pool_idle_timeout")]
    pub pool_idle_timeout: u32,
    #[serde(default = "default_dns_retries")]
    pub dns_retries: u32,
    #[serde(default)]
    pub log_level: LogLevel,
    #[serde(default)]
    pub config: SshConfigFiles,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub management_cidr: Option<String>,
    #[serde(default)]
    pub container_cidr: Option<String>,
    /// Forwarded to for any DNS query outside this project's own `.jiji` zone (see
    /// `jiji-agent/src/dns.rs`) -- without this, a jiji-managed service container could never
    /// resolve a normal internet hostname at all, since its `resolv.conf` only ever gets this
    /// project's own agent as its nameserver. Defaults to public resolvers; override to point at
    /// a home router, Pi-hole, or other local resolver instead.
    #[serde(default = "default_dns_forwarders")]
    pub dns_forwarders: Vec<Ipv4Addr>,
}

impl Network {
    pub fn management_cidr(&self) -> &str {
        self.management_cidr
            .as_deref()
            .unwrap_or(jiji_core::DEFAULT_MANAGEMENT_CIDR)
    }

    pub fn container_cidr(&self) -> &str {
        self.container_cidr
            .as_deref()
            .unwrap_or(jiji_core::DEFAULT_CONTAINER_CIDR)
    }
}

/// The default forwarders used both by serde (when `network:` is present but omits
/// `dns_forwarders`) and by callers resolving a fully-absent `network:` section (`Config.network`
/// is itself `Option<Network>` -- see `NetworkPlanner::plan`'s equivalent `unwrap_or` pattern for
/// `management_cidr`/`container_cidr`).
pub fn default_dns_forwarders() -> Vec<Ipv4Addr> {
    vec![Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(8, 8, 8, 8)]
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SecretsAdapter {
    pub adapter: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub config: Option<String>,
}

/// A `clear` env var value: YAML allows string, number, or boolean; all are coerced to `String`
/// for actual use.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ClearValue {
    String(String),
    Number(serde_yaml::Number),
    Bool(bool),
}

impl fmt::Display for ClearValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClearValue::String(s) => write!(f, "{s}"),
            ClearValue::Number(n) => write!(f, "{n}"),
            ClearValue::Bool(b) => write!(f, "{b}"),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Environment {
    #[serde(default)]
    pub clear: HashMap<String, ClearValue>,
    #[serde(default)]
    pub secrets: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BuildConfig {
    pub context: String,
    #[serde(default)]
    pub dockerfile: Option<String>,
    #[serde(default)]
    pub args: Option<HashMap<String, String>>,
    #[serde(default)]
    pub target: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum BuildValue {
    Context(String),
    Detailed(BuildConfig),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CommandValue {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CpusValue {
    Number(f64),
    Text(String),
}

/// A file/directory mount: `"local:remote[:options]"` or the detailed object form.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum MountConfig {
    Str(String),
    Detailed {
        local: String,
        remote: String,
        #[serde(default)]
        mode: Option<String>,
        #[serde(default)]
        owner: Option<String>,
        #[serde(default)]
        options: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SslValue {
    Enabled(bool),
    Certs {
        certificate_pem: String,
        private_key_pem: String,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HealthcheckConfig {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub cmd: Option<String>,
    #[serde(default)]
    pub cmd_runtime: Option<ContainerEngine>,
    #[serde(default)]
    pub interval: Option<String>,
    #[serde(default)]
    pub timeout: Option<String>,
    #[serde(default)]
    pub deploy_timeout: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyTarget {
    pub port: u32,
    #[serde(default)]
    pub hosts: Option<Vec<String>>,
    #[serde(default)]
    pub ssl: Option<SslValue>,
    #[serde(default)]
    pub path_prefix: Option<String>,
    #[serde(default)]
    pub healthcheck: Option<HealthcheckConfig>,
    /// Presence selects raw TCP mode instead of HTTP Host-header routing:
    /// the public port jiji-proxy exposes this target on, distinct from
    /// `port` (the backend container port). Mutually exclusive with
    /// `path_prefix`/`ssl`, which are HTTP-only concepts -- see
    /// `jiji_config::validation::validate_tcp_targets`.
    #[serde(default)]
    pub listen_port: Option<u16>,
}

/// Single-target fields live directly on this struct; multi-target configs use `targets`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    #[serde(default)]
    pub port: Option<u32>,
    #[serde(default)]
    pub hosts: Option<Vec<String>>,
    #[serde(default)]
    pub ssl: Option<SslValue>,
    #[serde(default)]
    pub path_prefix: Option<String>,
    #[serde(default)]
    pub healthcheck: Option<HealthcheckConfig>,
    #[serde(default)]
    pub targets: Option<Vec<ProxyTarget>>,
    /// See `ProxyTarget::listen_port`.
    #[serde(default)]
    pub listen_port: Option<u16>,
}

/// Mirrors Docker/Podman's `--restart` values exactly (`Display`/serde both render the same
/// kebab-case strings `container_runtime.rs` passes straight through as the flag value).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    #[default]
    UnlessStopped,
    Always,
    OnFailure,
    No,
}

impl fmt::Display for RestartPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RestartPolicy::UnlessStopped => write!(f, "unless-stopped"),
            RestartPolicy::Always => write!(f, "always"),
            RestartPolicy::OnFailure => write!(f, "on-failure"),
            RestartPolicy::No => write!(f, "no"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlacementPolicy {
    #[default]
    Spread,
    Packed,
}

impl fmt::Display for PlacementPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlacementPolicy::Spread => write!(f, "spread"),
            PlacementPolicy::Packed => write!(f, "packed"),
        }
    }
}

/// `overlap: forbid` skips a due run while the prior run is still active. Modeled as a
/// single-variant enum (not `bool`) so a later release can add a queuing/allow variant without a
/// schema-breaking change; any other value is rejected at parse time like `ContainerEngine`'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CronOverlap {
    #[default]
    Forbid,
}

/// `missed_runs: skip` does not replay scheduled times missed while the owning agent was
/// offline. Single-variant for the same forward-compatibility reason as `CronOverlap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CronMissedRuns {
    #[default]
    Skip,
}

/// One scheduled command for a service, run in its own one-off container rather than inside the
/// serving container (see `plans/service-cron.md`).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CronConfig {
    pub schedule: String,
    pub command: CommandValue,
    #[serde(default = "default_cron_timezone")]
    pub timezone: String,
    #[serde(default = "default_cron_timeout")]
    pub timeout: String,
    #[serde(default)]
    pub overlap: CronOverlap,
    #[serde(default)]
    pub missed_runs: CronMissedRuns,
}

impl CronConfig {
    /// Parses `timeout` per `parse_cron_duration`; `None` means the configured value is not a
    /// well-formed duration (caught by `jiji_config::validation` before this would matter).
    pub fn timeout_duration(&self) -> Option<Duration> {
        parse_cron_duration(&self.timeout)
    }
}

/// Parses `"<digits><unit>"` where unit is `s`, `m`, or `h`. A cron `timeout` commonly runs
/// longer than a healthcheck's typical seconds/minutes range (default `1h`), hence `h` here
/// where `jiji-cli`'s healthcheck duration parsing stops at `m`.
pub fn parse_cron_duration(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.len() < 2 {
        return None;
    }
    let (digits, unit) = value.split_at(value.len() - 1);
    let amount: u64 = digits.parse().ok()?;
    match unit {
        "s" => Some(Duration::from_secs(amount)),
        "m" => Some(Duration::from_secs(amount * 60)),
        "h" => Some(Duration::from_secs(amount * 3600)),
        _ => None,
    }
}

fn default_cron_timezone() -> String {
    "UTC".to_string()
}

fn default_cron_timeout() -> String {
    "1h".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Service {
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub build: Option<BuildValue>,
    #[serde(default)]
    pub servers: Vec<String>,
    #[serde(default = "default_replicas")]
    pub replicas: u32,
    #[serde(default)]
    pub placement: PlacementPolicy,
    #[serde(default)]
    pub ports: Vec<String>,
    #[serde(default)]
    pub volumes: Vec<String>,
    #[serde(default)]
    pub files: Vec<MountConfig>,
    #[serde(default)]
    pub directories: Vec<MountConfig>,
    #[serde(default)]
    pub environment: Environment,
    #[serde(default)]
    pub command: Option<CommandValue>,
    #[serde(default)]
    pub proxy: Option<ProxyConfig>,
    #[serde(default = "default_retain")]
    pub retain: u32,
    #[serde(default = "default_network_mode")]
    pub network_mode: String,
    #[serde(default)]
    pub cpus: Option<CpusValue>,
    #[serde(default)]
    pub memory: Option<String>,
    #[serde(default)]
    pub gpus: Option<String>,
    #[serde(default)]
    pub devices: Vec<String>,
    #[serde(default)]
    pub privileged: bool,
    #[serde(default)]
    pub cap_add: Vec<String>,
    #[serde(default)]
    pub stop_first: bool,
    #[serde(default)]
    pub restart: Option<RestartPolicy>,
    /// Stable map key is the cron name. `BTreeMap` (not `HashMap`, unlike `Config.services`)
    /// keeps iteration order deterministic for canonical spec hashing and `list` output.
    #[serde(default)]
    pub crons: BTreeMap<String, CronConfig>,
}

impl Service {
    /// The upstream service name this service depends on, if `network_mode` is `service:<name>`
    /// (Docker Compose's shorthand for sharing another container's network namespace). Naming the
    /// upstream this way is itself the dependency declaration -- there is no separate
    /// `depends_on` field.
    pub fn network_mode_dependency(&self) -> Option<&str> {
        self.network_mode.strip_prefix("service:")
    }
}

fn default_true() -> bool {
    true
}

fn default_registry_port() -> u16 {
    jiji_core::DEFAULT_LOCAL_REGISTRY_PORT
}

fn default_ssh_port() -> u16 {
    22
}

fn default_connect_timeout() -> u32 {
    30
}

fn default_command_timeout() -> u32 {
    300
}

fn default_max_concurrent_starts() -> u32 {
    30
}

fn default_pool_idle_timeout() -> u32 {
    900
}

fn default_dns_retries() -> u32 {
    3
}

fn default_retain() -> u32 {
    3
}

fn default_replicas() -> u32 {
    1
}

fn default_network_mode() -> String {
    "bridge".to_string()
}
