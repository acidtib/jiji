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

use crate::catalog::{CatalogApply, CatalogError, CatalogView, SignedCatalogOperation};
use crate::desired::{DesiredApply, DesiredError, DesiredStateView, SignedDesiredState};
use crate::membership::{
    AuthorityKeyring, MembershipApply, MembershipError, MembershipState, MembershipView,
    SignedMembership,
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
                  signer_id TEXT NOT NULL,
                  signature BLOB NOT NULL,
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
                  signer_id TEXT NOT NULL,
                  signature BLOB NOT NULL,
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
                  signer_id TEXT NOT NULL,
                  signature BLOB NOT NULL,
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
                  signer_id TEXT NOT NULL,
                  signature BLOB NOT NULL,
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

    pub fn membership_operations(&self) -> Result<Vec<SignedMembership>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT operation_id, record_json, signer_id, signature
             FROM membership_operations ORDER BY rowid",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        let mut operations = Vec::new();
        for row in rows {
            let (operation_id, record_json, signer_id, signature) = row?;
            operations.push(SignedMembership {
                operation_id,
                signer_id,
                record: serde_json::from_str(&record_json)?,
                signature,
            });
        }
        Ok(operations)
    }

    /// Returns the signed operation behind every materialized winning membership record.
    ///
    /// This is the bounded anti-entropy snapshot. It deliberately includes tombstones: removing
    /// a winning tombstone would let a long-offline peer replay an older active record and
    /// resurrect a decommissioned node.
    pub fn membership_snapshot_operations(&self) -> Result<Vec<SignedMembership>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT o.operation_id, o.record_json, o.signer_id, o.signature
             FROM membership_records r
             JOIN membership_operations o ON o.operation_id = r.operation_id
             ORDER BY r.node_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        let mut operations = Vec::new();
        for row in rows {
            let (operation_id, record_json, signer_id, signature) = row?;
            operations.push(SignedMembership {
                operation_id,
                signer_id,
                record: serde_json::from_str(&record_json)?,
                signature,
            });
        }
        Ok(operations)
    }

    /// Verifies and durably materializes one replicated membership operation.
    /// The operation and winning record commit atomically.
    pub fn apply_membership(
        &self,
        operation: &SignedMembership,
        authority: &AuthorityKeyring,
    ) -> Result<MembershipApply, StoreError> {
        if self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM membership_operations WHERE operation_id = ?1)",
            [&operation.operation_id],
            |row| row.get::<_, bool>(0),
        )? {
            return Ok(MembershipApply::Duplicate);
        }
        self.ensure_replication_capacity()?;
        let existing = self.membership_operations()?;
        let mut view = MembershipView::from_operations(existing, authority)?;
        let outcome = view.apply(operation.clone(), authority)?;
        let tx = self.conn.unchecked_transaction()?;
        let inserted = tx.execute(
            "INSERT INTO membership_operations
                 (operation_id, record_json, signer_id, signature, applied_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(operation_id) DO NOTHING",
            params![
                operation.operation_id,
                serde_json::to_string(&operation.record)?,
                operation.signer_id,
                operation.signature,
                now(),
            ],
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
                    operation.record.node_id,
                    operation.record.server_name,
                    operation.record.owner_epoch,
                    operation.record.revision,
                    match operation.record.state {
                        crate::membership::MembershipState::Active => "active",
                        crate::membership::MembershipState::Tombstoned => "tombstoned",
                    },
                    operation.operation_id,
                    serde_json::to_string(&operation.record)?,
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

    /// Returns the winning record for every known node, including authenticated
    /// tombstones. Absence alone is deliberately never removal authority.
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

    pub fn catalog_operations(&self) -> Result<Vec<SignedCatalogOperation>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT operation_id, record_json, signer_id, signature
             FROM catalog_operations ORDER BY rowid",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        let mut operations = Vec::new();
        for row in rows {
            let (operation_id, record_json, signer_id, signature) = row?;
            operations.push(SignedCatalogOperation {
                operation_id,
                signer_id,
                record: serde_json::from_str(&record_json)?,
                signature,
            });
        }
        Ok(operations)
    }

    /// Returns one signed winner per deployment, including stopped and tombstoned deployments.
    /// Superseded history is not required for convergence because catalog ordering is monotonic
    /// per owner/deployment and the signed winner contains the complete record.
    pub fn catalog_snapshot_operations(&self) -> Result<Vec<SignedCatalogOperation>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT o.operation_id, o.record_json, o.signer_id, o.signature
             FROM catalog_records r
             JOIN catalog_operations o ON o.operation_id = r.operation_id
             ORDER BY r.replica_id, r.deployment_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        let mut operations = Vec::new();
        for row in rows {
            let (operation_id, record_json, signer_id, signature) = row?;
            operations.push(SignedCatalogOperation {
                operation_id,
                signer_id,
                record: serde_json::from_str(&record_json)?,
                signature,
            });
        }
        Ok(operations)
    }

    /// Verifies (against the caller-supplied, already-authenticated membership view) and durably
    /// materializes one replicated catalog operation. Mirrors `apply_membership`: the operation and
    /// winning record commit atomically, and only a successful verification touches the database.
    pub fn apply_catalog(
        &self,
        operation: &SignedCatalogOperation,
        project_id: &str,
        recovery_epoch: u64,
        membership: &MembershipView,
    ) -> Result<CatalogApply, StoreError> {
        if self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM catalog_operations WHERE operation_id = ?1)",
            [&operation.operation_id],
            |row| row.get::<_, bool>(0),
        )? {
            return Ok(CatalogApply::Duplicate);
        }
        self.ensure_replication_capacity()?;
        let existing = self.catalog_operations()?;
        let mut view =
            CatalogView::from_operations(existing, project_id, recovery_epoch, membership)?;
        let outcome = view.apply(operation.clone(), project_id, recovery_epoch, membership)?;
        let tx = self.conn.unchecked_transaction()?;
        let inserted = tx.execute(
            "INSERT INTO catalog_operations
                 (operation_id, record_json, signer_id, signature, applied_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(operation_id) DO NOTHING",
            params![
                operation.operation_id,
                serde_json::to_string(&operation.record)?,
                operation.signer_id,
                operation.signature,
                now(),
            ],
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
                    operation.record.deployment_id,
                    operation.record.replica_id,
                    operation.record.service,
                    operation.record.owner_node_id,
                    operation.record.owner_epoch,
                    operation.record.revision,
                    match operation.record.state {
                        crate::catalog::DeploymentState::Candidate => "candidate",
                        crate::catalog::DeploymentState::Active => "active",
                        crate::catalog::DeploymentState::Draining => "draining",
                        crate::catalog::DeploymentState::Stopped => "stopped",
                        crate::catalog::DeploymentState::Tombstoned => "tombstoned",
                    },
                    operation.operation_id,
                    serde_json::to_string(&operation.record)?,
                ],
            )?;
        }
        tx.commit()?;
        Ok(outcome)
    }

    /// Returns the winning record for every known replica, including tombstones -- absence alone
    /// is never removal authority, matching `latest_membership`.
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

    pub fn desired_operations(&self) -> Result<Vec<SignedDesiredState>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT operation_id, record_json, signer_id, signature
             FROM desired_operations ORDER BY rowid",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        let mut operations = Vec::new();
        for row in rows {
            let (operation_id, record_json, signer_id, signature) = row?;
            operations.push(SignedDesiredState {
                operation_id,
                signer_id,
                record: serde_json::from_str(&record_json)?,
                signature,
            });
        }
        Ok(operations)
    }

    /// Returns the signed winning desired-state record for each service.
    pub fn desired_snapshot_operations(&self) -> Result<Vec<SignedDesiredState>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT o.operation_id, o.record_json, o.signer_id, o.signature
             FROM desired_records r
             JOIN desired_operations o ON o.operation_id = r.operation_id
             ORDER BY r.service",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        let mut operations = Vec::new();
        for row in rows {
            let (operation_id, record_json, signer_id, signature) = row?;
            operations.push(SignedDesiredState {
                operation_id,
                signer_id,
                record: serde_json::from_str(&record_json)?,
                signature,
            });
        }
        Ok(operations)
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
        operation: &SignedDesiredState,
        project_id: &str,
        recovery_epoch: u64,
        membership: &MembershipView,
    ) -> Result<DesiredApply, StoreError> {
        if self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM desired_operations WHERE operation_id = ?1)",
            [&operation.operation_id],
            |row| row.get::<_, bool>(0),
        )? {
            return Ok(DesiredApply::Duplicate);
        }
        self.ensure_replication_capacity()?;
        let mut view = DesiredStateView::from_operations(
            self.desired_operations()?,
            project_id,
            recovery_epoch,
            membership,
        )?;
        let outcome = view.apply(operation.clone(), project_id, recovery_epoch, membership)?;
        let tx = self.conn.unchecked_transaction()?;
        let inserted = tx.execute(
            "INSERT INTO desired_operations
                 (operation_id, service, record_json, signer_id, signature, applied_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(operation_id) DO NOTHING",
            params![
                operation.operation_id,
                operation.record.service,
                serde_json::to_string(&operation.record)?,
                operation.signer_id,
                operation.signature,
                now(),
            ],
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
                    operation.record.service,
                    operation.record.revision,
                    operation.operation_id,
                    serde_json::to_string(&operation.record)?,
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
        AuthorityKeyring, MembershipRecord, MembershipState, SignedMembership,
        MEMBERSHIP_PROTOCOL_VERSION, MEMBERSHIP_SCHEMA_VERSION,
    };
    use ed25519_dalek::SigningKey;
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

    fn signed_member(node: &str, revision: u64) -> (AuthorityKeyring, SignedMembership) {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let mut authority = AuthorityKeyring::new("project", 1);
        authority.add_authority("root", signing_key.verifying_key());
        let record = MembershipRecord {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            node_id: node.into(),
            server_name: node.into(),
            node_signing_public_key: vec![8; 32],
            wireguard_public_key: format!("wg-{node}"),
            management_address: Ipv4Addr::new(100, 98, 64, 2),
            container_subnet: "198.18.2.0/24".into(),
            endpoints: vec!["192.0.2.2:51820".parse().unwrap()],
            owner_epoch: 1,
            revision,
            state: MembershipState::Active,
        };
        let signed = SignedMembership::sign(record, "root", &signing_key).unwrap();
        (authority, signed)
    }

    #[test]
    fn signed_membership_is_atomic_idempotent_and_survives_restart() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("agent.sqlite3");
        let (authority, operation) = signed_member("node-a", 1);
        {
            let store = AgentStore::open(&db_path).unwrap();
            assert_eq!(
                store.apply_membership(&operation, &authority).unwrap(),
                MembershipApply::Applied
            );
            assert_eq!(
                store.apply_membership(&operation, &authority).unwrap(),
                MembershipApply::Duplicate
            );
        }
        let store = AgentStore::open(&db_path).unwrap();
        assert_eq!(store.active_membership().unwrap().len(), 1);
        assert_eq!(store.membership_operations().unwrap(), vec![operation]);
    }

    #[test]
    fn compaction_keeps_the_signed_winner_and_removes_only_superseded_history() {
        let dir = tempdir().unwrap();
        let store = AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap();
        let (authority, first) = signed_member("node-a", 1);
        let (_, second) = signed_member("node-a", 2);
        store.apply_membership(&first, &authority).unwrap();
        store.apply_membership(&second, &authority).unwrap();
        assert_eq!(store.membership_operations().unwrap().len(), 2);

        let result = store.compact_operations().unwrap();
        assert_eq!(result.membership_removed, 1);
        assert_eq!(
            store.membership_snapshot_operations().unwrap(),
            vec![second.clone()]
        );
        assert_eq!(store.membership_operations().unwrap(), vec![second]);
        assert_eq!(store.latest_membership().unwrap()[0].revision, 2);
    }

    #[test]
    fn compaction_never_collects_a_winning_tombstone_fence() {
        let dir = tempdir().unwrap();
        let store = AgentStore::open(&dir.path().join("agent.sqlite3")).unwrap();
        let (authority, active) = signed_member("node-a", 1);
        store.apply_membership(&active, &authority).unwrap();
        let mut record = active.record.clone();
        record.revision = 2;
        record.state = MembershipState::Tombstoned;
        let tombstone =
            SignedMembership::sign(record, "root", &SigningKey::from_bytes(&[7; 32])).unwrap();
        store.apply_membership(&tombstone, &authority).unwrap();
        store.compact_operations().unwrap();

        assert_eq!(
            store.membership_snapshot_operations().unwrap(),
            vec![tombstone]
        );
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
        let (authority, first) = signed_member("node-a", 1);
        store.apply_membership(&first, &authority).unwrap();
        store.set_soft_quota_bytes(Some(1));

        assert_eq!(
            store.apply_membership(&first, &authority).unwrap(),
            MembershipApply::Duplicate
        );
        let (_, second) = signed_member("node-a", 2);
        assert!(matches!(
            store.apply_membership(&second, &authority),
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

    fn membership_view_for(
        node_id: &str,
        signing_key: &ed25519_dalek::SigningKey,
    ) -> MembershipView {
        let signer_key = SigningKey::from_bytes(&[42; 32]);
        let mut authority = AuthorityKeyring::new("project", 1);
        authority.add_authority("root", signer_key.verifying_key());
        let record = MembershipRecord {
            project_id: "project".into(),
            recovery_epoch: 1,
            protocol_version: MEMBERSHIP_PROTOCOL_VERSION,
            schema_version: MEMBERSHIP_SCHEMA_VERSION,
            node_id: node_id.into(),
            server_name: node_id.into(),
            node_signing_public_key: signing_key.verifying_key().to_bytes().to_vec(),
            wireguard_public_key: format!("wg-{node_id}"),
            management_address: Ipv4Addr::new(100, 98, 64, 3),
            container_subnet: "198.18.3.0/24".into(),
            endpoints: vec!["192.0.2.3:51820".parse().unwrap()],
            owner_epoch: 1,
            revision: 1,
            state: MembershipState::Active,
        };
        let signed = SignedMembership::sign(record, "root", &signer_key).unwrap();
        let mut view = MembershipView::default();
        view.apply(signed, &authority).unwrap();
        view
    }

    fn signed_catalog_operation(
        replica: &str,
        node_id: &str,
        revision: u64,
        node_key: &ed25519_dalek::SigningKey,
    ) -> SignedCatalogOperation {
        use crate::catalog::{CatalogRecord, DeploymentState, HealthState};
        SignedCatalogOperation::sign(
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
            },
            node_key,
        )
        .unwrap()
    }

    #[test]
    fn signed_catalog_is_atomic_idempotent_and_survives_restart() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("agent.sqlite3");
        let node_key = ed25519_dalek::SigningKey::from_bytes(&[11; 32]);
        let membership = membership_view_for("node-a", &node_key);
        let operation = signed_catalog_operation("r1", "node-a", 1, &node_key);
        {
            let store = AgentStore::open(&db_path).unwrap();
            assert_eq!(
                store
                    .apply_catalog(&operation, "project", 1, &membership)
                    .unwrap(),
                CatalogApply::Applied
            );
            assert_eq!(
                store
                    .apply_catalog(&operation, "project", 1, &membership)
                    .unwrap(),
                CatalogApply::Duplicate
            );
        }
        let store = AgentStore::open(&db_path).unwrap();
        assert_eq!(store.latest_catalog().unwrap().len(), 1);
        assert_eq!(store.catalog_operations().unwrap(), vec![operation]);
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
