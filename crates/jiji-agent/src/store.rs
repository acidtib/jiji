//! Durable local runtime state: never merged or replicated between hosts. SQLite in WAL mode,
//! versioned/transactional migrations, and a corruption gate that refuses to start serving from
//! partial or unreadable state rather than silently reinitializing over it.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::catalog::{CatalogApply, CatalogError, CatalogRecord, CatalogView};
use crate::cron::{
    CronClaimOutcome, CronJobSpec, CronRun, CronRunCause, CronRunFilter, CronRunState,
    CronSchedulerState, CronSpecApplyOutcome,
};
use crate::desired::{DesiredApply, DesiredError, DesiredStateRecord, DesiredStateView};
use crate::image_retention::ImageRetentionSpec;
use crate::membership::{
    MembershipApply, MembershipError, MembershipRecord, MembershipScope, MembershipState,
    MembershipView, RecordProvenance,
};
use crate::wireguard::PeerCacheEntry;
use std::net::Ipv4Addr;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error(
        "database at {path} failed an integrity check ({detail}); it will not be started \
         automatically. Restore it from a backup under the same directory or remove it to start \
         fresh, then restart the agent."
    )]
    Corrupted { path: String, detail: String },
    #[error(
        "database schema version {found} is newer than the {supported} version(s) this agent \
         binary supports; refusing to touch it. Upgrade the jiji-agent binary before restarting."
    )]
    UnsupportedSchema { found: i64, supported: i64 },
    #[error("migration to schema version {version} failed: {detail}")]
    MigrationFailed { version: i64, detail: String },
    #[error("membership validation failed: {0}")]
    Membership(#[from] MembershipError),
    #[error("catalog validation failed: {0}")]
    Catalog(#[from] CatalogError),
    #[error("desired state validation failed: {0}")]
    Desired(#[from] DesiredError),
    #[error(
        "agent database uses {used_bytes} bytes, reaching its configured soft quota of \
         {quota_bytes} bytes; reads and DNS remain available, but new replicated operations are \
         refused until the quota is raised or superseded history is compacted"
    )]
    QuotaExceeded { used_bytes: u64, quota_bytes: u64 },
    #[error("membership state serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

struct Migration {
    version: i64,
    sql: &'static str,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: "CREATE TABLE idempotency_keys (
              key TEXT PRIMARY KEY,
              response_json TEXT NOT NULL,
              created_at TEXT NOT NULL
          );
          CREATE TABLE observations (
              container_id TEXT PRIMARY KEY,
              name TEXT NOT NULL,
              image TEXT NOT NULL,
              labels_json TEXT NOT NULL,
              state TEXT NOT NULL,
              first_seen_at TEXT NOT NULL,
              last_seen_at TEXT NOT NULL
          );
          CREATE TABLE reconciliation_checkpoints (
              key TEXT PRIMARY KEY,
              value TEXT NOT NULL,
              updated_at TEXT NOT NULL
          );
          CREATE TABLE local_state (
              key TEXT PRIMARY KEY,
              value TEXT NOT NULL,
              revision INTEGER NOT NULL,
              updated_at TEXT NOT NULL
          );",
    },
    Migration {
        version: 2,
        sql: "CREATE TABLE membership_operations (
                  operation_id TEXT PRIMARY KEY,
                  record_json TEXT NOT NULL,
                  applied_at TEXT NOT NULL
              );
              CREATE TABLE membership_records (
                  node_id TEXT PRIMARY KEY,
                  server_name TEXT NOT NULL,
                  owner_epoch INTEGER NOT NULL,
                  revision INTEGER NOT NULL,
                  state TEXT NOT NULL,
                  operation_id TEXT NOT NULL,
                  record_json TEXT NOT NULL
              );
              CREATE UNIQUE INDEX membership_active_server_name
                  ON membership_records(server_name)
                  WHERE state = 'active';
              CREATE TABLE peer_cache (
                  node_id TEXT PRIMARY KEY,
                  wireguard_public_key TEXT NOT NULL,
                  management_address TEXT NOT NULL,
                  container_subnet TEXT NOT NULL,
                  endpoint TEXT NOT NULL,
                  owner_epoch INTEGER NOT NULL,
                  revision INTEGER NOT NULL,
                  updated_at TEXT NOT NULL
              );
              CREATE TABLE peer_cursors (
                  node_id TEXT PRIMARY KEY,
                  last_operation_id TEXT,
                  updated_at TEXT NOT NULL
              );",
    },
    Migration {
        version: 3,
        sql: "CREATE TABLE catalog_operations (
                  operation_id TEXT PRIMARY KEY,
                  record_json TEXT NOT NULL,
                  applied_at TEXT NOT NULL
              );
              CREATE TABLE catalog_records (
                  replica_id TEXT PRIMARY KEY,
                  service TEXT NOT NULL,
                  owner_node_id TEXT NOT NULL,
                  owner_epoch INTEGER NOT NULL,
                  revision INTEGER NOT NULL,
                  state TEXT NOT NULL,
                  operation_id TEXT NOT NULL,
                  record_json TEXT NOT NULL
              );
              CREATE TABLE node_liveness (
                  node_id TEXT PRIMARY KEY,
                  last_seen_at TEXT NOT NULL
              );",
    },
    Migration {
        version: 4,
        sql: "CREATE TABLE address_leases (
                  deployment_id TEXT PRIMARY KEY,
                  replica_id TEXT NOT NULL,
                  address TEXT NOT NULL UNIQUE,
                  state TEXT NOT NULL,
                  created_at TEXT NOT NULL,
                  quarantine_until INTEGER
              );
              CREATE INDEX address_leases_replica_id
                  ON address_leases(replica_id);
              CREATE TABLE scale_overrides (
                  service TEXT PRIMARY KEY,
                  replicas INTEGER NOT NULL,
                  revision INTEGER NOT NULL,
                  updated_at TEXT NOT NULL
              );
              CREATE TABLE replica_assignments (
                  replica_id TEXT PRIMARY KEY,
                  service TEXT NOT NULL,
                  ordinal INTEGER NOT NULL,
                  owner_node_id TEXT NOT NULL,
                  owner_epoch INTEGER NOT NULL,
                  state TEXT NOT NULL,
                  revision INTEGER NOT NULL,
                  updated_at TEXT NOT NULL,
                  UNIQUE(service, ordinal)
              );",
    },
    Migration {
        version: 5,
        sql: "DROP TABLE catalog_records;
              DROP TABLE catalog_operations;
              CREATE TABLE catalog_operations (
                  operation_id TEXT PRIMARY KEY,
                  record_json TEXT NOT NULL,
                  applied_at TEXT NOT NULL
              );
              CREATE TABLE catalog_records (
                  deployment_id TEXT PRIMARY KEY,
                  replica_id TEXT NOT NULL,
                  service TEXT NOT NULL,
                  owner_node_id TEXT NOT NULL,
                  owner_epoch INTEGER NOT NULL,
                  revision INTEGER NOT NULL,
                  state TEXT NOT NULL,
                  operation_id TEXT NOT NULL,
                  record_json TEXT NOT NULL
              );
              CREATE INDEX catalog_records_replica_id
                  ON catalog_records(replica_id);",
    },
    Migration {
        version: 6,
        sql: "CREATE TABLE desired_operations (
                  operation_id TEXT PRIMARY KEY,
                  service TEXT NOT NULL,
                  record_json TEXT NOT NULL,
                  applied_at TEXT NOT NULL
              );
              CREATE TABLE desired_records (
                  service TEXT PRIMARY KEY,
                  revision INTEGER NOT NULL,
                  operation_id TEXT NOT NULL,
                  record_json TEXT NOT NULL
              );",
    },
    Migration {
        version: 7,
        sql: "CREATE TABLE peer_sync_status (
                  node_id TEXT PRIMARY KEY,
                  last_success_at TEXT,
                  consecutive_failures INTEGER NOT NULL DEFAULT 0,
                  last_error TEXT,
                  updated_at TEXT NOT NULL
              );
              CREATE TABLE component_status (
                  component TEXT PRIMARY KEY,
                  last_attempt_at TEXT,
                  last_success_at TEXT,
                  consecutive_failures INTEGER NOT NULL DEFAULT 0,
                  last_error TEXT,
                  next_retry_at TEXT,
                  updated_at TEXT NOT NULL
              );
              CREATE TABLE maintenance_events (
                  id INTEGER PRIMARY KEY AUTOINCREMENT,
                  kind TEXT NOT NULL,
                  detail_json TEXT NOT NULL,
                  created_at TEXT NOT NULL
              );",
    },
    Migration {
        version: 8,
        // Local-only (see `cron.rs`'s module doc comment): no `_operations` replication log
        // table alongside these, unlike membership/catalog/desired-state above. `scheduled_at`
        // is nullable so SQLite's own NULL-is-never-equal semantics give scheduled runs a real
        // `(service, cron_name, scheduled_at)` dedup constraint while manual runs (always NULL)
        // never collide with each other or with a scheduled run.
        sql: "CREATE TABLE cron_job_specs (
                  service TEXT NOT NULL,
                  cron_name TEXT NOT NULL,
                  revision INTEGER NOT NULL,
                  canonical_hash TEXT NOT NULL,
                  spec_json TEXT NOT NULL,
                  updated_at TEXT NOT NULL,
                  PRIMARY KEY (service, cron_name)
              );
              CREATE TABLE cron_runs (
                  run_id TEXT PRIMARY KEY,
                  service TEXT NOT NULL,
                  cron_name TEXT NOT NULL,
                  cause TEXT NOT NULL,
                  scheduled_at INTEGER,
                  claimed_at INTEGER NOT NULL,
                  state TEXT NOT NULL,
                  run_json TEXT NOT NULL,
                  UNIQUE(service, cron_name, scheduled_at)
              );
              CREATE INDEX cron_runs_service_cron_name
                  ON cron_runs(service, cron_name, claimed_at);
              CREATE TABLE cron_scheduler_state (
                  service TEXT NOT NULL,
                  cron_name TEXT NOT NULL,
                  skipped_overlap_count INTEGER NOT NULL DEFAULT 0,
                  state_json TEXT NOT NULL,
                  updated_at TEXT NOT NULL,
                  PRIMARY KEY (service, cron_name)
              );",
    },
    Migration {
        version: 9,
        // Schema versions 2, 3, 5, and 6 originally created these operation logs with required
        // `signer_id` and `signature` columns. Membership signatures and the shared signature
        // wrapper for catalog/desired records were later removed from the runtime. Editing those
        // already-applied migrations only fixed fresh databases: an upgraded database kept the
        // old NOT NULL columns and rejected every new operation. Rebuild all affected logs from
        // their common columns so this migration works for both legacy and fresh version-8 stores.
        sql: "ALTER TABLE membership_operations RENAME TO membership_operations_v8;
              CREATE TABLE membership_operations (
                  operation_id TEXT PRIMARY KEY,
                  record_json TEXT NOT NULL,
                  applied_at TEXT NOT NULL
              );
              INSERT INTO membership_operations (operation_id, record_json, applied_at)
                  SELECT operation_id, record_json, applied_at FROM membership_operations_v8;
              DROP TABLE membership_operations_v8;

              ALTER TABLE catalog_operations RENAME TO catalog_operations_v8;
              CREATE TABLE catalog_operations (
                  operation_id TEXT PRIMARY KEY,
                  record_json TEXT NOT NULL,
                  applied_at TEXT NOT NULL
              );
              INSERT INTO catalog_operations (operation_id, record_json, applied_at)
                  SELECT operation_id, record_json, applied_at FROM catalog_operations_v8;
              DROP TABLE catalog_operations_v8;

              ALTER TABLE desired_operations RENAME TO desired_operations_v8;
              CREATE TABLE desired_operations (
                  operation_id TEXT PRIMARY KEY,
                  service TEXT NOT NULL,
                  record_json TEXT NOT NULL,
                  applied_at TEXT NOT NULL
              );
              INSERT INTO desired_operations (operation_id, service, record_json, applied_at)
                  SELECT operation_id, service, record_json, applied_at
                  FROM desired_operations_v8;
              DROP TABLE desired_operations_v8;",
    },
    Migration {
        version: 10,
        // Local-only, one row per build-configured service (see `image_retention.rs`'s module
        // doc comment): mirrors `cron_job_specs`' shape, but keyed by `service` alone since
        // retention is pushed identically to every host in a service's eligible `servers:` set,
        // not owned by a single node the way a cron job is.
        sql: "CREATE TABLE image_retention_specs (
                  service TEXT PRIMARY KEY,
                  repo TEXT NOT NULL,
                  retain INTEGER NOT NULL,
                  updated_at TEXT NOT NULL
              );",
    },
    Migration {
        version: 11,
        // `scale_overrides`/`replica_assignments` (from migration 4) were superseded by
        // `desired_operations`/`desired_records` (migration 6) before anything ever read or
        // wrote them -- dead tables sitting in every agent database since. Dropped here as
        // cleanup while touching desired-state semantics for the `servers:`-is-literal /
        // `scale:`-is-per-server rework.
        sql: "DROP TABLE scale_overrides;
              DROP TABLE replica_assignments;",
    },
];

