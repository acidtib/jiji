use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::{Parser, Subcommand};
use jiji_agent::api::{self, AgentApi, Identity, Request, RequestBody};
use jiji_agent::engine::Engine;
use jiji_agent::store::AgentStore;
use tokio::net::{UnixListener, UnixStream};

#[derive(Parser)]
#[command(name = "jiji-agent", about = "Jiji's project-scoped host agent")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Prints this binary's own version, bare (no formatting), so `jiji server setup`/`jiji
    /// server upgrade` can compare a locally discovered binary against the version this CLI was
    /// built alongside before trusting it -- see `agent_distribution::resolve_agent_binary_source`.
    Version,
    /// Runs the agent in the foreground (the systemd unit rendered by `jiji server setup` execs
    /// this directly, `Type=simple`).
    Run {
        #[arg(long)]
        project: String,
        #[arg(long)]
        engine: Engine,
        #[arg(long)]
        state_dir: PathBuf,
        #[arg(long)]
        socket: PathBuf,
        /// Enables catalog/desired-state replication and incremental
        /// WireGuard repair once enrollment has written this file.
        #[arg(long)]
        mesh_config: PathBuf,
        #[arg(long, default_value_t = 10)]
        discovery_interval_secs: u64,
    },
    /// One-shot health check against a running agent's socket, for install verification and
    /// manual smoke testing.
    Ping {
        #[arg(long)]
        socket: PathBuf,
    },
    /// Sends one JSON control request from stdin to the running project agent.
    /// This is the narrow SSH bridge used by `jiji`; the socket remains local
    /// and root-owned rather than being exposed on the mesh.
    Request {
        #[arg(long)]
        socket: PathBuf,
    },
    /// Imports membership pushed directly by `jiji-cli` over SSH into the durable store -- the
    /// only way membership ever changes (no peer-to-peer relay, see `jiji_agent::membership`).
    /// Used for enrollment/bootstrap and for every later membership change `jiji server setup`
    /// derives by reconciling `jiji.yml` against each host's observed WireGuard identity.
    MembershipImport {
        #[arg(long)]
        project: String,
        #[arg(long)]
        state_dir: PathBuf,
        #[arg(long)]
        mesh_config: PathBuf,
        #[arg(long)]
        input: PathBuf,
    },
    /// Prints the durable membership operation log.
    MembershipExport {
        #[arg(long)]
        state_dir: PathBuf,
    },
    /// Prints the winning catalog record for every known replica (including tombstones), for
    /// `jiji network catalog` to run over SSH -- mirrors `MembershipExport`'s pattern, since the
    /// CLI has no direct WireGuard-mesh reachability to call the socket API remotely.
    CatalogExport {
        #[arg(long)]
        state_dir: PathBuf,
    },
    /// Exports a secret-free catalog/desired-state winning snapshot plus this host's address
    /// claims.
    BackupExport {
        #[arg(long)]
        project: String,
        #[arg(long)]
        state_dir: PathBuf,
        #[arg(long)]
        mesh_config: PathBuf,
    },
    /// Verifies and imports one same-project, same-epoch agent backup snapshot.
    BackupImport {
        #[arg(long)]
        project: String,
        #[arg(long)]
        state_dir: PathBuf,
        #[arg(long)]
        mesh_config: PathBuf,
        #[arg(long)]
        input: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Run {
            project,
            engine,
            state_dir,
            socket,
            mesh_config,
            discovery_interval_secs,
        } => {
            run(
                project,
                engine,
                state_dir,
                socket,
                mesh_config,
                discovery_interval_secs,
            )
            .await
        }
        Command::Ping { socket } => ping(socket).await,
        Command::Request { socket } => request(socket).await,
        Command::MembershipImport {
            project,
            state_dir,
            mesh_config,
            input,
        } => membership_import(&project, &state_dir, &mesh_config, &input),
        Command::MembershipExport { state_dir } => membership_export(&state_dir),
        Command::CatalogExport { state_dir } => catalog_export(&state_dir),
        Command::BackupExport {
            project,
            state_dir,
            mesh_config,
        } => backup_export(&project, &state_dir, &mesh_config),
        Command::BackupImport {
            project,
            state_dir,
            mesh_config,
            input,
        } => backup_import(&project, &state_dir, &mesh_config, &input),
    }
}

