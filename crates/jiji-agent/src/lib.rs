//! Project-scoped host agent. It owns durable local state and observe-only
//! container discovery; once enrolled with a mesh configuration it also
//! ingests CLI-pushed membership directly (no peer-to-peer relay, see
//! `membership.rs`), incrementally repairs WireGuard, replicates a
//! node-owned service catalog authoritatively updated by dynamic
//! deployments (`jiji-cli`'s deploy/restart/rollback/remove/scale, via this
//! agent's `CatalogCommit` API), and serves project DNS from that catalog.

pub mod api;
pub mod backup;
pub mod bridge_bringup;
pub mod catalog;
pub mod catalog_replication;
pub mod cron;
pub mod cron_exec;
pub mod cron_schedule;
pub mod desired;
pub mod discovery;
pub mod dns;
pub mod engine;
pub mod host_lease;
pub mod leases;
pub mod local_reconcile;
pub mod membership;
pub mod paths;
pub mod proxy_bringup;
pub mod runtime;
pub mod scheduler;
pub mod store;
pub mod systemd;
pub mod wireguard;
pub mod wireguard_bringup;

pub use engine::Engine;
pub use paths::AgentPaths;
pub use store::{AddressLease, AgentStore, Observation, StoreError};