fn current_schema_version() -> i64 {
    MIGRATIONS.last().map_or(0, |m| m.version)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub container_id: String,
    pub name: String,
    pub image: String,
    pub labels_json: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressLease {
    pub deployment_id: String,
    pub replica_id: String,
    pub address: Ipv4Addr,
    pub state: String,
    pub quarantine_until: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionResult {
    pub membership_removed: usize,
    pub catalog_removed: usize,
    pub desired_removed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerSyncStatus {
    pub node_id: String,
    pub last_success_at: Option<String>,
    pub consecutive_failures: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationCounts {
    pub membership: u64,
    pub catalog: u64,
    pub desired: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentStatus {
    pub component: String,
    pub last_attempt_at: Option<String>,
    pub last_success_at: Option<String>,
    pub consecutive_failures: u64,
    pub last_error: Option<String>,
    pub next_retry_at: Option<String>,
}

#[derive(Debug)]
pub struct AgentStore {
    conn: Connection,
    soft_quota_bytes: Cell<Option<u64>>,
}

fn now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    secs.to_string()
}

fn lease_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AddressLease> {
    let address = row.get::<_, String>(2)?;
    let address = address.parse().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(AddressLease {
        deployment_id: row.get(0)?,
        replica_id: row.get(1)?,
        address,
        state: row.get(3)?,
        quarantine_until: row.get(4)?,
    })
}

impl AgentStore {
    /// Opens (creating if absent) the store at `db_path`. Runs the integrity check first and
    /// refuses to proceed on a corrupt file; backs up the existing file before applying any
    /// pending migration; refuses a schema newer than this binary understands. None of these
    /// failure paths mutate the file on disk.
    pub fn open(db_path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let existed_before = db_path.exists();
        let conn = Connection::open(db_path)?;
        // Checked before touching any pragma or table: on a corrupt/non-database file, even
        // `PRAGMA journal_mode` can fail, and a failure there must still surface as `Corrupted`
        // rather than a generic database error.
        if existed_before {
            check_integrity(&conn, db_path)?;
        }
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 version INTEGER PRIMARY KEY,
                 applied_at TEXT NOT NULL
             );",
        )?;

        let installed_version: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        let supported = current_schema_version();
        if installed_version > supported {
            return Err(StoreError::UnsupportedSchema {
                found: installed_version,
                supported,
            });
        }

        let pending: Vec<&Migration> = MIGRATIONS
            .iter()
            .filter(|m| m.version > installed_version)
            .collect();

        if !pending.is_empty() {
            if existed_before {
                backup_before_migration(db_path, installed_version)?;
            }
            for migration in pending {
                apply_migration(&conn, migration)?;
            }
        }

        Ok(Self {
            conn,
            soft_quota_bytes: Cell::new(None),
        })
    }

    pub fn set_soft_quota_bytes(&self, quota_bytes: Option<u64>) {
        self.soft_quota_bytes.set(quota_bytes);
    }

    pub fn soft_quota_bytes(&self) -> Option<u64> {
        self.soft_quota_bytes.get()
    }

    pub fn database_usage_bytes(&self) -> Result<u64, StoreError> {
        let pages: u64 = self
            .conn
            .query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let page_size: u64 = self
            .conn
            .query_row("PRAGMA page_size", [], |row| row.get(0))?;
        Ok(pages.saturating_mul(page_size))
    }

    fn ensure_replication_capacity(&self) -> Result<(), StoreError> {
        let Some(quota_bytes) = self.soft_quota_bytes.get() else {
            return Ok(());
        };
        let used_bytes = self.database_usage_bytes()?;
        if used_bytes >= quota_bytes {
            return Err(StoreError::QuotaExceeded {
                used_bytes,
                quota_bytes,
            });
        }
        Ok(())
    }

    pub fn operation_counts(&self) -> Result<OperationCounts, StoreError> {
        Ok(OperationCounts {
            membership: self.conn.query_row(
                "SELECT COUNT(*) FROM membership_operations",
                [],
                |row| row.get(0),
            )?,
            catalog: self
                .conn
                .query_row("SELECT COUNT(*) FROM catalog_operations", [], |row| {
                    row.get(0)
                })?,
            desired: self
                .conn
                .query_row("SELECT COUNT(*) FROM desired_operations", [], |row| {
                    row.get(0)
                })?,
        })
    }

    pub fn record_peer_sync_success(&self, node_id: &str) -> Result<(), StoreError> {
        let timestamp = now();
        self.conn.execute(
            "INSERT INTO peer_sync_status
                 (node_id, last_success_at, consecutive_failures, last_error, updated_at)
             VALUES (?1, ?2, 0, NULL, ?2)
             ON CONFLICT(node_id) DO UPDATE SET
               last_success_at=excluded.last_success_at,
               consecutive_failures=0,
               last_error=NULL,
               updated_at=excluded.updated_at",
            params![node_id, timestamp],
        )?;
        Ok(())
    }

    pub fn record_peer_sync_failure(&self, node_id: &str, error: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO peer_sync_status
                 (node_id, last_success_at, consecutive_failures, last_error, updated_at)
             VALUES (?1, NULL, 1, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET
               consecutive_failures=peer_sync_status.consecutive_failures + 1,
               last_error=excluded.last_error,
               updated_at=excluded.updated_at",
            params![node_id, error, now()],
        )?;
        Ok(())
    }

    pub fn delete_peer_sync_status(&self, node_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM peer_sync_status WHERE node_id = ?1",
            params![node_id],
        )?;
        Ok(())
    }

    pub fn peer_sync_statuses(&self) -> Result<Vec<PeerSyncStatus>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT node_id, last_success_at, consecutive_failures, last_error
             FROM peer_sync_status ORDER BY node_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(PeerSyncStatus {
                node_id: row.get(0)?,
                last_success_at: row.get(1)?,
                consecutive_failures: row.get(2)?,
                last_error: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn record_component_result(
        &self,
        component: &str,
        result: Result<(), &str>,
        next_retry_at: Option<u64>,
    ) -> Result<(), StoreError> {
        let timestamp = now();
        match result {
            Ok(()) => {
                self.conn.execute(
                    "INSERT INTO component_status
                         (component, last_attempt_at, last_success_at, consecutive_failures,
                          last_error, next_retry_at, updated_at)
                     VALUES (?1, ?2, ?2, 0, NULL, NULL, ?2)
                     ON CONFLICT(component) DO UPDATE SET
                       last_attempt_at=excluded.last_attempt_at,
                       last_success_at=excluded.last_success_at,
                       consecutive_failures=0,
                       last_error=NULL,
                       next_retry_at=NULL,
                       updated_at=excluded.updated_at",
                    params![component, timestamp],
                )?;
            }
            Err(error) => {
                self.conn.execute(
                    "INSERT INTO component_status
                         (component, last_attempt_at, last_success_at, consecutive_failures,
                          last_error, next_retry_at, updated_at)
                     VALUES (?1, ?2, NULL, 1, ?3, ?4, ?2)
                     ON CONFLICT(component) DO UPDATE SET
                       last_attempt_at=excluded.last_attempt_at,
                       consecutive_failures=component_status.consecutive_failures + 1,
                       last_error=excluded.last_error,
                       next_retry_at=excluded.next_retry_at,
                       updated_at=excluded.updated_at",
                    params![
                        component,
                        timestamp,
                        error,
                        next_retry_at.map(|value| value.to_string())
                    ],
                )?;
            }
        }
        Ok(())
    }

    pub fn component_statuses(&self) -> Result<Vec<ComponentStatus>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT component, last_attempt_at, last_success_at, consecutive_failures,
                    last_error, next_retry_at
             FROM component_status ORDER BY component",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(ComponentStatus {
                component: row.get(0)?,
                last_attempt_at: row.get(1)?,
                last_success_at: row.get(2)?,
                consecutive_failures: row.get(3)?,
                last_error: row.get(4)?,
                next_retry_at: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn schema_version(&self) -> Result<i64, StoreError> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn idempotent_get(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT response_json FROM idempotency_keys WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn idempotent_put(&self, key: &str, response_json: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO idempotency_keys (key, response_json, created_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO NOTHING",
            params![key, response_json, now()],
        )?;
        Ok(())
    }

    /// The local-transaction primitive: an atomic read-modify-write of one key with a
    /// monotonically increasing revision, the plumbing later phases build the real service
    /// catalog cutover on top of (see "Target Deployment Transaction" in the distributed
    /// control-plane plan). Phase 2 does not yet give this catalog semantics.
    pub fn commit_local_transaction(&self, key: &str, value: &str) -> Result<i64, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let previous_revision: i64 = tx
            .query_row(
                "SELECT revision FROM local_state WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let revision = previous_revision + 1;
        tx.execute(
            "INSERT INTO local_state (key, value, revision, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(key) DO UPDATE SET
               value = excluded.value, revision = excluded.revision, updated_at = excluded.updated_at",
            params![key, value, revision, now()],
        )?;
        tx.commit()?;
        Ok(revision)
    }

    pub fn read_local_state(&self, key: &str) -> Result<Option<(String, i64)>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT value, revision FROM local_state WHERE key = ?1",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    pub fn set_checkpoint(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO reconciliation_checkpoints (key, value, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value, now()],
        )?;
        Ok(())
    }

    pub fn get_checkpoint(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM reconciliation_checkpoints WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn upsert_observation(&self, observation: &Observation) -> Result<(), StoreError> {
        let timestamp = now();
        self.conn.execute(
            "INSERT INTO observations
                 (container_id, name, image, labels_json, state, first_seen_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(container_id) DO UPDATE SET
               name = excluded.name,
               image = excluded.image,
               labels_json = excluded.labels_json,
               state = excluded.state,
               last_seen_at = excluded.last_seen_at",
            params![
                observation.container_id,
                observation.name,
                observation.image,
                observation.labels_json,
                observation.state,
                timestamp,
            ],
        )?;
        Ok(())
    }

    /// Removes observations not present in `current_ids`, i.e. containers the last discovery
    /// pass no longer sees. Observe-only bookkeeping, never touches the containers themselves.
    pub fn retain_observations(&self, current_ids: &[String]) -> Result<usize, StoreError> {
        let existing: Vec<String> = {
            let mut statement = self.conn.prepare("SELECT container_id FROM observations")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut removed = 0;
        for id in existing {
            if !current_ids.contains(&id) {
                self.conn
                    .execute("DELETE FROM observations WHERE container_id = ?1", [&id])?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn list_observations(&self) -> Result<Vec<Observation>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT container_id, name, image, labels_json, state FROM observations ORDER BY container_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Observation {
                container_id: row.get(0)?,
                name: row.get(1)?,
                image: row.get(2)?,
                labels_json: row.get(3)?,
                state: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn observation_count(&self) -> Result<i64, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM observations", [], |row| row.get(0))?)
    }

    pub fn address_lease(&self, deployment_id: &str) -> Result<Option<AddressLease>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT deployment_id, replica_id, address, state, quarantine_until
                 FROM address_leases WHERE deployment_id = ?1",
                [deployment_id],
                lease_from_row,
            )
            .optional()?)
    }

    pub fn address_leases(&self) -> Result<Vec<AddressLease>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT deployment_id, replica_id, address, state, quarantine_until
             FROM address_leases ORDER BY address",
        )?;
        let rows = statement.query_map([], lease_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Atomically claims `address` for a deployment. Repeating the same claim is idempotent;
    /// a different deployment can never steal an active or quarantined address.
    pub fn claim_address_lease(
        &self,
        deployment_id: &str,
        replica_id: &str,
        address: Ipv4Addr,
    ) -> Result<bool, StoreError> {
        let inserted = self.conn.execute(
            "INSERT INTO address_leases
                 (deployment_id, replica_id, address, state, created_at, quarantine_until)
             VALUES (?1, ?2, ?3, 'active', ?4, NULL)
             ON CONFLICT(deployment_id) DO NOTHING",
            params![deployment_id, replica_id, address.to_string(), now()],
        )?;
        if inserted == 1 {
            return Ok(true);
        }
        Ok(self
            .address_lease(deployment_id)?
            .is_some_and(|lease| lease.replica_id == replica_id && lease.address == address))
    }

    /// Reconstructs an active lease from a labeled local container. Exact repeats reactivate a
    /// quarantined lease after an agent/engine restart; a deployment or address conflict is
    /// refused and never overwritten.
    pub fn recover_address_lease(
        &self,
        deployment_id: &str,
        replica_id: &str,
        address: Ipv4Addr,
    ) -> Result<bool, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let deployment: Option<(String, String)> = tx
            .query_row(
                "SELECT replica_id, address FROM address_leases WHERE deployment_id = ?1",
                [deployment_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((existing_replica, existing_address)) = deployment {
            if existing_replica != replica_id || existing_address != address.to_string() {
                tx.commit()?;
                return Ok(false);
            }
            tx.execute(
                "UPDATE address_leases
                 SET state='active', quarantine_until=NULL
                 WHERE deployment_id=?1",
                [deployment_id],
            )?;
            tx.commit()?;
            return Ok(true);
        }
        let address_owner: Option<String> = tx
            .query_row(
                "SELECT deployment_id FROM address_leases WHERE address = ?1",
                [address.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if address_owner.is_some() {
            tx.commit()?;
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO address_leases
                 (deployment_id, replica_id, address, state, created_at, quarantine_until)
             VALUES (?1, ?2, ?3, 'active', ?4, NULL)",
            params![deployment_id, replica_id, address.to_string(), now()],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn quarantine_address_lease(
        &self,
        deployment_id: &str,
        quarantine_until: u64,
    ) -> Result<bool, StoreError> {
        Ok(self.conn.execute(
            "UPDATE address_leases
             SET state = 'quarantined', quarantine_until = ?2
             WHERE deployment_id = ?1",
            params![deployment_id, quarantine_until],
        )? == 1)
    }

    pub fn collect_expired_address_leases(&self, timestamp: u64) -> Result<usize, StoreError> {
        // A completed inventory is the positive evidence required to collect. Before the first
        // successful discovery pass (or if discovery has never checkpointed), timeout alone has
        // no deletion authority.
        if self.get_checkpoint("last_discovery_at")?.is_none() {
            return Ok(0);
        }
        let claimed = self
            .list_observations()?
            .into_iter()
            .filter_map(|observation| {
                let labels: serde_json::Value =
                    serde_json::from_str(&observation.labels_json).ok()?;
                labels
                    .get("jiji.deployment")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
            .collect::<Vec<_>>();
        let tx = self.conn.unchecked_transaction()?;
        let expired: Vec<String> = {
            let mut statement = tx.prepare(
                "SELECT deployment_id FROM address_leases
                 WHERE state = 'quarantined' AND quarantine_until <= ?1",
            )?;
            let rows = statement.query_map([timestamp], |row| row.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut removed = 0;
        for deployment_id in expired {
            if claimed.contains(&deployment_id) {
                continue;
            }
            removed += tx.execute(
                "DELETE FROM address_leases
                 WHERE deployment_id=?1 AND state='quarantined' AND quarantine_until <= ?2",
                params![deployment_id, timestamp],
            )?;
        }
        tx.commit()?;
        Ok(removed)
    }

    pub fn membership_operations(&self) -> Result<Vec<MembershipRecord>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT record_json FROM membership_operations ORDER BY rowid")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(serde_json::from_str(&row?)?);
        }
        Ok(records)
    }

    /// Verifies and durably materializes one CLI-pushed membership record.
    /// The operation and winning record commit atomically.
    pub fn apply_membership(
        &self,
        record: MembershipRecord,
        scope: &MembershipScope,
    ) -> Result<MembershipApply, StoreError> {
        let id = crate::membership::record_id(&record)?;
        if self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM membership_operations WHERE operation_id = ?1)",
            [&id],
            |row| row.get::<_, bool>(0),
        )? {
            return Ok(MembershipApply::Duplicate);
        }
        self.ensure_replication_capacity()?;
        let existing = self.membership_operations()?;
        let mut view = MembershipView::from_records(existing, scope)?;
        let outcome = view.apply(record.clone(), scope)?;
        let tx = self.conn.unchecked_transaction()?;
        let inserted = tx.execute(
            "INSERT INTO membership_operations (operation_id, record_json, applied_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(operation_id) DO NOTHING",
            params![id, serde_json::to_string(&record)?, now()],
        )?;
        if inserted == 0 {
            tx.commit()?;
            return Ok(MembershipApply::Duplicate);
        }
        if outcome == MembershipApply::Applied {
            tx.execute(
                "INSERT INTO membership_records
                     (node_id, server_name, owner_epoch, revision, state,
                      operation_id, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(node_id) DO UPDATE SET
                   server_name=excluded.server_name,
                   owner_epoch=excluded.owner_epoch,
                   revision=excluded.revision,
                   state=excluded.state,
                   operation_id=excluded.operation_id,
                   record_json=excluded.record_json",
                params![
                    record.node_id,
                    record.server_name,
                    record.owner_epoch,
                    record.revision,
                    match record.state {
                        crate::membership::MembershipState::Active => "active",
                        crate::membership::MembershipState::Tombstoned => "tombstoned",
                    },
                    id,
                    serde_json::to_string(&record)?,
                ],
            )?;
        }
        tx.commit()?;
        Ok(outcome)
    }

    pub fn active_membership(
        &self,
    ) -> Result<Vec<crate::membership::MembershipRecord>, StoreError> {
        Ok(self
            .latest_membership()?
            .into_iter()
            .filter(|record| record.state == MembershipState::Active)
            .collect())
    }

    /// Returns the winning record for every known node, including tombstones. Absence alone is
    /// deliberately never removal authority. This is also the set `jiji-cli` reconciles against
    /// when deciding whether a host's membership is stale.
    pub fn latest_membership(
        &self,
    ) -> Result<Vec<crate::membership::MembershipRecord>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT record_json FROM membership_records ORDER BY node_id")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(serde_json::from_str(&row?)?);
        }
        Ok(records)
    }

    pub fn replace_peer_cache(
        &self,
        cache: &std::collections::BTreeMap<String, PeerCacheEntry>,
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM peer_cache", [])?;
        for entry in cache.values() {
            tx.execute(
                "INSERT INTO peer_cache
                     (node_id, wireguard_public_key, management_address,
                      container_subnet, endpoint, owner_epoch, revision, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    entry.node_id,
                    entry.wireguard_public_key,
                    entry.management_address,
                    entry.container_subnet,
                    entry.endpoint.to_string(),
                    entry.owner_epoch,
                    entry.revision,
                    now(),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn peer_cache(
        &self,
    ) -> Result<std::collections::BTreeMap<String, PeerCacheEntry>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT node_id, wireguard_public_key, management_address,
                    container_subnet, endpoint, owner_epoch, revision
             FROM peer_cache ORDER BY node_id",
        )?;
        let rows = statement.query_map([], |row| {
            let endpoint = row.get::<_, String>(4)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                endpoint,
                row.get::<_, u64>(5)?,
                row.get::<_, u64>(6)?,
            ))
        })?;
        let mut cache = std::collections::BTreeMap::new();
        for row in rows {
            let (
                node_id,
                wireguard_public_key,
                management_address,
                container_subnet,
                endpoint,
                owner_epoch,
                revision,
            ) = row?;
            let endpoint = endpoint.parse().map_err(|_| StoreError::MigrationFailed {
                version: current_schema_version(),
                detail: format!("peer cache contains invalid endpoint '{endpoint}'"),
            })?;
            cache.insert(
                node_id.clone(),
                PeerCacheEntry {
                    node_id,
                    wireguard_public_key,
                    management_address,
                    container_subnet,
                    endpoint,
                    owner_epoch,
                    revision,
                },
            );
        }
        Ok(cache)
    }

    pub fn catalog_operations(&self) -> Result<Vec<CatalogRecord>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT record_json FROM catalog_operations ORDER BY rowid")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(serde_json::from_str(&row?)?);
        }
        Ok(records)
    }

    /// Verifies (against the caller-supplied, already-materialized membership view) and durably
    /// materializes one catalog record. Mirrors `apply_membership`: the operation and winning
    /// record commit atomically, and only a successful verification touches the database.
    pub fn apply_catalog(
        &self,
        record: CatalogRecord,
        provenance: RecordProvenance,
        project_id: &str,
        recovery_epoch: u64,
        membership: &MembershipView,
    ) -> Result<CatalogApply, StoreError> {
        let id = crate::catalog::record_id(&record)?;
        if self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM catalog_operations WHERE operation_id = ?1)",
            [&id],
            |row| row.get::<_, bool>(0),
        )? {
            return Ok(CatalogApply::Duplicate);
        }
        self.ensure_replication_capacity()?;
        let existing = self.catalog_operations()?;
        let mut view = CatalogView::from_records(
            existing
                .into_iter()
                .map(|record| (record, RecordProvenance::Verified)),
            project_id,
            recovery_epoch,
            membership,
        )?;
        let outcome = view.apply(
            record.clone(),
            provenance,
            project_id,
            recovery_epoch,
            membership,
        )?;
        let tx = self.conn.unchecked_transaction()?;
        let inserted = tx.execute(
            "INSERT INTO catalog_operations (operation_id, record_json, applied_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(operation_id) DO NOTHING",
            params![id, serde_json::to_string(&record)?, now()],
        )?;
        if inserted == 0 {
            tx.commit()?;
            return Ok(CatalogApply::Duplicate);
        }
        if outcome == CatalogApply::Applied {
            tx.execute(
                "INSERT INTO catalog_records
                     (deployment_id, replica_id, service, owner_node_id, owner_epoch, revision,
                      state, operation_id, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(deployment_id) DO UPDATE SET
                   replica_id=excluded.replica_id,
                   service=excluded.service,
                   owner_node_id=excluded.owner_node_id,
                   owner_epoch=excluded.owner_epoch,
                   revision=excluded.revision,
                   state=excluded.state,
                   operation_id=excluded.operation_id,
                   record_json=excluded.record_json",
                params![
                    record.deployment_id,
                    record.replica_id,
                    record.service,
                    record.owner_node_id,
                    record.owner_epoch,
                    record.revision,
                    match record.state {
                        crate::catalog::DeploymentState::Candidate => "candidate",
                        crate::catalog::DeploymentState::Active => "active",
                        crate::catalog::DeploymentState::Draining => "draining",
                        crate::catalog::DeploymentState::Stopped => "stopped",
                        crate::catalog::DeploymentState::Tombstoned => "tombstoned",
                    },
                    id,
                    serde_json::to_string(&record)?,
                ],
            )?;
        }
        tx.commit()?;
        Ok(outcome)
    }

    /// Returns one winner per deployment (including stopped and tombstoned ones), keyed by
    /// deployment rather than replica -- absence alone is never removal authority, matching
    /// `latest_membership`. Superseded history is not required for convergence because catalog
    /// ordering is monotonic per owner/deployment and the winner contains the complete record;
    /// this is also the snapshot `catalog_replication.rs`'s outbound exchange and
    /// `backup.rs`'s export both read from directly.
    pub fn latest_catalog(&self) -> Result<Vec<crate::catalog::CatalogRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT record_json FROM catalog_records ORDER BY replica_id, deployment_id",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(serde_json::from_str(&row?)?);
        }
        Ok(records)
    }

    pub fn desired_operations(&self) -> Result<Vec<DesiredStateRecord>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT record_json FROM desired_operations ORDER BY rowid")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(serde_json::from_str(&row?)?);
        }
        Ok(records)
    }

    /// Returns the winning desired-state record for each service.
    pub fn desired_snapshot_operations(&self) -> Result<Vec<DesiredStateRecord>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT record_json FROM desired_records ORDER BY service")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(serde_json::from_str(&row?)?);
        }
        Ok(records)
    }

    /// Deletes only superseded operation history. The winning operation IDs come from durable
    /// materialized tables in the same SQLite database and remain untouched, so compaction cannot
    /// erase a tombstone fence or change the reconstructed state.
    pub fn compact_operations(&self) -> Result<CompactionResult, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let membership_removed = tx.execute(
            "DELETE FROM membership_operations
             WHERE operation_id NOT IN (SELECT operation_id FROM membership_records)",
            [],
        )?;
        let catalog_removed = tx.execute(
            "DELETE FROM catalog_operations
             WHERE operation_id NOT IN (SELECT operation_id FROM catalog_records)",
            [],
        )?;
        let desired_removed = tx.execute(
            "DELETE FROM desired_operations
             WHERE operation_id NOT IN (SELECT operation_id FROM desired_records)",
            [],
        )?;
        tx.commit()?;
        Ok(CompactionResult {
            membership_removed,
            catalog_removed,
            desired_removed,
        })
    }

    pub fn apply_desired(
        &self,
        record: DesiredStateRecord,
        provenance: RecordProvenance,
        project_id: &str,
        recovery_epoch: u64,
        membership: &MembershipView,
    ) -> Result<DesiredApply, StoreError> {
        let id = crate::desired::record_id(&record)?;
        if self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM desired_operations WHERE operation_id = ?1)",
            [&id],
            |row| row.get::<_, bool>(0),
        )? {
            return Ok(DesiredApply::Duplicate);
        }
        self.ensure_replication_capacity()?;
        let mut view = DesiredStateView::from_records(
            self.desired_operations()?
                .into_iter()
                .map(|record| (record, RecordProvenance::Verified)),
            project_id,
            recovery_epoch,
            membership,
        )?;
        let outcome = view.apply(
            record.clone(),
            provenance,
            project_id,
            recovery_epoch,
            membership,
        )?;
        let tx = self.conn.unchecked_transaction()?;
        let inserted = tx.execute(
            "INSERT INTO desired_operations (operation_id, service, record_json, applied_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(operation_id) DO NOTHING",
            params![id, record.service, serde_json::to_string(&record)?, now()],
        )?;
        if inserted == 0 {
            tx.commit()?;
            return Ok(DesiredApply::Duplicate);
        }
        if outcome == DesiredApply::Applied {
            tx.execute(
                "INSERT INTO desired_records (service, revision, operation_id, record_json)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(service) DO UPDATE SET
                   revision=excluded.revision,
                   operation_id=excluded.operation_id,
                   record_json=excluded.record_json",
                params![
                    record.service,
                    record.revision,
                    id,
                    serde_json::to_string(&record)?,
                ],
            )?;
        }
        tx.commit()?;
        Ok(outcome)
    }

    pub fn desired_state(
        &self,
        service: &str,
    ) -> Result<Option<crate::desired::DesiredStateRecord>, StoreError> {
        let json = self
            .conn
            .query_row(
                "SELECT record_json FROM desired_records WHERE service = ?1",
                [service],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        json.map(|value| serde_json::from_str(&value).map_err(StoreError::from))
            .transpose()
    }

    /// Records that this node observed `node_id` as live just now -- either itself (always live)
    /// or a peer whose anti-entropy exchange just completed successfully. DNS treats a node's
    /// replicas as reachable only while its liveness stays within a bounded window (see `dns.rs`);
    /// this is a local, reversible eligibility overlay, never a deletion.
    pub fn mark_node_seen(&self, node_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO node_liveness (node_id, last_seen_at) VALUES (?1, ?2)
             ON CONFLICT(node_id) DO UPDATE SET last_seen_at = excluded.last_seen_at",
            params![node_id, now()],
        )?;
        Ok(())
    }

    /// `node_id -> last_seen_at` (Unix seconds as text, matching this store's other timestamp
    /// columns) for every node ever observed live.
    pub fn node_liveness(&self) -> Result<std::collections::BTreeMap<String, u64>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT node_id, last_seen_at FROM node_liveness")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut liveness = std::collections::BTreeMap::new();
        for row in rows {
            let (node_id, last_seen_at) = row?;
            liveness.insert(node_id, last_seen_at.parse().unwrap_or(0));
        }
        Ok(liveness)
    }

    /// Idempotent upsert keyed by `(service, cron_name)`, comparing `revision` and
    /// `canonical_hash` against whatever is already installed (see `cron.rs`'s `CronJobSpec` doc
    /// comment and the plan's "Agent API" section).
    pub fn apply_cron_spec(&self, spec: &CronJobSpec) -> Result<CronSpecApplyOutcome, StoreError> {
        let existing: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT revision, canonical_hash FROM cron_job_specs
                 WHERE service = ?1 AND cron_name = ?2",
                params![spec.service, spec.cron_name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let unchanged = existing.as_ref().is_some_and(|(revision, hash)| {
            *revision == spec.revision as i64 && *hash == spec.canonical_hash
        });
        self.conn.execute(
            "INSERT INTO cron_job_specs
                 (service, cron_name, revision, canonical_hash, spec_json, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(service, cron_name) DO UPDATE SET
               revision = excluded.revision,
               canonical_hash = excluded.canonical_hash,
               spec_json = excluded.spec_json,
               updated_at = excluded.updated_at",
            params![
                spec.service,
                spec.cron_name,
                spec.revision as i64,
                spec.canonical_hash,
                serde_json::to_string(spec)?,
                now(),
            ],
        )?;
        Ok(if unchanged {
            CronSpecApplyOutcome::Unchanged(spec.clone())
        } else if existing.is_some() {
            CronSpecApplyOutcome::Updated(spec.clone())
        } else {
            CronSpecApplyOutcome::Installed(spec.clone())
        })
    }

    pub fn remove_cron_spec(&self, service: &str, cron_name: &str) -> Result<bool, StoreError> {
        Ok(self.conn.execute(
            "DELETE FROM cron_job_specs WHERE service = ?1 AND cron_name = ?2",
            params![service, cron_name],
        )? == 1)
    }

    pub fn cron_spec(
        &self,
        service: &str,
        cron_name: &str,
    ) -> Result<Option<CronJobSpec>, StoreError> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT spec_json FROM cron_job_specs WHERE service = ?1 AND cron_name = ?2",
                params![service, cron_name],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|value| serde_json::from_str(&value).map_err(StoreError::from))
            .transpose()
    }

    pub fn cron_specs(&self) -> Result<Vec<CronJobSpec>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT spec_json FROM cron_job_specs ORDER BY service, cron_name")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?))
            .collect::<Result<_, StoreError>>()
    }

    /// Idempotent upsert keyed by `service`, returning the stored spec. There's no
    /// `canonical_hash` here (unlike `apply_cron_spec`) since `ImageRetentionSpec` has only two
    /// content fields, and no install/update classification either: the sole caller only needs
    /// the spec back for its response, and whether the row changed never alters what it does.
    pub fn apply_image_retention_spec(
        &self,
        spec: &ImageRetentionSpec,
    ) -> Result<ImageRetentionSpec, StoreError> {
        self.conn.execute(
            "INSERT INTO image_retention_specs (service, repo, retain, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(service) DO UPDATE SET
               repo = excluded.repo,
               retain = excluded.retain,
               updated_at = excluded.updated_at",
            params![spec.service, spec.repo, spec.retain as i64, now()],
        )?;
        Ok(spec.clone())
    }

    pub fn remove_image_retention_spec(&self, service: &str) -> Result<bool, StoreError> {
        Ok(self.conn.execute(
            "DELETE FROM image_retention_specs WHERE service = ?1",
            params![service],
        )? == 1)
    }

    pub fn image_retention_specs(&self) -> Result<Vec<ImageRetentionSpec>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT service, repo, retain FROM image_retention_specs ORDER BY service")?;
        let rows = statement.query_map([], |row| {
            Ok(ImageRetentionSpec {
                service: row.get(0)?,
                repo: row.get(1)?,
                retain: row.get::<_, i64>(2)? as u32,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(StoreError::from)
    }

    /// Transactionally claims a due (scheduled) or requested (manual) cron run (see the plan's
    /// "Scheduler Rules" section). Checks an exact repeat of the same `(service, cron_name,
    /// scheduled_at)` first -- unconditionally returning the existing run, regardless of its own
    /// state -- before the general `overlap: forbid` check, so retrying the identical scheduled
    /// claim while its own run is still active is never mistaken for a different run blocking it.
    #[allow(clippy::too_many_arguments)]
    pub fn claim_cron_run(
        &self,
        project: &str,
        service: &str,
        cron_name: &str,
        cause: CronRunCause,
        scheduled_at: Option<u64>,
        run_id: &str,
        timestamp: u64,
    ) -> Result<CronClaimOutcome, StoreError> {
        let tx = self.conn.unchecked_transaction()?;

        if let Some(scheduled_at_value) = scheduled_at {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT run_json FROM cron_runs
                     WHERE service = ?1 AND cron_name = ?2 AND scheduled_at = ?3",
                    params![service, cron_name, scheduled_at_value as i64],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(existing_json) = existing {
                tx.commit()?;
                return Ok(CronClaimOutcome::DuplicateScheduledClaim(
                    serde_json::from_str(&existing_json)?,
                ));
            }
        }

        let active: Option<String> = tx
            .query_row(
                "SELECT run_id FROM cron_runs
                 WHERE service = ?1 AND cron_name = ?2 AND state IN ('claimed', 'running')
                 LIMIT 1",
                params![service, cron_name],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(active_run_id) = active {
            let default_state = serde_json::to_string(&SchedulerStateJson {
                last_evaluated_at: None,
                next_due_at: None,
            })?;
            tx.execute(
                "INSERT INTO cron_scheduler_state
                     (service, cron_name, skipped_overlap_count, state_json, updated_at)
                 VALUES (?1, ?2, 1, ?3, ?4)
                 ON CONFLICT(service, cron_name) DO UPDATE SET
                   skipped_overlap_count = skipped_overlap_count + 1,
                   updated_at = excluded.updated_at",
                params![service, cron_name, default_state, now()],
            )?;
            tx.commit()?;
            return Ok(CronClaimOutcome::OverlapForbidden { active_run_id });
        }

        let run = CronRun {
            run_id: run_id.to_string(),
            project: project.to_string(),
            service: service.to_string(),
            cron_name: cron_name.to_string(),
            cause,
            scheduled_at,
            claimed_at: timestamp,
            started_at: None,
            finished_at: None,
            state: CronRunState::Claimed,
            deployment_id: None,
            container_name: None,
            address: None,
            exit_code: None,
            error: None,
        };
        tx.execute(
            "INSERT INTO cron_runs
                 (run_id, service, cron_name, cause, scheduled_at, claimed_at, state, run_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'claimed', ?7)",
            params![
                run.run_id,
                run.service,
                run.cron_name,
                match cause {
                    CronRunCause::Scheduled => "scheduled",
                    CronRunCause::Manual => "manual",
                },
                scheduled_at.map(|value| value as i64),
                timestamp as i64,
                serde_json::to_string(&run)?,
            ],
        )?;
        tx.commit()?;
        Ok(CronClaimOutcome::Claimed(run))
    }

    pub fn cron_run(&self, run_id: &str) -> Result<Option<CronRun>, StoreError> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT run_json FROM cron_runs WHERE run_id = ?1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|value| serde_json::from_str(&value).map_err(StoreError::from))
            .transpose()
    }

    pub fn active_cron_run(
        &self,
        service: &str,
        cron_name: &str,
    ) -> Result<Option<CronRun>, StoreError> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT run_json FROM cron_runs
                 WHERE service = ?1 AND cron_name = ?2 AND state IN ('claimed', 'running')
                 LIMIT 1",
                params![service, cron_name],
                |row| row.get(0),
            )
            .optional()?;
        json.map(|value| serde_json::from_str(&value).map_err(StoreError::from))
            .transpose()
    }

    pub fn cron_runs(&self, filter: &CronRunFilter) -> Result<Vec<CronRun>, StoreError> {
        let limit = filter.limit.map_or(i64::MAX, i64::from);
        let since = filter.since.map(|value| value as i64);
        let mut statement = self.conn.prepare(
            "SELECT run_json FROM cron_runs
             WHERE (?1 IS NULL OR service = ?1)
               AND (?2 IS NULL OR cron_name = ?2)
               AND (?3 IS NULL OR run_id = ?3)
               AND (?4 IS NULL OR claimed_at >= ?4)
             ORDER BY claimed_at DESC
             LIMIT ?5",
        )?;
        let rows = statement.query_map(
            params![
                filter.service,
                filter.cron_name,
                filter.run_id,
                since,
                limit
            ],
            |row| row.get::<_, String>(0),
        )?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?))
            .collect::<Result<_, StoreError>>()
    }

    /// Moves a claimed run to `running` once its container actually starts. Returns `false`
    /// (rather than an error) for an unknown `run_id`: a caller bug, not a storage failure.
    pub fn start_cron_run(
        &self,
        run_id: &str,
        started_at: u64,
        deployment_id: &str,
        container_name: &str,
        address: &str,
    ) -> Result<bool, StoreError> {
        let Some(mut run) = self.cron_run(run_id)? else {
            return Ok(false);
        };
        run.state = CronRunState::Running;
        run.started_at = Some(started_at);
        run.deployment_id = Some(deployment_id.to_string());
        run.container_name = Some(container_name.to_string());
        run.address = Some(address.to_string());
        self.conn.execute(
            "UPDATE cron_runs SET state = 'running', run_json = ?2 WHERE run_id = ?1",
            params![run_id, serde_json::to_string(&run)?],
        )?;
        Ok(true)
    }

    /// Moves a run to a terminal state (`succeeded`/`failed`/`timed_out`/`skipped`). Returns
    /// `false` for an unknown `run_id`, as `start_cron_run` does.
    pub fn finish_cron_run(
        &self,
        run_id: &str,
        state: CronRunState,
        finished_at: u64,
        exit_code: Option<i32>,
        error: Option<String>,
    ) -> Result<bool, StoreError> {
        let Some(mut run) = self.cron_run(run_id)? else {
            return Ok(false);
        };
        run.state = state;
        run.finished_at = Some(finished_at);
        run.exit_code = exit_code;
        run.error = error;
        let state_text = match state {
            CronRunState::Claimed => "claimed",
            CronRunState::Running => "running",
            CronRunState::Succeeded => "succeeded",
            CronRunState::Failed => "failed",
            CronRunState::TimedOut => "timed_out",
            CronRunState::Skipped => "skipped",
        };
        self.conn.execute(
            "UPDATE cron_runs SET state = ?2, run_json = ?3 WHERE run_id = ?1",
            params![run_id, state_text, serde_json::to_string(&run)?],
        )?;
        Ok(true)
    }

    pub fn cron_scheduler_state(
        &self,
        service: &str,
        cron_name: &str,
    ) -> Result<Option<CronSchedulerState>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT service, cron_name, skipped_overlap_count, state_json
                 FROM cron_scheduler_state WHERE service = ?1 AND cron_name = ?2",
                params![service, cron_name],
                scheduler_state_from_row,
            )
            .optional()?)
    }

    /// Preserves `skipped_overlap_count` (bumped separately, by `claim_cron_run`'s overlap
    /// path); this only ever touches `last_evaluated_at`/`next_due_at`.
    pub fn set_cron_scheduler_state(
        &self,
        service: &str,
        cron_name: &str,
        last_evaluated_at: Option<u64>,
        next_due_at: Option<u64>,
    ) -> Result<(), StoreError> {
        let state_json = serde_json::to_string(&SchedulerStateJson {
            last_evaluated_at,
            next_due_at,
        })?;
        self.conn.execute(
            "INSERT INTO cron_scheduler_state
                 (service, cron_name, skipped_overlap_count, state_json, updated_at)
             VALUES (?1, ?2, 0, ?3, ?4)
             ON CONFLICT(service, cron_name) DO UPDATE SET
               state_json = excluded.state_json,
               updated_at = excluded.updated_at",
            params![service, cron_name, state_json, now()],
        )?;
        Ok(())
    }

    /// Applies the plan's "Durable Storage" retention: a completed run older than `keep_seconds`
    /// is removed unless it is among each job's latest `keep_latest` runs (by `claimed_at`),
    /// which are kept regardless of age. An active (`claimed`/`running`) run is never removed.
    pub fn retain_cron_runs(
        &self,
        now_ts: u64,
        keep_seconds: u64,
        keep_latest: u32,
    ) -> Result<usize, StoreError> {
        let cutoff = now_ts.saturating_sub(keep_seconds) as i64;
        Ok(self.conn.execute(
            "DELETE FROM cron_runs
             WHERE state IN ('succeeded', 'failed', 'timed_out', 'skipped')
               AND claimed_at < ?1
               AND run_id IN (
                   SELECT run_id FROM (
                       SELECT run_id,
                              ROW_NUMBER() OVER (
                                  PARTITION BY service, cron_name ORDER BY claimed_at DESC
                              ) AS rn
                       FROM cron_runs
                       WHERE state IN ('succeeded', 'failed', 'timed_out', 'skipped')
                   )
                   WHERE rn > ?2
               )",
            params![cutoff, keep_latest],
        )?)
    }
}