fn backup_import(
    project: &str,
    state_dir: &std::path::Path,
    mesh_config: &std::path::Path,
    input: &std::path::Path,
) -> anyhow::Result<()> {
    let config = jiji_agent::runtime::MeshConfig::load(mesh_config, project)?;
    let store = AgentStore::open(&state_dir.join("agent.sqlite3"))?;
    let snapshot: jiji_agent::backup::AgentBackupSnapshot =
        serde_json::from_slice(&std::fs::read(input)?)?;
    snapshot.import(&store, &config.project_id, config.recovery_epoch)?;
    println!("Imported control-plane snapshot.");
    Ok(())
}

fn backup_export(
    project: &str,
    state_dir: &std::path::Path,
    mesh_config: &std::path::Path,
) -> anyhow::Result<()> {
    let config = jiji_agent::runtime::MeshConfig::load(mesh_config, project)?;
    let store = AgentStore::open(&state_dir.join("agent.sqlite3"))?;
    let snapshot = jiji_agent::backup::AgentBackupSnapshot::export(
        &store,
        &config.project_id,
        config.recovery_epoch,
        &config.node_id,
    )?;
    println!("{}", serde_json::to_string(&snapshot)?);
    Ok(())
}

fn catalog_export(state_dir: &std::path::Path) -> anyhow::Result<()> {
    let store = AgentStore::open(&state_dir.join("agent.sqlite3"))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&store.latest_catalog()?)?
    );
    Ok(())
}

fn membership_import(
    project: &str,
    state_dir: &std::path::Path,
    mesh_config_path: &std::path::Path,
    input: &std::path::Path,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let config = jiji_agent::runtime::MeshConfig::load(mesh_config_path, project)?;
    let scope = jiji_agent::membership::MembershipScope::new(
        config.project_id.clone(),
        config.recovery_epoch,
    );
    let records: Vec<jiji_agent::membership::MembershipRecord> =
        serde_json::from_slice(&std::fs::read(input)?)?;
    let db_path = state_dir.join("agent.sqlite3");
    let store = open_for_membership_import(&db_path, config.recovery_epoch)?;
    for record in &records {
        store.apply_membership(record.clone(), &scope)?;
    }
    println!("Imported {} membership record(s).", records.len());
    Ok(())
}

fn open_for_membership_import(
    db_path: &std::path::Path,
    recovery_epoch: u64,
) -> anyhow::Result<AgentStore> {
    let existing = AgentStore::open(db_path)?;
    let epochs = existing
        .membership_operations()?
        .into_iter()
        .map(|record| record.recovery_epoch)
        .collect::<std::collections::BTreeSet<_>>();
    if epochs.is_empty() || epochs == std::collections::BTreeSet::from([recovery_epoch]) {
        return Ok(existing);
    }
    drop(existing);
    let prior_epoch = epochs
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join("-");
    let archived = db_path.with_extension(format!("pre-recovery-epoch-{prior_epoch}.sqlite3"));
    if archived.exists() {
        anyhow::bail!(
            "{} already exists; inspect the previous recovery attempt before replacing agent state",
            archived.display()
        );
    }
    std::fs::rename(db_path, &archived)?;
    for suffix in ["-wal", "-shm"] {
        let source = std::path::PathBuf::from(format!("{}{suffix}", db_path.display()));
        if source.exists() {
            let destination = std::path::PathBuf::from(format!("{}{suffix}", archived.display()));
            std::fs::rename(source, destination)?;
        }
    }
    tracing::warn!(
        from = %prior_epoch,
        to = recovery_epoch,
        archive = %archived.display(),
        "archived old-epoch agent state before recovery enrollment"
    );
    Ok(AgentStore::open(db_path)?)
}

fn membership_export(state_dir: &std::path::Path) -> anyhow::Result<()> {
    let store = AgentStore::open(&state_dir.join("agent.sqlite3"))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&store.membership_operations()?)?
    );
    Ok(())
}

/// How often the scheduler evaluates every installed cron job for a due run. Matches
/// `reconcile_interval_secs`'s own default (`runtime.rs`): frequent enough that a due time is
/// never missed by more than a few seconds, without polling every job on every tick of a much
/// tighter loop.
const SCHEDULER_TICK_INTERVAL_SECS: u64 = 10;
/// How often the scheduler applies cron run metadata/container retention (`scheduler.rs`'s
/// `METADATA_RETAIN_SECS`/`CONTAINER_RETAIN_SECS`, both measured in hours/days) -- far less
/// frequent than the due-run tick above, since retention only ever removes what's already old.
const SCHEDULER_CLEANUP_INTERVAL_SECS: u64 = 3600;

