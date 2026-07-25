use crate::naming::{self, stable_hash};
use crate::{Ipv4Cidr, NetworkPlanError};
use jiji_config::Config;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::net::Ipv4Addr;

pub const CONTAINER_SERVER_PREFIX: u8 = 21;
const SERVER_BUCKET_CAPACITY: usize = 8;
const ENDPOINT_BUCKET_CAPACITY: usize = 16;
const FIRST_CONTAINER_OFFSET: u64 = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPlan {
    pub enabled: bool,
    pub project: String,
    pub management_cidr: Ipv4Cidr,
    pub container_cidr: Ipv4Cidr,
    pub servers: BTreeMap<String, ServerPlan>,
    pub endpoints: BTreeMap<String, ServiceEndpointPlan>,
    pub dns_records: BTreeMap<String, DnsRecord>,
    pub generation: String,
}

impl NetworkPlan {
    /// Selects where a complete plan should be applied without recalculating topology.
    pub fn select_hosts(&self, filters: &[String]) -> Result<Vec<&ServerPlan>, NetworkPlanError> {
        if filters.is_empty() {
            return Ok(self.servers.values().collect());
        }

        let mut selected = BTreeSet::new();
        for filter in filters {
            let matches: Vec<&str> = self
                .servers
                .iter()
                .filter(|(name, server)| {
                    jiji_core::matches_pattern(name, filter)
                        || jiji_core::matches_pattern(&server.public_host, filter)
                })
                .map(|(name, _)| name.as_str())
                .collect();
            if matches.is_empty() {
                return Err(NetworkPlanError::UnmatchedHostFilter {
                    filter: filter.clone(),
                });
            }
            selected.extend(matches.into_iter().map(str::to_string));
        }

        Ok(selected
            .iter()
            .filter_map(|name| self.servers.get(name))
            .collect())
    }