/// The part of `CronSchedulerState` not already covered by `cron_scheduler_state`'s own indexed
/// `skipped_overlap_count` column.
#[derive(Serialize, Deserialize)]
struct SchedulerStateJson {
    last_evaluated_at: Option<u64>,
    next_due_at: Option<u64>,
}

fn scheduler_state_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CronSchedulerState> {
    let service: String = row.get(0)?;
    let cron_name: String = row.get(1)?;
    let skipped_overlap_count: i64 = row.get(2)?;
    let state_json: String = row.get(3)?;
    let parsed: SchedulerStateJson = serde_json::from_str(&state_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(CronSchedulerState {
        service,
        cron_name,
        last_evaluated_at: parsed.last_evaluated_at,
        next_due_at: parsed.next_due_at,
        skipped_overlap_count: skipped_overlap_count.max(0) as u64,
    })
}

fn check_integrity(conn: &Connection, path: &Path) -> Result<(), StoreError> {
    let result: Result<String, rusqlite::Error> =
        conn.query_row("PRAGMA integrity_check", [], |row| row.get(0));
    match result {
        Ok(status) if status == "ok" => Ok(()),
        Ok(status) => Err(StoreError::Corrupted {
            path: path.display().to_string(),
            detail: status,
        }),
        Err(error) => Err(StoreError::Corrupted {
            path: path.display().to_string(),
            detail: error.to_string(),
        }),
    }
}

/// Best-effort consistent snapshot before mutating schema: checkpoints the WAL back into the main
/// file first so the plain-file copy taken afterward is complete, per ADR 0001 ("Migrations are
/// ... backed up before execution").
fn backup_before_migration(db_path: &Path, from_version: i64) -> Result<PathBuf, StoreError> {
    {
        let conn = Connection::open(db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .ok();
    }
    let backup_path = db_path.with_extension(format!("pre-migration-v{from_version}.sqlite3"));
    fs::copy(db_path, &backup_path)?;
    Ok(backup_path)
}

fn apply_migration(conn: &Connection, migration: &Migration) -> Result<(), StoreError> {
    conn.execute_batch("BEGIN IMMEDIATE;")
        .map_err(|error| StoreError::MigrationFailed {
            version: migration.version,
            detail: error.to_string(),
        })?;
    let result = (|| -> Result<(), rusqlite::Error> {
        conn.execute_batch(migration.sql)?;
        conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![migration.version, now()],
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT;")
                .map_err(|error| StoreError::MigrationFailed {
                    version: migration.version,
                    detail: error.to_string(),
                })?;
            Ok(())
        }
        Err(error) => {
            conn.execute_batch("ROLLBACK;").ok();
            Err(StoreError::MigrationFailed {
                version: migration.version,
                detail: error.to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::membership::{
        MembershipRecord, MembershipScope, MembershipState, MEMBERSHIP_PROTOCOL_VERSION,
        MEMBERSHIP_SCHEMA_VERSION,
    };
    use std::net::Ipv4Addr;
    use tempfile::tempdir;

    #[test]
    fn fresh_store_is_migrated_to_the_current_schema_version() {
        let dir = tempdir().unwrap();
        let store = AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap();
        assert_eq!(store.schema_version().unwrap(), current_schema_version());
    }

    #[test]
    fn reopening_an_existing_store_preserves_data_and_does_not_remigrate() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("agent.sqlite3");
        {
            let store = AgentStore::open(&db_path).unwrap();
            store.commit_local_transaction("k", "v1").unwrap();
        }
        let store = AgentStore::open(&db_path).unwrap();
        assert_eq!(
            store.read_local_state("k").unwrap(),
            Some(("v1".to_string(), 1))
        );
        assert_eq!(store.schema_version().unwrap(), current_schema_version());
    }

    #[test]
    fn corrupt_file_is_refused_without_being_overwritten() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("agent.sqlite3");
        fs::write(&db_path, b"this is not a sqlite database").unwrap();
        let original = fs::read(&db_path).unwrap();

        let error = AgentStore::open(&db_path).unwrap_err();
        assert!(matches!(error, StoreError::Corrupted { .. }));
        assert_eq!(fs::read(&db_path).unwrap(), original);
    }

    #[test]
    fn a_schema_from_a_newer_binary_is_refused_without_being_touched() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("agent.sqlite3");
        {
            let store = AgentStore::open(&db_path).unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![current_schema_version() + 1, now()],
                )
                .unwrap();
        }

        let error = AgentStore::open(&db_path).unwrap_err();
        match error {
            StoreError::UnsupportedSchema { found, supported } => {
                assert_eq!(found, current_schema_version() + 1);
                assert_eq!(supported, current_schema_version());
            }
            other => panic!("expected UnsupportedSchema, got {other:?}"),
        }
    }

    #[test]
    fn migration_takes_a_backup_of_pre_existing_data() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("agent.sqlite3");
        {
            let store = AgentStore::open(&db_path).unwrap();
            store.commit_local_transaction("k", "v").unwrap();
        }

        let backup_path = backup_before_migration(&db_path, current_schema_version()).unwrap();
        assert!(backup_path.exists());

        let backup_conn = Connection::open(&backup_path).unwrap();
        let value: String = backup_conn
            .query_row("SELECT value FROM local_state WHERE key = 'k'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(value, "v");
    }

    #[test]
    fn observations_round_trip_and_stale_entries_are_pruned() {
        let dir = tempdir().unwrap();
        let store = AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap();
        store
            .upsert_observation(&Observation {
                container_id: "c1".into(),
                name: "demo-a".into(),
                image: "nginx:alpine".into(),
                labels_json: "{}".into(),
                state: "running".into(),
            })
            .unwrap();
        store
            .upsert_observation(&Observation {
                container_id: "c2".into(),
                name: "demo-b".into(),
                image: "nginx:alpine".into(),
                labels_json: "{}".into(),
                state: "running".into(),
            })
            .unwrap();
        assert_eq!(store.observation_count().unwrap(), 2);

        let removed = store.retain_observations(&["c1".to_string()]).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(store.observation_count().unwrap(), 1);
        assert_eq!(store.list_observations().unwrap()[0].container_id, "c1");
    }

    #[test]
    fn idempotency_keys_are_stored_once() {
        let dir = tempdir().unwrap();
        let store = AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap();
        assert_eq!(store.idempotent_get("req-1").unwrap(), None);
        store.idempotent_put("req-1", "{\"ok\":true}").unwrap();
        store.idempotent_put("req-1", "{\"ok\":false}").unwrap();
        assert_eq!(
            store.idempotent_get("req-1").unwrap(),
            Some("{\"ok\":true}".to_string())
        );
    }

    fn member_record(node: &str, revision: u64) -> (MembershipScope, MembershipRecord) {
        let scope = MembershipScope::new("project", 1);
        let record = MembershipRecord {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            node_id: node.into(),
            server_name: node.into(),
            wireguard_public_key: format!("wg-{node}"),
            management_address: Ipv4Addr::new(100, 98, 64, 2),
            container_subnet: "198.18.2.0/24".into(),
            endpoints: vec!["192.0.2.2:51820".parse().unwrap()],
            owner_epoch: 1,
            revision,
            state: MembershipState::Active,
        };
        (scope, record)
    }

    #[test]
    fn version_nine_removes_legacy_signature_columns_and_preserves_operations() {
        // Pending migrations are computed from `MAX(version)` in `schema_migrations`, not from
        // which individual rows are present -- so forcing migration 9 to re-run means rolling
        // back every later migration's row (and undoing its effect) too, or `MAX` never drops
        // back below 9 and nothing re-applies. Update the `DROP TABLE`/`version >=` cutoff here
        // whenever a new migration lands after this one.
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("agent.sqlite3");
        let (scope, first) = member_record("node-a", 1);
        {
            let store = AgentStore::open(&db_path).unwrap();
            store.apply_membership(first.clone(), &scope).unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO catalog_operations
                         (operation_id, record_json, applied_at)
                     VALUES ('legacy-catalog', '{}', '1')",
                    [],
                )
                .unwrap();
            store
                .conn
                .execute(
                    "INSERT INTO desired_operations
                         (operation_id, service, record_json, applied_at)
                     VALUES ('legacy-desired', 'web', '{}', '1')",
                    [],
                )
                .unwrap();
            store
                .conn
                .execute_batch(
                    "ALTER TABLE membership_operations RENAME TO membership_operations_current;
                     CREATE TABLE membership_operations (
                         operation_id TEXT PRIMARY KEY,
                         record_json TEXT NOT NULL,
                         signer_id TEXT NOT NULL,
                         signature BLOB NOT NULL,
                         applied_at TEXT NOT NULL
                     );
                     INSERT INTO membership_operations
                         (operation_id, record_json, signer_id, signature, applied_at)
                         SELECT operation_id, record_json, 'legacy', X'00', applied_at
                         FROM membership_operations_current;
                     DROP TABLE membership_operations_current;

                     ALTER TABLE catalog_operations RENAME TO catalog_operations_current;
                     CREATE TABLE catalog_operations (
                         operation_id TEXT PRIMARY KEY,
                         record_json TEXT NOT NULL,
                         signer_id TEXT NOT NULL,
                         signature BLOB NOT NULL,
                         applied_at TEXT NOT NULL
                     );
                     INSERT INTO catalog_operations
                         (operation_id, record_json, signer_id, signature, applied_at)
                         SELECT operation_id, record_json, 'legacy', X'00', applied_at
                         FROM catalog_operations_current;
                     DROP TABLE catalog_operations_current;

                     ALTER TABLE desired_operations RENAME TO desired_operations_current;
                     CREATE TABLE desired_operations (
                         operation_id TEXT PRIMARY KEY,
                         service TEXT NOT NULL,
                         record_json TEXT NOT NULL,
                         signer_id TEXT NOT NULL,
                         signature BLOB NOT NULL,
                         applied_at TEXT NOT NULL
                     );
                     INSERT INTO desired_operations
                         (operation_id, service, record_json, signer_id, signature, applied_at)
                         SELECT operation_id, service, record_json, 'legacy', X'00', applied_at
                         FROM desired_operations_current;
                     DROP TABLE desired_operations_current;
                     DROP TABLE image_retention_specs;
                     CREATE TABLE scale_overrides (
                         service TEXT PRIMARY KEY,
                         replicas INTEGER NOT NULL,
                         revision INTEGER NOT NULL,
                         updated_at TEXT NOT NULL
                     );
                     CREATE TABLE replica_assignments (
                         replica_id TEXT PRIMARY KEY,
                         service TEXT NOT NULL,
                         ordinal INTEGER NOT NULL,
                         owner_node_id TEXT NOT NULL,
                         owner_epoch INTEGER NOT NULL,
                         state TEXT NOT NULL,
                         revision INTEGER NOT NULL,
                         updated_at TEXT NOT NULL,
                         UNIQUE(service, ordinal)
                     );
                     DELETE FROM schema_migrations WHERE version >= 9;",
                )
                .unwrap();
        }

        let store = AgentStore::open(&db_path).unwrap();
        assert_eq!(store.schema_version().unwrap(), current_schema_version());
        assert_eq!(store.membership_operations().unwrap(), vec![first]);

        for table in [
            "membership_operations",
            "catalog_operations",
            "desired_operations",
        ] {
            let mut statement = store
                .conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            let columns = statement
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert!(!columns.contains(&"signer_id".to_string()));
            assert!(!columns.contains(&"signature".to_string()));
        }

        let catalog_count: u64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM catalog_operations", [], |row| {
                row.get(0)
            })
            .unwrap();
        let desired_count: u64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM desired_operations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(catalog_count, 1);
        assert_eq!(desired_count, 1);

        let (_, second) = member_record("node-a", 2);
        assert_eq!(
            store.apply_membership(second, &scope).unwrap(),
            MembershipApply::Applied
        );
    }

    #[test]
    fn membership_write_is_atomic_idempotent_and_survives_restart() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("agent.sqlite3");
        let (scope, record) = member_record("node-a", 1);
        {
            let store = AgentStore::open(&db_path).unwrap();
            assert_eq!(
                store.apply_membership(record.clone(), &scope).unwrap(),
                MembershipApply::Applied
            );
            assert_eq!(
                store.apply_membership(record.clone(), &scope).unwrap(),
                MembershipApply::Duplicate
            );
        }
        let store = AgentStore::open(&db_path).unwrap();
        assert_eq!(store.active_membership().unwrap().len(), 1);
        assert_eq!(store.membership_operations().unwrap(), vec![record]);
    }

    #[test]
    fn compaction_keeps_the_winner_and_removes_only_superseded_history() {
        let dir = tempdir().unwrap();
        let store = AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap();
        let (scope, first) = member_record("node-a", 1);
        let (_, second) = member_record("node-a", 2);
        store.apply_membership(first, &scope).unwrap();
        store.apply_membership(second.clone(), &scope).unwrap();
        assert_eq!(store.membership_operations().unwrap().len(), 2);

        let result = store.compact_operations().unwrap();
        assert_eq!(result.membership_removed, 1);
        assert_eq!(store.latest_membership().unwrap(), vec![second.clone()]);
        assert_eq!(store.membership_operations().unwrap(), vec![second]);
        assert_eq!(store.latest_membership().unwrap()[0].revision, 2);
    }

    #[test]
    fn compaction_never_collects_a_winning_tombstone_fence() {
        let dir = tempdir().unwrap();
        let store = AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap();
        let (scope, active) = member_record("node-a", 1);
        store.apply_membership(active.clone(), &scope).unwrap();
        let mut tombstone = active;
        tombstone.revision = 2;
        tombstone.state = MembershipState::Tombstoned;
        store.apply_membership(tombstone.clone(), &scope).unwrap();
        store.compact_operations().unwrap();

        assert_eq!(store.latest_membership().unwrap(), vec![tombstone.clone()]);
        assert!(store.active_membership().unwrap().is_empty());
        assert_eq!(
            store.latest_membership().unwrap()[0].state,
            MembershipState::Tombstoned
        );
    }

    #[test]
    fn soft_quota_refuses_new_operations_but_keeps_reads_and_duplicates_available() {
        let dir = tempdir().unwrap();
        let store = AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap();
        let (scope, first) = member_record("node-a", 1);
        store.apply_membership(first.clone(), &scope).unwrap();
        store.set_soft_quota_bytes(Some(1));

        assert_eq!(
            store.apply_membership(first, &scope).unwrap(),
            MembershipApply::Duplicate
        );
        let (_, second) = member_record("node-a", 2);
        assert!(matches!(
            store.apply_membership(second, &scope),
            Err(StoreError::QuotaExceeded { .. })
        ));
        assert_eq!(store.latest_membership().unwrap()[0].revision, 1);
        assert!(store.database_usage_bytes().unwrap() > 1);
    }

    #[test]
    fn peer_sync_failures_are_durable_and_success_resets_them() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("agent.sqlite3");
        {
            let store = AgentStore::open(&db_path).unwrap();
            store
                .record_peer_sync_failure("node-b", "connection refused")
                .unwrap();
            store
                .record_peer_sync_failure("node-b", "timed out")
                .unwrap();
            assert_eq!(
                store.peer_sync_statuses().unwrap()[0].consecutive_failures,
                2
            );
        }
        let store = AgentStore::open(&db_path).unwrap();
        store.record_peer_sync_success("node-b").unwrap();
        let status = &store.peer_sync_statuses().unwrap()[0];
        assert_eq!(status.consecutive_failures, 0);
        assert_eq!(status.last_error, None);
        assert!(status.last_success_at.is_some());
    }

    #[test]
    fn peer_cache_round_trips_for_cold_start_before_replication() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("agent.sqlite3");
        let cache = std::collections::BTreeMap::from([(
            "node-a".into(),
            PeerCacheEntry {
                node_id: "node-a".into(),
                wireguard_public_key: "wg-a".into(),
                management_address: "100.98.64.2".into(),
                container_subnet: "198.18.2.0/24".into(),
                endpoint: "192.0.2.2:51820".parse().unwrap(),
                owner_epoch: 1,
                revision: 1,
            },
        )]);
        {
            let store = AgentStore::open(&db_path).unwrap();
            store.replace_peer_cache(&cache).unwrap();
        }
        let store = AgentStore::open(&db_path).unwrap();
        assert_eq!(store.peer_cache().unwrap(), cache);
    }

    fn membership_view_for(node_id: &str) -> MembershipView {
        let scope = MembershipScope::new("project", 1);
        let record = MembershipRecord {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            node_id: node_id.into(),
            server_name: node_id.into(),
            wireguard_public_key: format!("wg-{node_id}"),
            management_address: Ipv4Addr::new(100, 98, 64, 3),
            container_subnet: "198.18.3.0/24".into(),
            endpoints: vec!["192.0.2.3:51820".parse().unwrap()],
            owner_epoch: 1,
            revision: 1,
            state: MembershipState::Active,
        };
        let mut view = MembershipView::default();
        view.apply(record, &scope).unwrap();
        view
    }

    fn catalog_record(replica: &str, node_id: &str, revision: u64) -> CatalogRecord {
        use crate::catalog::{DeploymentState, HealthState};
        CatalogRecord {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: crate::catalog::CATALOG_PROTOCOL_VERSION,
            schema_version: crate::catalog::CATALOG_SCHEMA_VERSION,
            service: "web".into(),
            replica_id: replica.into(),
            owner_node_id: node_id.into(),
            owner_epoch: 1,
            revision,
            deployment_id: "deploy-r1".into(),
            address: "198.18.3.10".parse().unwrap(),
            ports: vec![80],
            image: "nginx:alpine".into(),
            state: DeploymentState::Active,
            health: HealthState::Healthy,
        }
    }

    #[test]
    fn catalog_write_is_atomic_idempotent_and_survives_restart() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("agent.sqlite3");
        let membership = membership_view_for("node-a");
        let record = catalog_record("r1", "node-a", 1);
        {
            let store = AgentStore::open(&db_path).unwrap();
            assert_eq!(
                store
                    .apply_catalog(
                        record.clone(),
                        RecordProvenance::Local,
                        "project",
                        1,
                        &membership
                    )
                    .unwrap(),
                CatalogApply::Applied
            );
            assert_eq!(
                store
                    .apply_catalog(
                        record.clone(),
                        RecordProvenance::Local,
                        "project",
                        1,
                        &membership
                    )
                    .unwrap(),
                CatalogApply::Duplicate
            );
        }
        let store = AgentStore::open(&db_path).unwrap();
        assert_eq!(store.latest_catalog().unwrap().len(), 1);
        assert_eq!(store.catalog_operations().unwrap(), vec![record]);
    }

    #[test]
    fn node_liveness_round_trips_and_updates_on_reobservation() {
        let dir = tempdir().unwrap();
        let store = AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap();
        assert!(store.node_liveness().unwrap().is_empty());
        store.mark_node_seen("node-a").unwrap();
        let liveness = store.node_liveness().unwrap();
        assert_eq!(liveness.len(), 1);
        assert!(liveness.contains_key("node-a"));
        // Re-marking the same node updates rather than duplicating.
        store.mark_node_seen("node-a").unwrap();
        assert_eq!(store.node_liveness().unwrap().len(), 1);
    }
}