async fn run(
    project: String,
    engine: Engine,
    state_dir: PathBuf,
    socket_path: PathBuf,
    mesh_config_path: PathBuf,
    discovery_interval_secs: u64,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&state_dir)?;
    let db_path = state_dir.join("agent.sqlite3");
    let store = AgentStore::open(&db_path).map_err(|error| {
        anyhow::anyhow!(
            "could not open local store at {}: {error}. The agent will not start against \
             corrupt or unsupported state; see the error above for the required recovery step.",
            db_path.display()
        )
    })?;
    tracing::info!(project, engine = %engine, db = %db_path.display(), "local store ready");
    let store = Arc::new(Mutex::new(store));

    let listener = bind_socket(&socket_path).await?;
    tracing::info!(socket = %socket_path.display(), "agent API listening");

    let config = jiji_agent::runtime::MeshConfig::load(&mesh_config_path, &project)?;
    store
        .lock()
        .map_err(|_| anyhow::anyhow!("local store lock poisoned"))?
        .set_soft_quota_bytes(Some(config.store_soft_quota_bytes));
    match jiji_agent::discovery::discover(engine, &project).await {
        jiji_agent::discovery::DiscoveryOutcome::Observed(observations) => {
            let recovered = {
                let store = store
                    .lock()
                    .map_err(|_| anyhow::anyhow!("local store lock poisoned"))?;
                jiji_agent::discovery::recover_labeled_leases(&store, &observations)?
            };
            tracing::info!(
                recovered,
                "startup container inventory and address leases reconciled"
            );
        }
        jiji_agent::discovery::DiscoveryOutcome::EngineUnavailable(detail)
        | jiji_agent::discovery::DiscoveryOutcome::EngineError(detail) => {
            tracing::warn!(
                %detail,
                "startup container inventory deferred; the retry loop will reconcile it"
            );
        }
    }
    let mut discovery = tokio::spawn(jiji_agent::discovery::run_loop(
        Arc::clone(&store),
        engine,
        project.clone(),
        Duration::from_secs(discovery_interval_secs),
    ));
    tracing::info!(
        node_id = %config.node_id,
        bind = %config.replication_bind,
        "authoritative mesh runtime enabled"
    );
    let mesh_config = Arc::new(config.clone());
    let api = AgentApi::new(
        Arc::clone(&store),
        Identity {
            project: project.clone(),
            engine: engine.to_string(),
        },
        socket_path.display().to_string(),
    )
    .with_peer_reachability_timeout(config.peer_reachability_timeout_secs())
    .with_catalog_identity(config.identity())
    .with_engine(engine)
    .with_mesh_config(Arc::clone(&mesh_config));
    let startup_candidates = store
        .lock()
        .map_err(|_| anyhow::anyhow!("local store lock poisoned"))?
        .latest_catalog()?
        .into_iter()
        .filter(|record| {
            record.owner_node_id == config.node_id
                && record.state == jiji_agent::catalog::DeploymentState::Candidate
        })
        .map(|record| record.deployment_id)
        .collect();
    // `runtime::run` (spawned below) binds sockets to the WireGuard management address and the
    // bridge's DNS address. Neither exists on any interface yet on a cold boot, and nothing else
    // guarantees it does before this point now that Phase 9 removed the systemd-level
    // `Requires=wg-quick@...`/`jiji-network-restore-{slug}` ordering an earlier design relied on
    // -- so bring both up synchronously here, with retries, before spawning `runtime::run` and
    // this module's own `local_reconcile::run_loop` as independent concurrent tasks. Confirmed
    // live: without this, a real reboot raced the two and crash-looped the whole agent on
    // `EADDRNOTAVAIL`.
    let mut link_attempts = 0;
    loop {
        match jiji_agent::local_reconcile::ensure_network_links(engine, &config, &store).await {
            Ok(()) => break,
            Err(error) if link_attempts < 30 => {
                link_attempts += 1;
                tracing::warn!(%error, attempt = link_attempts, "network links not ready yet, retrying");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(error) => {
                tracing::error!(%error, "network links still not ready after 30 attempts; continuing, ongoing reconciliation will keep retrying");
                break;
            }
        }
    }
    let mut local_reconcile = tokio::spawn(jiji_agent::local_reconcile::run_loop(
        Arc::clone(&store),
        engine,
        config.clone(),
        startup_candidates,
    ));
    let mut mesh = tokio::spawn(jiji_agent::runtime::run(config, Arc::clone(&store)));
    // Must complete before the API starts accepting `CronRun` requests (the plan's "Address
    // Leases and Networking" section): reconciles every still-`claimed`/`running` cron run
    // against actual local containers so a restart never starts a duplicate for one already in
    // flight, nor leaves a permanently "active" ghost run blocking future claims.
    jiji_agent::cron_exec::recover_claimed_runs(Arc::clone(&store), engine, &project).await;
    let mut serve = tokio::spawn(api::serve(listener, api));
    // Spawned only after `serve` above: the scheduler's very first tick could otherwise claim and
    // start a run before the API (and thus `CronRun`) even exists to race against it, but there's
    // no harm either way since `recover_claimed_runs` (which the scheduler must follow) already
    // completed by this point regardless of `serve`'s own position.
    let mut scheduler = tokio::spawn(jiji_agent::scheduler::run_loop(
        Arc::clone(&store),
        engine,
        Arc::clone(&mesh_config),
        project.clone(),
        Duration::from_secs(SCHEDULER_TICK_INTERVAL_SECS),
        Duration::from_secs(SCHEDULER_CLEANUP_INTERVAL_SECS),
    ));

    let unexpected = tokio::select! {
        _ = wait_for_shutdown_signal() => {
            tracing::info!("shutdown signal received, exiting");
            None
        }
        result = &mut mesh => {
            Some(match result {
                Ok(Ok(())) => "authoritative mesh runtime stopped unexpectedly".to_string(),
                Ok(Err(error)) => format!("authoritative mesh runtime failed: {error}"),
                Err(error) => format!("authoritative mesh runtime task failed: {error}"),
            })
        }
        result = &mut discovery => {
            Some(match result {
                Ok(()) => "container discovery stopped unexpectedly".to_string(),
                Err(error) => format!("container discovery task failed: {error}"),
            })
        }
        result = &mut local_reconcile => {
            Some(match result {
                Ok(()) => "local reconciliation stopped unexpectedly".to_string(),
                Err(error) => format!("local reconciliation task failed: {error}"),
            })
        }
        result = &mut serve => {
            Some(match result {
                Ok(()) => "agent API stopped unexpectedly".to_string(),
                Err(error) => format!("agent API task failed: {error}"),
            })
        }
        result = &mut scheduler => {
            Some(match result {
                Ok(()) => "cron scheduler stopped unexpectedly".to_string(),
                Err(error) => format!("cron scheduler task failed: {error}"),
            })
        }
    };
    discovery.abort();
    local_reconcile.abort();
    mesh.abort();
    serve.abort();
    scheduler.abort();
    if let Some(error) = unexpected {
        anyhow::bail!(
            "{error}; exiting so the bounded systemd restart policy can recover all components"
        );
    }
    Ok(())
}