    /// Selects service endpoints from the complete plan without recalculating addresses or DNS.
    pub fn select_endpoints(
        &self,
        filters: &[String],
    ) -> Result<Vec<&ServiceEndpointPlan>, NetworkPlanError> {
        if filters.is_empty() {
            return Ok(self.endpoints.values().collect());
        }

        let mut selected = BTreeSet::new();
        for filter in filters {
            let matches: Vec<&str> = self
                .endpoints
                .iter()
                .filter(|(_, endpoint)| jiji_core::matches_pattern(&endpoint.service, filter))
                .map(|(identity, _)| identity.as_str())
                .collect();
            if matches.is_empty() {
                return Err(NetworkPlanError::UnmatchedServiceFilter {
                    filter: filter.clone(),
                });
            }
            selected.extend(matches.into_iter().map(str::to_string));
        }

        Ok(selected
            .iter()
            .filter_map(|identity| self.endpoints.get(identity))
            .collect())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerPlan {
    pub name: String,
    pub public_host: String,
    pub management_address: Ipv4Addr,
    pub container_subnet: Ipv4Cidr,
    pub bridge_gateway: Ipv4Addr,
    pub dns_address: Ipv4Addr,
    /// Fixed address for the kamal-proxy container, reserved below `FIRST_CONTAINER_OFFSET` so it
    /// can never collide with a service endpoint address, and pinned explicitly (rather than left
    /// to IPAM auto-assignment) so it can never collide with `dns_address` either.
    pub proxy_address: Ipv4Addr,
    pub wireguard_port: u16,
    /// Project-scoped WireGuard interface name (see `naming::wireguard_interface_name`) -- one
    /// per project, shared by every server in that project's plan, distinct from every other
    /// project's, so multiple projects can coexist on one host without colliding.
    pub wireguard_interface: String,
    /// Project-scoped kernel bridge device name (`naming::bridge_interface_name`), distinct from
    /// `bridge_name` below: this one is passed to `--opt com.docker.network.bridge.name=`/
    /// `--interface-name` and is therefore subject to Linux's 15-char interface name limit.
    pub bridge_interface: String,
    /// Project-scoped Docker/Podman logical network name (`naming::bridge_network_name`),
    /// unconstrained length, used everywhere else a network name is needed.
    pub bridge_name: String,
    pub peers: Vec<WireGuardPeerPlan>,
    pub routes: Vec<RoutePlan>,
    pub firewall: FirewallPlan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireGuardPeerPlan {
    pub server: String,
    pub endpoint: String,
    pub management_address: Ipv4Addr,
    pub allowed_ips: Vec<Ipv4Cidr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutePlan {
    pub destination: Ipv4Cidr,
    pub interface: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirewallPlan {
    pub local_container_subnet: Ipv4Cidr,
    pub remote_container_subnets: Vec<Ipv4Cidr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceEndpointPlan {
    pub identity: String,
    pub project: String,
    pub service: String,
    pub server: String,
    /// Stable address published in DNS. No container binds this address directly.
    pub address: Ipv4Addr,
    /// Deterministic blue/green addresses used by old and replacement containers.
    pub backend_addresses: [Ipv4Addr; 2],
    /// Aggregate service name, shared by every replica.
    pub dns_name: String,
    /// Replica-specific name for this service endpoint.
    pub server_dns_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsRecord {
    pub name: String,
    pub addresses: Vec<Ipv4Addr>,
}

#[derive(Debug, Default)]
pub struct NetworkPlanner;

impl NetworkPlanner {
    pub fn new() -> Self {
        Self
    }

    pub fn plan(&self, config: &Config) -> Result<NetworkPlan, NetworkPlanError> {
        let network = config.network.as_ref();
        let management_cidr = network
            .map(|value| value.management_cidr())
            .unwrap_or(jiji_core::DEFAULT_MANAGEMENT_CIDR)
            .parse::<Ipv4Cidr>()?;
        let container_cidr = network
            .map(|value| value.container_cidr())
            .unwrap_or(jiji_core::DEFAULT_CONTAINER_CIDR)
            .parse::<Ipv4Cidr>()?;
        if management_cidr.overlaps(container_cidr) {
            return Err(NetworkPlanError::OverlappingAddressSpaces {
                management: management_cidr.to_string(),
                container: container_cidr.to_string(),
            });
        }

        let enabled = network.map(|value| value.enabled).unwrap_or(true);
        let server_slots = container_cidr
            .subnet_count(CONTAINER_SERVER_PREFIX)
            .ok_or_else(|| NetworkPlanError::ContainerRangeTooSmall {
                cidr: container_cidr.to_string(),
                prefix: CONTAINER_SERVER_PREFIX,
            })?;
        let management_slots = management_cidr.address_count().saturating_sub(2);
        if management_slots < server_slots {
            return Err(NetworkPlanError::ManagementRangeTooSmall {
                cidr: management_cidr.to_string(),
                available: management_slots,
                required: server_slots,
            });
        }

        // Widened from plain server name to `{project}:{server_name}` so two independent
        // projects' server-slot assignments (and therefore subnets/management addresses) are
        // computed independently of each other -- this is what lets multiple projects share a
        // host's address space without any coordination between them (see naming.rs's doc
        // comment and the project's network-isolation design notes for the full rationale).
        let server_identities: Vec<String> = config
            .servers
            .keys()
            .map(|name| format!("{}:{name}", config.project))
            .collect();
        let server_slot_assignments = allocate_bucketed(
            &server_identities,
            server_slots,
            SERVER_BUCKET_CAPACITY,
            "servers",
        )?;

        let wireguard_interface = naming::wireguard_interface_name(&config.project);
        let bridge_interface = naming::bridge_interface_name(&config.project);
        let bridge_name = naming::bridge_network_name(&config.project);
        let wireguard_port = naming::wireguard_port(&config.project);

        let mut base_servers = BTreeMap::new();
        for (identity, slot) in server_slot_assignments {
            let name = identity
                .strip_prefix(&format!("{}:", config.project))
                .expect("identity was built with this exact prefix above")
                .to_string();
            let named_server = &config.servers[&name];
            let container_subnet = container_cidr
                .subnet(CONTAINER_SERVER_PREFIX, slot)
                .expect("validated server subnet index");
            let management_address = management_cidr
                .address(slot + 1)
                .expect("validated management address capacity");
            base_servers.insert(
                name.clone(),
                ServerPlan {
                    name,
                    public_host: named_server.host.clone(),
                    management_address,
                    container_subnet,
                    bridge_gateway: container_subnet
                        .address(1)
                        .expect("a /20 has a gateway address"),
                    dns_address: container_subnet
                        .address(2)
                        .expect("a /20 has a DNS address"),
                    // Offset 3 is reserved by the Podman bridge's `jiji-network-anchor` keepalive
                    // container (`BridgeProvisioner::render_podman_network`); use the next offset
                    // so the two reservations can never collide.
                    proxy_address: container_subnet
                        .address(4)
                        .expect("a /20 has a proxy address"),
                    wireguard_port,
                    wireguard_interface: wireguard_interface.clone(),
                    bridge_interface: bridge_interface.clone(),
                    bridge_name: bridge_name.clone(),
                    peers: Vec::new(),
                    routes: Vec::new(),
                    firewall: FirewallPlan {
                        local_container_subnet: container_subnet,
                        remote_container_subnets: Vec::new(),
                    },
                },
            );
        }

        let endpoints = plan_endpoints(config, &base_servers)?;
        let dns_records = plan_dns(&endpoints);
        let servers = populate_server_networks(base_servers);
        let generation = generation_checksum(
            enabled,
            &config.project,
            management_cidr,
            container_cidr,
            &servers,
            &endpoints,
            &dns_records,
        );

        Ok(NetworkPlan {
            enabled,
            project: config.project.clone(),
            management_cidr,
            container_cidr,
            servers,
            endpoints,
            dns_records,
            generation,
        })
    }
}

fn plan_endpoints(
    config: &Config,
    servers: &BTreeMap<String, ServerPlan>,
) -> Result<BTreeMap<String, ServiceEndpointPlan>, NetworkPlanError> {
    let mut identities_by_server: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (service_name, service) in &config.services {
        let mut seen = BTreeSet::new();
        for server_name in &service.servers {
            if !seen.insert(server_name) {
                return Err(NetworkPlanError::DuplicateServiceServer {
                    service: service_name.clone(),
                    server: server_name.clone(),
                });
            }
            if !servers.contains_key(server_name) {
                return Err(NetworkPlanError::UnknownServiceServer {
                    service: service_name.clone(),
                    server: server_name.clone(),
                });
            }
            let identity = format!("{}:{service_name}:{server_name}", config.project);
            identities_by_server
                .entry(server_name.clone())
                .or_default()
                .push((identity, service_name.clone()));
        }
    }

    let mut endpoints = BTreeMap::new();
    for (server_name, identities) in identities_by_server {
        let server = &servers[&server_name];
        let assignable = server
            .container_subnet
            .address_count()
            .saturating_sub(FIRST_CONTAINER_OFFSET + 1);
        let canonical_identities: Vec<String> = identities
            .iter()
            .flat_map(|(identity, _)| {
                [
                    endpoint_address_identity(identity, "vip"),
                    endpoint_address_identity(identity, "backend-a"),
                    endpoint_address_identity(identity, "backend-b"),
                ]
            })
            .collect();
        let assignments = allocate_bucketed(
            &canonical_identities,
            assignable,
            ENDPOINT_BUCKET_CAPACITY,
            "service endpoints",
        )?;
        for (endpoint_identity, service) in identities {
            let address_for = |role: &str| {
                let allocation_identity = endpoint_address_identity(&endpoint_identity, role);
                let slot = assignments[&allocation_identity];
                server
                    .container_subnet
                    .address(FIRST_CONTAINER_OFFSET + slot)
                    .expect("validated service address offset")
            };
            let address = address_for("vip");
            let backend_addresses = [address_for("backend-a"), address_for("backend-b")];
            let dns_name = format!(
                "{}-{}.{}.",
                config.project,
                service,
                jiji_core::DEFAULT_SERVICE_DOMAIN
            );
            let server_dns_name = format!(
                "{}-{}-{}.{}.",
                config.project,
                service,
                server_name,
                jiji_core::DEFAULT_SERVICE_DOMAIN
            );
            endpoints.insert(
                endpoint_identity.clone(),
                ServiceEndpointPlan {
                    identity: endpoint_identity,
                    project: config.project.clone(),
                    service,
                    server: server_name.clone(),
                    address,
                    backend_addresses,
                    dns_name,
                    server_dns_name,
                },
            );
        }
    }
    Ok(endpoints)
}

fn endpoint_address_identity(endpoint_identity: &str, role: &str) -> String {
    format!("{endpoint_identity}:{role}")
}

fn plan_dns(endpoints: &BTreeMap<String, ServiceEndpointPlan>) -> BTreeMap<String, DnsRecord> {
    let mut records: BTreeMap<String, DnsRecord> = BTreeMap::new();
    for endpoint in endpoints.values() {
        for name in [&endpoint.dns_name, &endpoint.server_dns_name] {
            records
                .entry(name.clone())
                .or_insert_with(|| DnsRecord {
                    name: name.clone(),
                    addresses: Vec::new(),
                })
                .addresses
                .push(endpoint.address);
        }
    }
    for record in records.values_mut() {
        record.addresses.sort_unstable();
    }
    records
}

fn populate_server_networks(
    mut servers: BTreeMap<String, ServerPlan>,
) -> BTreeMap<String, ServerPlan> {
    let snapshots: Vec<(String, String, Ipv4Addr, Ipv4Cidr, u16)> = servers
        .values()
        .map(|server| {
            (
                server.name.clone(),
                server.public_host.clone(),
                server.management_address,
                server.container_subnet,
                server.wireguard_port,
            )
        })
        .collect();

    for server in servers.values_mut() {
        let wireguard_interface = server.wireguard_interface.clone();
        for (name, public_host, management_address, container_subnet, wireguard_port) in &snapshots
        {
            if name == &server.name {
                continue;
            }
            server.peers.push(WireGuardPeerPlan {
                server: name.clone(),
                endpoint: format!("{public_host}:{wireguard_port}"),
                management_address: *management_address,
                allowed_ips: vec![
                    Ipv4Cidr::new(*management_address, 32)
                        .expect("a management host route is a valid CIDR"),
                    *container_subnet,
                ],
            });
            server.routes.push(RoutePlan {
                destination: *container_subnet,
                interface: wireguard_interface.clone(),
            });
            server
                .firewall
                .remote_container_subnets
                .push(*container_subnet);
        }
    }
    servers
}

fn allocate_bucketed(
    identities: &[String],
    total_slots: u64,
    desired_bucket_capacity: usize,
    kind: &'static str,
) -> Result<BTreeMap<String, u64>, NetworkPlanError> {
    if identities.is_empty() {
        return Ok(BTreeMap::new());
    }

    let bucket_capacity = desired_bucket_capacity.min(total_slots as usize);
    let bucket_count = total_slots / bucket_capacity as u64;
    let mut buckets: BTreeMap<u64, Vec<String>> = BTreeMap::new();
    for identity in identities {
        let bucket = stable_hash(identity.as_bytes()) % bucket_count;
        buckets.entry(bucket).or_default().push(identity.clone());
    }

    let mut assignments = BTreeMap::new();
    for (bucket, mut bucket_identities) in buckets {
        bucket_identities.sort_unstable();
        if bucket_identities.len() > bucket_capacity {
            return Err(NetworkPlanError::BucketExhausted {
                kind,
                bucket,
                capacity: bucket_capacity,
            });
        }
        for (offset, identity) in bucket_identities.into_iter().enumerate() {
            assignments.insert(identity, bucket * bucket_capacity as u64 + offset as u64);
        }
    }
    Ok(assignments)
}

fn generation_checksum(
    enabled: bool,
    project: &str,
    management_cidr: Ipv4Cidr,
    container_cidr: Ipv4Cidr,
    servers: &BTreeMap<String, ServerPlan>,
    endpoints: &BTreeMap<String, ServiceEndpointPlan>,
    dns_records: &BTreeMap<String, DnsRecord>,
) -> String {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, if enabled { "enabled" } else { "disabled" });
    hash_field(&mut hasher, project);
    hash_field(&mut hasher, &management_cidr.to_string());
    hash_field(&mut hasher, &container_cidr.to_string());

    for server in servers.values() {
        hash_field(&mut hasher, &server.name);
        hash_field(&mut hasher, &server.public_host);
        hash_field(&mut hasher, &server.management_address.to_string());
        hash_field(&mut hasher, &server.container_subnet.to_string());
        hash_field(&mut hasher, &server.wireguard_interface);
        hash_field(&mut hasher, &server.bridge_interface);
        hash_field(&mut hasher, &server.bridge_name);
        hash_field(&mut hasher, &server.wireguard_port.to_string());
        for peer in &server.peers {
            hash_field(&mut hasher, &peer.server);
            hash_field(&mut hasher, &peer.endpoint);
            for allowed_ip in &peer.allowed_ips {
                hash_field(&mut hasher, &allowed_ip.to_string());
            }
        }
    }
    for endpoint in endpoints.values() {
        hash_field(&mut hasher, &endpoint.identity);
        hash_field(&mut hasher, &endpoint.address.to_string());
        for address in endpoint.backend_addresses {
            hash_field(&mut hasher, &address.to_string());
        }
        hash_field(&mut hasher, &endpoint.dns_name);
        hash_field(&mut hasher, &endpoint.server_dns_name);
    }
    for record in dns_records.values() {
        hash_field(&mut hasher, &record.name);
        for address in &record.addresses {
            hash_field(&mut hasher, &address.to_string());
        }
    }

    let mut generation = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(generation, "{byte:02x}").expect("writing to a String cannot fail");
    }
    generation
}

fn hash_field(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(yaml: &str) -> Config {
        serde_yaml::from_str(yaml).expect("test configuration should parse")
    }

    fn base_config() -> Config {
        config(
            r#"
project: demo
builder:
  engine: docker
servers:
  app:
    host: 203.0.113.10
  data:
    host: 203.0.113.20
services:
  web:
    image: example/web
    servers: [app, data]
    ports: ["3000"]
  redis:
    image: redis
    servers: [data]
    ports: ["6379"]
network:
  management_cidr: 198.18.0.0/16
  container_cidr: 100.64.0.0/10
"#,
        )
    }

    #[test]
    fn repeated_plans_are_identical() {
        let planner = NetworkPlanner::new();
        let first = planner.plan(&base_config()).unwrap();
        let second = planner.plan(&base_config()).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.generation.len(), 64);
    }

    #[test]
    fn defaults_use_disjoint_shared_address_ranges() {
        let config = config(
            r#"
project: demo
builder: { engine: docker }
servers: {}
services: {}
"#,
        );
        let plan = NetworkPlanner::new().plan(&config).unwrap();
        assert_eq!(plan.management_cidr.to_string(), "198.18.0.0/16");
        assert_eq!(plan.container_cidr.to_string(), "100.64.0.0/10");
        assert!(!plan.management_cidr.overlaps(plan.container_cidr));
        assert_eq!(
            plan.container_cidr.subnet_count(CONTAINER_SERVER_PREFIX),
            Some(2_048)
        );
    }

    #[test]
    fn yaml_map_order_does_not_change_plan() {
        let reordered = config(
            r#"
project: demo
builder:
  engine: docker
servers:
  data:
    host: 203.0.113.20
  app:
    host: 203.0.113.10
services:
  redis:
    image: redis
    servers: [data]
    ports: ["6379"]
  web:
    image: example/web
    servers: [app, data]
    ports: ["3000"]
network:
  container_cidr: 100.64.0.0/10
  management_cidr: 198.18.0.0/16
"#,
        );
        let planner = NetworkPlanner::new();
        assert_eq!(
            planner.plan(&base_config()).unwrap(),
            planner.plan(&reordered).unwrap()
        );
    }

    #[test]
    fn plans_peers_routes_firewalls_and_replica_dns() {
        let plan = NetworkPlanner::new().plan(&base_config()).unwrap();
        let app = &plan.servers["app"];
        let data = &plan.servers["data"];

        assert_eq!(app.peers.len(), 1);
        assert_eq!(app.peers[0].server, "data");
        assert!(app.peers[0].allowed_ips.contains(&data.container_subnet));
        assert!(app
            .routes
            .iter()
            .any(|route| route.destination == data.container_subnet));
        assert_eq!(
            app.firewall.remote_container_subnets,
            vec![data.container_subnet]
        );

        let web = &plan.dns_records["demo-web.jiji."];
        assert_eq!(web.addresses.len(), 2);
        assert!(web.addresses.windows(2).all(|pair| pair[0] < pair[1]));
        let app_web = &plan.dns_records["demo-web-app.jiji."];
        assert_eq!(
            app_web.addresses,
            vec![plan.endpoints["demo:web:app"].address]
        );
        let data_web = &plan.dns_records["demo-web-data.jiji."];
        assert_eq!(
            data_web.addresses,
            vec![plan.endpoints["demo:web:data"].address]
        );
        assert_eq!(plan.dns_records["demo-redis.jiji."].addresses.len(), 1);
        assert_eq!(
            plan.dns_records["demo-redis-data.jiji."].addresses,
            vec![plan.endpoints["demo:redis:data"].address]
        );
    }

    #[test]
    fn selected_hosts_are_a_view_of_the_complete_plan() {
        let plan = NetworkPlanner::new().plan(&base_config()).unwrap();
        let generation = plan.generation.clone();
        let selected = plan.select_hosts(&["203.0.113.10".to_string()]).unwrap();

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "app");
        assert_eq!(selected[0].peers.len(), 1);
        assert_eq!(plan.servers.len(), 2);
        assert_eq!(plan.generation, generation);
    }

    #[test]
    fn select_hosts_matches_the_configured_server_name_as_well_as_its_address() {
        let plan = NetworkPlanner::new().plan(&base_config()).unwrap();

        let by_name = plan.select_hosts(&["app".to_string()]).unwrap();
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].name, "app");

        let by_wildcard_name = plan.select_hosts(&["a*".to_string()]).unwrap();
        assert_eq!(by_wildcard_name.len(), 1);
        assert_eq!(by_wildcard_name[0].name, "app");

        // A filter matching both a server's name and its address must not select it twice.
        let by_both = plan
            .select_hosts(&["app".to_string(), "203.0.113.10".to_string()])
            .unwrap();
        assert_eq!(by_both.len(), 1);
        assert_eq!(by_both[0].name, "app");
    }

    #[test]
    fn selected_services_are_a_view_of_the_complete_plan() {
        let plan = NetworkPlanner::new().plan(&base_config()).unwrap();
        let generation = plan.generation.clone();
        let selected = plan.select_endpoints(&["web".to_string()]).unwrap();

        assert_eq!(selected.len(), 2);
        assert!(selected.iter().all(|endpoint| endpoint.service == "web"));
        assert_eq!(plan.endpoints.len(), 3);
        assert_eq!(plan.dns_records.len(), 5);
        assert_eq!(plan.generation, generation);
    }

    #[test]
    fn service_addresses_exclude_infrastructure_and_broadcast() {
        let plan = NetworkPlanner::new().plan(&base_config()).unwrap();
        let mut all_addresses = BTreeSet::new();
        for endpoint in plan.endpoints.values() {
            let server = &plan.servers[&endpoint.server];
            for address in
                std::iter::once(endpoint.address).chain(endpoint.backend_addresses.iter().copied())
            {
                let offset =
                    u32::from(address) as u64 - u32::from(server.container_subnet.network()) as u64;
                assert!(offset >= FIRST_CONTAINER_OFFSET);
                assert!(offset < server.container_subnet.address_count() - 1);
                assert_ne!(address, server.bridge_gateway);
                assert_ne!(address, server.dns_address);
                assert_ne!(address, server.proxy_address);
                assert!(
                    all_addresses.insert((endpoint.server.clone(), address)),
                    "endpoint addresses must be unique per server"
                );
            }
            assert!(!endpoint.backend_addresses.contains(&endpoint.address));
        }
    }

    #[test]
    fn adding_identity_in_another_bucket_does_not_change_existing_assignment() {
        let total_slots = 64;
        let capacity = 4;
        let original = "service-0".to_string();
        let bucket_count = total_slots / capacity as u64;
        let original_bucket = stable_hash(original.as_bytes()) % bucket_count;
        let other = (1..10_000)
            .map(|index| format!("service-{index}"))
            .find(|identity| stable_hash(identity.as_bytes()) % bucket_count != original_bucket)
            .unwrap();

        let before = allocate_bucketed(
            std::slice::from_ref(&original),
            total_slots,
            capacity,
            "test",
        )
        .unwrap();
        let after =
            allocate_bucketed(&[original.clone(), other], total_slots, capacity, "test").unwrap();
        assert_eq!(before[&original], after[&original]);
    }

    #[test]
    fn changes_in_one_bucket_cannot_change_another_bucket() {
        let total_slots = 64;
        let capacity = 4;
        let unaffected = "unaffected".to_string();
        let bucket_count = total_slots / capacity as u64;
        let unaffected_bucket = stable_hash(unaffected.as_bytes()) % bucket_count;
        let target_bucket = (unaffected_bucket + 1) % bucket_count;
        let colliding: Vec<String> = (0..100_000)
            .map(|index| format!("candidate-{index}"))
            .filter(|identity| stable_hash(identity.as_bytes()) % bucket_count == target_bucket)
            .take(3)
            .collect();

        let mut before_identities = vec![unaffected.clone()];
        before_identities.extend(colliding[..2].iter().cloned());
        let before = allocate_bucketed(&before_identities, total_slots, capacity, "test").unwrap();
        let mut after_identities = before_identities;
        after_identities.push(colliding[2].clone());
        let after = allocate_bucketed(&after_identities, total_slots, capacity, "test").unwrap();
        assert_eq!(before[&unaffected], after[&unaffected]);
    }

    #[test]
    fn bucket_exhaustion_is_reported_even_with_capacity_elsewhere() {
        let total_slots = 16;
        let capacity = 2;
        let bucket_count = total_slots / capacity as u64;
        let target_bucket = 0;
        let identities: Vec<String> = (0..100_000)
            .map(|index| format!("collision-{index}"))
            .filter(|identity| stable_hash(identity.as_bytes()) % bucket_count == target_bucket)
            .take(capacity + 1)
            .collect();

        let error = allocate_bucketed(&identities, total_slots, capacity, "test").unwrap_err();
        assert_eq!(
            error,
            NetworkPlanError::BucketExhausted {
                kind: "test",
                bucket: target_bucket,
                capacity,
            }
        );
    }

    #[test]
    fn rejects_invalid_and_overlapping_ranges() {
        let overlapping = config(
            r#"
project: demo
builder: { engine: docker }
servers: {}
services: {}
network:
  management_cidr: 10.210.0.0/16
  container_cidr: 10.128.0.0/9
"#,
        );
        assert!(matches!(
            NetworkPlanner::new().plan(&overlapping),
            Err(NetworkPlanError::OverlappingAddressSpaces { .. })
        ));

        let invalid = config(
            r#"
project: demo
builder: { engine: docker }
servers: {}
services: {}
network:
  management_cidr: not-a-cidr
  container_cidr: 10.0.0.0/9
"#,
        );
        assert!(matches!(
            NetworkPlanner::new().plan(&invalid),
            Err(NetworkPlanError::InvalidCidr { .. })
        ));
    }

    #[test]
    fn rejects_ranges_without_required_capacity() {
        let too_small_container = config(
            r#"
project: demo
builder: { engine: docker }
servers: {}
services: {}
network:
  management_cidr: 192.0.2.0/24
  container_cidr: 10.0.0.0/24
"#,
        );
        assert!(matches!(
            NetworkPlanner::new().plan(&too_small_container),
            Err(NetworkPlanError::ContainerRangeTooSmall { .. })
        ));

        let too_small_management = config(
            r#"
project: demo
builder: { engine: docker }
servers: {}
services: {}
network:
  management_cidr: 192.0.2.0/30
  container_cidr: 10.0.0.0/16
"#,
        );
        assert!(matches!(
            NetworkPlanner::new().plan(&too_small_management),
            Err(NetworkPlanError::ManagementRangeTooSmall { .. })
        ));
    }

    #[test]
    fn wireguard_bridge_and_port_are_project_scoped_shared_across_servers_and_change_on_rename() {
        let plan = NetworkPlanner::new().plan(&base_config()).unwrap();
        let app = &plan.servers["app"];
        let data = &plan.servers["data"];

        assert_eq!(app.wireguard_interface, data.wireguard_interface);
        assert_eq!(app.bridge_interface, data.bridge_interface);
        assert_eq!(app.bridge_name, data.bridge_name);
        assert_eq!(app.wireguard_port, data.wireguard_port);
        assert_eq!(
            app.wireguard_interface,
            naming::wireguard_interface_name("demo")
        );
        assert_eq!(app.bridge_interface, naming::bridge_interface_name("demo"));
        assert_eq!(app.bridge_name, naming::bridge_network_name("demo"));
        assert_eq!(app.wireguard_port, naming::wireguard_port("demo"));

        let mut renamed = base_config();
        renamed.project = "other".to_string();
        let renamed_plan = NetworkPlanner::new().plan(&renamed).unwrap();
        let renamed_app = &renamed_plan.servers["app"];
        assert_ne!(app.wireguard_interface, renamed_app.wireguard_interface);
        assert_ne!(app.bridge_interface, renamed_app.bridge_interface);
        assert_ne!(app.bridge_name, renamed_app.bridge_name);
    }

    #[test]
    fn two_projects_with_identical_default_cidrs_usually_produce_different_server_subnets() {
        // Not a guarantee (see the project's network-isolation design notes for the real,
        // acknowledged collision odds) -- this is a spot check that the widened bucket key
        // actually varies the result per project, not a proof of collision-freedom.
        let a = config(
            r#"
project: project-a
builder: { engine: docker }
servers:
  app: { host: 203.0.113.10 }
services: {}
"#,
        );
        let b = config(
            r#"
project: project-b
builder: { engine: docker }
servers:
  app: { host: 203.0.113.10 }
services: {}
"#,
        );
        let plan_a = NetworkPlanner::new().plan(&a).unwrap();
        let plan_b = NetworkPlanner::new().plan(&b).unwrap();
        assert_ne!(
            plan_a.servers["app"].container_subnet,
            plan_b.servers["app"].container_subnet
        );
    }

    #[test]
    fn widened_server_slot_key_preserves_bucket_stability_across_projects() {
        // A second project's servers must never be able to reslot a first project's server --
        // trivially true since `plan()` never sees another project's config, but this guards
        // against a future refactor accidentally introducing any shared state between plans.
        let a = base_config();
        let plan_a = NetworkPlanner::new().plan(&a).unwrap();

        let unrelated = config(
            r#"
project: totally-unrelated
builder: { engine: docker }
servers:
  app: { host: 198.51.100.5 }
  extra: { host: 198.51.100.6 }
  more: { host: 198.51.100.7 }
services: {}
"#,
        );
        let _ = NetworkPlanner::new().plan(&unrelated).unwrap();
        let plan_a_again = NetworkPlanner::new().plan(&a).unwrap();
        assert_eq!(
            plan_a.servers["app"].container_subnet,
            plan_a_again.servers["app"].container_subnet
        );
    }

    #[test]
    fn rejects_unknown_and_duplicate_service_hosts() {
        let unknown = config(
            r#"
project: demo
builder: { engine: docker }
servers:
  app: { host: 203.0.113.10 }
services:
  web:
    servers: [missing]
"#,
        );
        assert!(matches!(
            NetworkPlanner::new().plan(&unknown),
            Err(NetworkPlanError::UnknownServiceServer { .. })
        ));

        let duplicate = config(
            r#"
project: demo
builder: { engine: docker }
servers:
  app: { host: 203.0.113.10 }
services:
  web:
    servers: [app, app]
"#,
        );
        assert!(matches!(
            NetworkPlanner::new().plan(&duplicate),
            Err(NetworkPlanError::DuplicateServiceServer { .. })
        ));
    }
}