#[cfg(test)]
mod cron_tests {
    use super::*;
    use crate::cron::{CronMissedRuns, CronOverlap};
    use tempfile::tempdir;

    fn store() -> AgentStore {
        let dir = tempdir().unwrap();
        // Leak the tempdir so it outlives the store within a single test; each test gets its own
        // directory regardless, since `tempdir()` is called fresh per call.
        let path = dir.keep().join("agent.sqlite3");
        AgentStore::open(&path).unwrap()
    }

    fn spec(service: &str, cron_name: &str, revision: u64) -> CronJobSpec {
        let mut spec = CronJobSpec {
            project: "demo".into(),
            service: service.into(),
            cron_name: cron_name.into(),
            revision,
            canonical_hash: String::new(),
            owner_node_id: "node-a".into(),
            owner_epoch: 1,
            server: "node-a".into(),
            source_deployment_id: "dep-a".into(),
            source_replica_id: "replica-a".into(),
            image: "ghcr.io/example/twitch-sync:latest".into(),
            schedule: "7 */2 * * *".into(),
            timezone: "UTC".into(),
            timeout_seconds: 3600,
            overlap: CronOverlap::Forbid,
            missed_runs: CronMissedRuns::Skip,
            command: vec!["npm".into(), "run".into(), "sync:twitch".into()],
            env_file_path: "/var/lib/jiji/demo/env/twitch".into(),
            mount_args: vec![],
            resource_args: vec![],
            bridge_network: "jiji-demo".into(),
            dns_address: "100.64.0.5".into(),
        };
        spec.canonical_hash = spec.canonical_hash();
        spec
    }