/// Binds the socket, replacing a stale leftover file from an unclean previous shutdown but never
/// one still owned by a live agent -- attempts to connect to an existing path first, and only
/// removes it if nothing answers.
async fn bind_socket(socket_path: &std::path::Path) -> anyhow::Result<UnixListener> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    if socket_path.exists() {
        if UnixStream::connect(socket_path).await.is_ok() {
            anyhow::bail!(
                "another agent is already listening on {}; refusing to start a second instance \
                 for the same project",
                socket_path.display()
            );
        }
        std::fs::remove_file(socket_path)?;
    }
    let listener = UnixListener::bind(socket_path)?;
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        let mut interrupt = signal(SignalKind::interrupt()).expect("install SIGINT handler");
        tokio::select! {
            _ = terminate.recv() => {},
            _ = interrupt.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn ping(socket_path: PathBuf) -> anyhow::Result<()> {
    let response = api::call(
        &socket_path,
        &Request {
            idempotency_key: None,
            body: RequestBody::Health,
        },
    )
    .await
    .map_err(|error| {
        anyhow::anyhow!(
            "could not reach agent at {}: {error}",
            socket_path.display()
        )
    })?;
    match response {
        Ok(body) => {
            println!("{}", serde_json::to_string_pretty(&body)?);
            Ok(())
        }
        Err(error) => anyhow::bail!("agent returned an error: {error:?}"),
    }
}

async fn request(socket_path: PathBuf) -> anyhow::Result<()> {
    use std::io::Read;

    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    let request: Request = serde_json::from_slice(&input)?;
    let response = api::call(&socket_path, &request).await.map_err(|error| {
        anyhow::anyhow!(
            "could not reach agent at {}: {error}",
            socket_path.display()
        )
    })?;
    println!("{}", serde_json::to_string(&response)?);
    if let Err(error) = response {
        anyhow::bail!("agent returned an error: {error:?}");
    }
    Ok(())
}