    #[test]
    fn apply_cron_spec_reports_installed_then_unchanged_then_updated() {
        let store = store();
        let spec = spec("twitch", "sync-twitch", 1);

        assert_eq!(
            store.apply_cron_spec(&spec).unwrap(),
            CronSpecApplyOutcome::Installed(spec.clone())
        );
        assert_eq!(
            store.apply_cron_spec(&spec).unwrap(),
            CronSpecApplyOutcome::Unchanged(spec.clone())
        );

        let mut changed = spec.clone();
        changed.revision = 2;
        changed.schedule = "0 3 * * *".into();
        changed.canonical_hash = changed.canonical_hash();
        assert_eq!(
            store.apply_cron_spec(&changed).unwrap(),
            CronSpecApplyOutcome::Updated(changed.clone())
        );
        assert_eq!(
            store.cron_spec("twitch", "sync-twitch").unwrap(),
            Some(changed)
        );
    }

    #[test]
    fn cron_specs_lists_every_installed_spec_sorted() {
        let store = store();
        store
            .apply_cron_spec(&spec("twitch", "sync-twitch", 1))
            .unwrap();
        store
            .apply_cron_spec(&spec("backups", "nightly", 1))
            .unwrap();
        let names: Vec<(String, String)> = store
            .cron_specs()
            .unwrap()
            .into_iter()
            .map(|spec| (spec.service, spec.cron_name))
            .collect();
        assert_eq!(
            names,
            vec![
                ("backups".to_string(), "nightly".to_string()),
                ("twitch".to_string(), "sync-twitch".to_string()),
            ]
        );
    }

    #[test]
    fn remove_cron_spec_deletes_an_installed_spec_and_is_idempotent() {
        let store = store();
        store
            .apply_cron_spec(&spec("twitch", "sync-twitch", 1))
            .unwrap();
        assert!(store.remove_cron_spec("twitch", "sync-twitch").unwrap());
        assert!(store.cron_spec("twitch", "sync-twitch").unwrap().is_none());
        assert!(!store.remove_cron_spec("twitch", "sync-twitch").unwrap());
    }

    fn retention_spec(service: &str, repo: &str, retain: u32) -> ImageRetentionSpec {
        ImageRetentionSpec {
            service: service.into(),
            repo: repo.into(),
            retain,
        }
    }

    #[test]
    fn apply_image_retention_spec_upserts_idempotently() {
        let store = store();
        let spec = retention_spec("web", "ghcr.io/example/demo-web", 3);

        assert_eq!(store.apply_image_retention_spec(&spec).unwrap(), spec);
        assert_eq!(store.apply_image_retention_spec(&spec).unwrap(), spec);

        let changed = retention_spec("web", "ghcr.io/example/demo-web", 5);
        assert_eq!(store.apply_image_retention_spec(&changed).unwrap(), changed);
        assert_eq!(store.image_retention_specs().unwrap(), vec![changed]);
    }

    #[test]
    fn image_retention_specs_lists_every_installed_spec_sorted() {
        let store = store();
        store
            .apply_image_retention_spec(&retention_spec("web", "ghcr.io/example/demo-web", 3))
            .unwrap();
        store
            .apply_image_retention_spec(&retention_spec(
                "backend",
                "ghcr.io/example/demo-backend",
                3,
            ))
            .unwrap();
        let names: Vec<String> = store
            .image_retention_specs()
            .unwrap()
            .into_iter()
            .map(|spec| spec.service)
            .collect();
        assert_eq!(names, vec!["backend".to_string(), "web".to_string()]);
    }

    #[test]
    fn remove_image_retention_spec_deletes_an_installed_spec_and_is_idempotent() {
        let store = store();
        store
            .apply_image_retention_spec(&retention_spec("web", "ghcr.io/example/demo-web", 3))
            .unwrap();
        assert!(store.remove_image_retention_spec("web").unwrap());
        assert!(store.image_retention_specs().unwrap().is_empty());
        assert!(!store.remove_image_retention_spec("web").unwrap());
    }

    #[test]
    fn claim_cron_run_claims_a_fresh_manual_run() {
        let store = store();
        let outcome = store
            .claim_cron_run(
                "demo",
                "twitch",
                "sync-twitch",
                CronRunCause::Manual,
                None,
                "run-1",
                100,
            )
            .unwrap();
        let CronClaimOutcome::Claimed(run) = outcome else {
            panic!("expected Claimed, got {outcome:?}");
        };
        assert_eq!(run.run_id, "run-1");
        assert_eq!(run.cause, CronRunCause::Manual);
        assert_eq!(run.scheduled_at, None);
        assert_eq!(run.state, CronRunState::Claimed);
        assert_eq!(
            store.active_cron_run("twitch", "sync-twitch").unwrap(),
            Some(run)
        );
    }

    #[test]
    fn claim_cron_run_forbids_overlap_while_a_run_is_active_and_counts_the_skip() {
        let store = store();
        store
            .claim_cron_run(
                "demo",
                "twitch",
                "sync-twitch",
                CronRunCause::Manual,
                None,
                "run-1",
                100,
            )
            .unwrap();

        let outcome = store
            .claim_cron_run(
                "demo",
                "twitch",
                "sync-twitch",
                CronRunCause::Scheduled,
                Some(200),
                "run-2",
                200,
            )
            .unwrap();
        assert_eq!(
            outcome,
            CronClaimOutcome::OverlapForbidden {
                active_run_id: "run-1".to_string()
            }
        );
        assert_eq!(
            store
                .cron_scheduler_state("twitch", "sync-twitch")
                .unwrap()
                .unwrap()
                .skipped_overlap_count,
            1
        );
        // The forbidden claim must never have inserted a row for run-2.
        assert!(store.cron_run("run-2").unwrap().is_none());
    }

    #[test]
    fn claim_cron_run_replays_a_duplicate_scheduled_claim_without_starting_a_new_run() {
        let store = store();
        let first = store
            .claim_cron_run(
                "demo",
                "twitch",
                "sync-twitch",
                CronRunCause::Scheduled,
                Some(100),
                "run-1",
                100,
            )
            .unwrap();
        let CronClaimOutcome::Claimed(first_run) = first else {
            panic!("expected Claimed");
        };

        // A retried claim of the exact same scheduled time, even while that run is still
        // active, must return the existing run rather than treating it as an overlap.
        let replay = store
            .claim_cron_run(
                "demo",
                "twitch",
                "sync-twitch",
                CronRunCause::Scheduled,
                Some(100),
                "run-2",
                101,
            )
            .unwrap();
        assert_eq!(replay, CronClaimOutcome::DuplicateScheduledClaim(first_run));
        assert!(store.cron_run("run-2").unwrap().is_none());
    }

    #[test]
    fn different_scheduled_times_claim_independently_once_the_prior_run_finishes() {
        let store = store();
        let CronClaimOutcome::Claimed(first) = store
            .claim_cron_run(
                "demo",
                "twitch",
                "sync-twitch",
                CronRunCause::Scheduled,
                Some(100),
                "run-1",
                100,
            )
            .unwrap()
        else {
            panic!("expected Claimed");
        };
        store
            .finish_cron_run(&first.run_id, CronRunState::Succeeded, 150, Some(0), None)
            .unwrap();

        let outcome = store
            .claim_cron_run(
                "demo",
                "twitch",
                "sync-twitch",
                CronRunCause::Scheduled,
                Some(200),
                "run-2",
                200,
            )
            .unwrap();
        assert!(matches!(outcome, CronClaimOutcome::Claimed(_)));
    }

    #[test]
    fn start_and_finish_cron_run_update_state_and_report_unknown_run_ids() {
        let store = store();
        let CronClaimOutcome::Claimed(run) = store
            .claim_cron_run(
                "demo",
                "twitch",
                "sync-twitch",
                CronRunCause::Manual,
                None,
                "run-1",
                100,
            )
            .unwrap()
        else {
            panic!("expected Claimed");
        };

        assert!(store
            .start_cron_run(
                &run.run_id,
                101,
                "dep-b",
                "demo-twitch-cron-sync-twitch-abc123",
                "100.64.0.9"
            )
            .unwrap());
        let started = store.cron_run(&run.run_id).unwrap().unwrap();
        assert_eq!(started.state, CronRunState::Running);
        assert_eq!(started.deployment_id.as_deref(), Some("dep-b"));

        assert!(store
            .finish_cron_run(&run.run_id, CronRunState::Succeeded, 130, Some(0), None)
            .unwrap());
        let finished = store.cron_run(&run.run_id).unwrap().unwrap();
        assert_eq!(finished.state, CronRunState::Succeeded);
        assert_eq!(finished.exit_code, Some(0));
        assert_eq!(finished.finished_at, Some(130));
        assert!(store
            .active_cron_run("twitch", "sync-twitch")
            .unwrap()
            .is_none());

        assert!(!store
            .start_cron_run("no-such-run", 1, "d", "c", "a")
            .unwrap());
        assert!(!store
            .finish_cron_run("no-such-run", CronRunState::Failed, 1, None, None)
            .unwrap());
    }

    #[test]
    fn cron_runs_filters_by_service_cron_name_run_id_since_and_limit() {
        let store = store();
        for (service, cron_name, run_id, claimed_at) in [
            ("twitch", "sync-twitch", "run-1", 100u64),
            ("twitch", "sync-twitch", "run-2", 200),
            ("twitch", "cleanup", "run-3", 300),
            ("backups", "nightly", "run-4", 400),
        ] {
            store
                .claim_cron_run(
                    "demo",
                    service,
                    cron_name,
                    CronRunCause::Scheduled,
                    Some(claimed_at),
                    run_id,
                    claimed_at,
                )
                .unwrap();
            store
                .finish_cron_run(
                    run_id,
                    CronRunState::Succeeded,
                    claimed_at + 1,
                    Some(0),
                    None,
                )
                .unwrap();
        }

        let by_service = store
            .cron_runs(&CronRunFilter {
                service: Some("twitch".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(by_service.len(), 3);

        let by_cron_name = store
            .cron_runs(&CronRunFilter {
                service: Some("twitch".into()),
                cron_name: Some("cleanup".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(by_cron_name.len(), 1);
        assert_eq!(by_cron_name[0].run_id, "run-3");

        let by_run_id = store
            .cron_runs(&CronRunFilter {
                run_id: Some("run-2".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(by_run_id.len(), 1);

        let since = store
            .cron_runs(&CronRunFilter {
                since: Some(300),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(since.len(), 2);

        let limited = store
            .cron_runs(&CronRunFilter {
                limit: Some(1),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(limited.len(), 1);
        // ORDER BY claimed_at DESC: the single row returned must be the most recent.
        assert_eq!(limited[0].run_id, "run-4");
    }

    #[test]
    fn set_cron_scheduler_state_preserves_the_skip_counter() {
        let store = store();
        store
            .claim_cron_run(
                "demo",
                "twitch",
                "sync-twitch",
                CronRunCause::Manual,
                None,
                "run-1",
                100,
            )
            .unwrap();
        store
            .claim_cron_run(
                "demo",
                "twitch",
                "sync-twitch",
                CronRunCause::Scheduled,
                Some(200),
                "run-2",
                200,
            )
            .unwrap();
        assert_eq!(
            store
                .cron_scheduler_state("twitch", "sync-twitch")
                .unwrap()
                .unwrap()
                .skipped_overlap_count,
            1
        );

        store
            .set_cron_scheduler_state("twitch", "sync-twitch", Some(200), Some(7_200))
            .unwrap();
        let state = store
            .cron_scheduler_state("twitch", "sync-twitch")
            .unwrap()
            .unwrap();
        assert_eq!(state.last_evaluated_at, Some(200));
        assert_eq!(state.next_due_at, Some(7_200));
        assert_eq!(state.skipped_overlap_count, 1);
    }

    #[test]
    fn retain_cron_runs_keeps_the_latest_n_regardless_of_age_and_never_removes_active_runs() {
        let store = store();
        // Five terminal runs, 1 day apart starting at t=0; keep_seconds=1 day means only the
        // very latest survives on age alone, but keep_latest=2 protects one more.
        for i in 0..5u64 {
            let claimed_at = i * 86_400;
            let run_id = format!("run-{i}");
            store
                .claim_cron_run(
                    "demo",
                    "twitch",
                    "sync-twitch",
                    CronRunCause::Scheduled,
                    Some(claimed_at),
                    &run_id,
                    claimed_at,
                )
                .unwrap();
            store
                .finish_cron_run(
                    &run_id,
                    CronRunState::Succeeded,
                    claimed_at + 1,
                    Some(0),
                    None,
                )
                .unwrap();
        }
        // A still-active run must survive regardless of age.
        store
            .claim_cron_run(
                "demo",
                "backups",
                "nightly",
                CronRunCause::Scheduled,
                Some(0),
                "run-active",
                0,
            )
            .unwrap();

        let now_ts = 4 * 86_400;
        let removed = store.retain_cron_runs(now_ts, 86_400, 2).unwrap();
        // Cutoff is now_ts - 86_400 = 259_200: run-0/1/2 are older than that. Rank (by
        // claimed_at DESC) beyond the latest 2 is run-0/1/2 too, so all three are removed;
        // run-3 and run-4 survive on rank alone even though run-2 also satisfies the age check.
        assert_eq!(
            removed, 3,
            "run-0, run-1, and run-2 are old and beyond the latest 2"
        );

        let remaining: Vec<String> = store
            .cron_runs(&CronRunFilter::default())
            .unwrap()
            .into_iter()
            .map(|run| run.run_id)
            .collect();
        assert!(!remaining.contains(&"run-0".to_string()));
        assert!(!remaining.contains(&"run-1".to_string()));
        assert!(!remaining.contains(&"run-2".to_string()));
        assert!(remaining.contains(&"run-3".to_string()));
        assert!(remaining.contains(&"run-4".to_string()));
        assert!(remaining.contains(&"run-active".to_string()));
    }

    #[test]
    fn retain_cron_runs_protects_old_runs_within_the_latest_n() {
        let store = store();
        for i in 0..5u64 {
            let claimed_at = i * 86_400;
            let run_id = format!("run-{i}");
            store
                .claim_cron_run(
                    "demo",
                    "twitch",
                    "sync-twitch",
                    CronRunCause::Scheduled,
                    Some(claimed_at),
                    &run_id,
                    claimed_at,
                )
                .unwrap();
            store
                .finish_cron_run(
                    &run_id,
                    CronRunState::Succeeded,
                    claimed_at + 1,
                    Some(0),
                    None,
                )
                .unwrap();
        }
        // Every run is well past a 1-day cutoff, but keep_latest=100 exceeds the total run
        // count, so rank alone protects all of them regardless of age.
        let removed = store.retain_cron_runs(4 * 86_400, 86_400, 100).unwrap();
        assert_eq!(removed, 0);
        assert_eq!(store.cron_runs(&CronRunFilter::default()).unwrap().len(), 5);
    }
}
