/**
 * DNS server utilities for service discovery
 *
 * Manages CoreDNS or dnsmasq installation and configuration
 * for resolving service names via Corrosion database.
 */

import type { SSHManager } from "../../utils/ssh.ts";
import { log } from "../../utils/logger.ts";
import type { DNSConfig } from "../../types/network.ts";

const COREDNS_INSTALL_DIR = "/opt/jiji/dns";
const COREDNS_VERSION = "1.11.1";

/**
 * Install CoreDNS on a remote server
 *
 * @param ssh - SSH connection to the server
 * @returns True if installation was successful
 */
export async function installCoreDNS(ssh: SSHManager): Promise<boolean> {
  const host = ssh.getHost();

  // Check if CoreDNS is already installed
  const checkResult = await ssh.executeCommand(
    `test -f ${COREDNS_INSTALL_DIR}/coredns && echo "exists"`,
  );

  if (checkResult.stdout.includes("exists")) {
    return true;
  }

  // Create installation directory
  await ssh.executeCommand(`mkdir -p ${COREDNS_INSTALL_DIR}`);

  // Detect architecture
  const archResult = await ssh.executeCommand("uname -m");
  const arch = archResult.stdout.trim();

  let downloadArch: string;
  if (arch === "x86_64" || arch === "amd64") {
    downloadArch = "amd64";
  } else if (arch === "aarch64" || arch === "arm64") {
    downloadArch = "arm64";
  } else {
    log.error(`Unsupported architecture: ${arch}`, "dns");
    return false;
  }

  try {
    const downloadUrl =
      `https://github.com/coredns/coredns/releases/download/v${COREDNS_VERSION}/coredns_${COREDNS_VERSION}_linux_${downloadArch}.tgz`;

    const downloadResult = await ssh.executeCommand(
      `cd ${COREDNS_INSTALL_DIR} && curl -fsSL "${downloadUrl}" -o coredns.tgz`,
    );

    if (downloadResult.code !== 0) {
      throw new Error(`Failed to download CoreDNS: ${downloadResult.stderr}`);
    }

    // Extract
    const extractResult = await ssh.executeCommand(
      `cd ${COREDNS_INSTALL_DIR} && tar -xzf coredns.tgz && rm coredns.tgz`,
    );

    if (extractResult.code !== 0) {
      throw new Error(`Failed to extract CoreDNS: ${extractResult.stderr}`);
    }

    // Make executable
    await ssh.executeCommand(`chmod +x ${COREDNS_INSTALL_DIR}/coredns`);

    return true;
  } catch (error) {
    log.error(`Failed to install CoreDNS on ${host}: ${error}`, "dns");
    return false;
  }
}

/**
 * Generate CoreDNS Corefile configuration
 *
 * Uses a simple hosts file approach that is regenerated periodically
 * from Corrosion database.
 *
 * @param config - DNS configuration
 * @returns Corefile content
 */
export function generateCorefileConfig(config: DNSConfig): string {
  return `# CoreDNS configuration for Jiji service discovery

${config.serviceDomain} {
    # Bind to specific address to avoid port conflicts
    bind ${config.listenAddr.split(":")[0]}

    # Serve service records from hosts file
    hosts ${COREDNS_INSTALL_DIR}/hosts {
        fallthrough
    }

    # Cache DNS responses
    cache 30

    # Log queries
    log

    # Error handling
    errors
}

# Handle system-wide container hostnames (fallback to system hosts)
. {
    # Bind to specific address to avoid port conflicts
    bind ${config.listenAddr.split(":")[0]}

    hosts /etc/hosts {
        fallthrough
    }

    forward . ${config.upstreamResolvers.join(" ")}
    cache 30
    log
    errors
}
`;
}

/**
 * Generate a script to update DNS hosts file from Corrosion
 *
 * @param serviceDomain - Service domain (e.g., "jiji")
 * @param corrosionApiAddr - Corrosion API address
 * @returns Script content
 */
export function generateHostsUpdateScript(
  serviceDomain: string,
  _corrosionApiAddr: string,
): string {
  return `#!/bin/bash
# Update DNS hosts file from Corrosion database

HOSTS_FILE="${COREDNS_INSTALL_DIR}/hosts"
TEMP_FILE="\${HOSTS_FILE}.tmp"
SYSTEM_HOSTS_FILE="/etc/hosts"
SYSTEM_TEMP_FILE="/etc/hosts.jiji.tmp"

# Clear temp files
> "\$TEMP_FILE"
cp "\$SYSTEM_HOSTS_FILE" "\$SYSTEM_TEMP_FILE"

# Remove old Jiji entries from system hosts
sed -i '/# Jiji container hostnames/,/# End Jiji container hostnames/d' "\$SYSTEM_TEMP_FILE"

# Add header to system hosts
echo "# Jiji container hostnames" >> "\$SYSTEM_TEMP_FILE"

# Query Corrosion for all healthy containers with service and project info
/opt/jiji/corrosion/corrosion query --config /opt/jiji/corrosion/config.toml "
  SELECT s.project || '|' || c.service || '|' || c.ip || '|' || c.id
  FROM containers c
  JOIN services s ON c.service = s.name
  WHERE c.healthy = 1;
" 2>/dev/null | while IFS='|' read -r project service ip container_id; do
  if [ -n "\$project" ] && [ -n "\$service" ] && [ -n "\$ip" ] && [ -n "\$container_id" ]; then
    # For CoreDNS (project-service discovery domain) - ONLY FORMAT NEEDED
    echo "\$ip \${project}-\${service}.${serviceDomain}" >> "\$TEMP_FILE"
  fi
done

# Add footer to system hosts
echo "# End Jiji container hostnames" >> "\$SYSTEM_TEMP_FILE"

# Atomic replace for both files
if [ -s "\$TEMP_FILE" ]; then
  mv "\$TEMP_FILE" "\$HOSTS_FILE"
  mv "\$SYSTEM_TEMP_FILE" "\$SYSTEM_HOSTS_FILE"
  echo "Updated DNS hosts: \$(wc -l < "\$HOSTS_FILE") entries"
else
  # Keep old files if query failed
  rm -f "\$TEMP_FILE" "\$SYSTEM_TEMP_FILE"
  echo "No containers found or query failed"
fi
`;
}

/**
 * Write CoreDNS configuration to a remote server
 *
 * @param ssh - SSH connection to the server
 * @param config - DNS configuration
 */
export async function writeCoreDNSConfig(
  ssh: SSHManager,
  config: DNSConfig,
): Promise<void> {
  const corefileContent = generateCorefileConfig(config);
  const corefilePath = `${COREDNS_INSTALL_DIR}/Corefile`;

  // Ensure directory exists
  await ssh.executeCommand(`mkdir -p ${COREDNS_INSTALL_DIR}`);

  // Write Corefile
  const writeResult = await ssh.executeCommand(
    `cat > ${corefilePath} << 'EOFCORE'\n${corefileContent}\nEOFCORE`,
  );

  if (writeResult.code !== 0) {
    throw new Error(`Failed to write CoreDNS config: ${writeResult.stderr}`);
  }

  // Create empty hosts file initially
  await ssh.executeCommand(`touch ${COREDNS_INSTALL_DIR}/hosts`);

  // Write hosts update script
  const updateScript = generateHostsUpdateScript(
    config.serviceDomain,
    config.corrosionApiAddr,
  );
  const scriptPath = `${COREDNS_INSTALL_DIR}/update-hosts.sh`;

  const scriptResult = await ssh.executeCommand(
    `cat > ${scriptPath} << 'EOFSCRIPT'\n${updateScript}\nEOFSCRIPT`,
  );

  if (scriptResult.code !== 0) {
    throw new Error(
      `Failed to write hosts update script: ${scriptResult.stderr}`,
    );
  }

  // Make script executable
  await ssh.executeCommand(`chmod +x ${scriptPath}`);
}

/**
 * Create systemd service for CoreDNS
 *
 * @param ssh - SSH connection to the server
 * @param listenAddr - Address to listen on (e.g., "10.210.1.1:53")
 */
export async function createCoreDNSService(
  ssh: SSHManager,
  _listenAddr: string,
): Promise<void> {
  const serviceContent = `[Unit]
Description=CoreDNS for Jiji service discovery
After=network.target jiji-corrosion.service
Requires=jiji-corrosion.service

[Service]
Type=simple
ExecStart=${COREDNS_INSTALL_DIR}/coredns -conf ${COREDNS_INSTALL_DIR}/Corefile -dns.port 53
Environment="COREFILE=${COREDNS_INSTALL_DIR}/Corefile"
Restart=always
RestartSec=5
# Allow binding to port 53
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
`;

  const serviceResult = await ssh.executeCommand(
    `cat > /etc/systemd/system/jiji-dns.service << 'EOFSVC'\n${serviceContent}\nEOFSVC`,
  );

  if (serviceResult.code !== 0) {
    throw new Error(`Failed to create DNS service: ${serviceResult.stderr}`);
  }

  // Reload systemd
  await ssh.executeCommand("systemctl daemon-reload");
}

/**
 * Create systemd timer for periodic hosts file updates
 *
 * @param ssh - SSH connection to the server
 * @param intervalSeconds - Update interval in seconds (default: 30)
 */
export async function createHostsUpdateTimer(
  ssh: SSHManager,
  intervalSeconds = 30,
): Promise<void> {
  // Create service unit
  const serviceContent = `[Unit]
Description=Update DNS hosts from Corrosion

[Service]
Type=oneshot
ExecStart=${COREDNS_INSTALL_DIR}/update-hosts.sh
`;

  await ssh.executeCommand(
    `cat > /etc/systemd/system/jiji-dns-update.service << 'EOFSVC'\n${serviceContent}\nEOFSVC`,
  );

  // Create timer unit
  const timerContent = `[Unit]
Description=Periodic DNS hosts update from Corrosion

[Timer]
OnBootSec=10s
OnUnitActiveSec=${intervalSeconds}s
AccuracySec=1s

[Install]
WantedBy=timers.target
`;

  await ssh.executeCommand(
    `cat > /etc/systemd/system/jiji-dns-update.timer << 'EOFTIMER'\n${timerContent}\nEOFTIMER`,
  );

  // Reload systemd
  await ssh.executeCommand("systemctl daemon-reload");
}

/**
 * Start CoreDNS service
 *
 * @param ssh - SSH connection to the server
 */
export async function startCoreDNSService(ssh: SSHManager): Promise<void> {
  // Start and enable the update timer first
  await ssh.executeCommand("systemctl enable --now jiji-dns-update.timer");

  // Run initial hosts update
  await ssh.executeCommand(`${COREDNS_INSTALL_DIR}/update-hosts.sh || true`);

  // Start and enable CoreDNS
  const result = await ssh.executeCommand("systemctl enable --now jiji-dns");

  if (result.code !== 0) {
    throw new Error(`Failed to start CoreDNS service: ${result.stderr}`);
  }

  // Wait a moment for service to start
  await new Promise((resolve) => setTimeout(resolve, 2000));
}

/**
 * Stop CoreDNS service
 *
 * @param ssh - SSH connection to the server
 */
export async function stopCoreDNSService(ssh: SSHManager): Promise<void> {
  await ssh.executeCommand("systemctl stop jiji-dns");
  await ssh.executeCommand("systemctl stop jiji-dns-update.timer");
}

/**
 * Check if CoreDNS service is running
 *
 * @param ssh - SSH connection to the server
 * @returns True if service is active
 */
export async function isCoreDNSRunning(ssh: SSHManager): Promise<boolean> {
  const result = await ssh.executeCommand("systemctl is-active jiji-dns");
  return result.stdout.trim() === "active";
}

/**
 * Trigger immediate DNS hosts update
 *
 * @param ssh - SSH connection to the server
 */
export async function triggerHostsUpdate(ssh: SSHManager): Promise<void> {
  const result = await ssh.executeCommand(
    `${COREDNS_INSTALL_DIR}/update-hosts.sh`,
  );

  if (result.code !== 0) {
    throw new Error(`Failed to update DNS hosts: ${result.stderr}`);
  }
}

/**
 * Register a container hostname in system DNS immediately
 *
 * @param serviceName - Service name
 * @param projectName - Project name
 * @param containerIp - Container IP address
 */
export function registerContainerHostname(
  serviceName: string,
  projectName: string,
  containerIp: string,
): void {
  log.success(
    `Registered ${projectName}-${serviceName}.jiji -> ${containerIp} in CoreDNS`,
    "dns",
  );
}

/**
 * Unregister a container hostname from system DNS
 *
 * @param serviceName - Service name
 * @param projectName - Project name
 */
export function unregisterContainerHostname(
  serviceName: string,
  projectName?: string,
): void {
  // CoreDNS entries are automatically removed when containers are
  // unregistered from Corrosion database

  log.say(
    `Unregistered ${projectName}-${serviceName}.jiji from CoreDNS`,
    3,
  );
}

/**
 * Configure container engine to use custom DNS
 *
 * This configures DNS at the daemon level so ALL containers (on any network)
 * can use service discovery, not just containers on the jiji network.
 *
 * @param ssh - SSH connection to the server
 * @param dnsServer - DNS server IP (WireGuard IP)
 * @param serviceDomain - Service domain for DNS search (default: "jiji")
 * @param engine - Container engine (docker or podman)
 */
export async function configureContainerDNS(
  ssh: SSHManager,
  dnsServer: string,
  serviceDomain: string,
  engine: "docker" | "podman",
): Promise<void> {
  if (engine === "docker") {
    // Read existing daemon.json if it exists
    const readResult = await ssh.executeCommand(
      "cat /etc/docker/daemon.json 2>/dev/null || echo '{}'",
    );

    let existingConfig: Record<string, unknown> = {};
    try {
      existingConfig = JSON.parse(readResult.stdout.trim() || "{}");
    } catch {
      log.warn("Failed to parse existing daemon.json, will overwrite", "dns");
    }

    // Merge DNS configuration with existing config
    const daemonConfig = {
      ...existingConfig,
      dns: [dnsServer, "8.8.8.8", "1.1.1.1"],
      "dns-search": [serviceDomain],
      "dns-opts": ["ndots:1"],
    };

    const configContent = JSON.stringify(daemonConfig, null, 2);

    // Create or update daemon.json
    await ssh.executeCommand(
      `mkdir -p /etc/docker && cat > /etc/docker/daemon.json << 'EOFJSON'\n${configContent}\nEOFJSON`,
    );

    // Reload Docker
    const reloadResult = await ssh.executeCommand(
      "systemctl reload docker 2>/dev/null || systemctl restart docker",
    );

    if (reloadResult.code !== 0) {
      log.warn(
        `Failed to reload Docker daemon: ${reloadResult.stderr}`,
        "dns",
      );
    }
  } else if (engine === "podman") {
    // Podman uses containers.conf
    const containersConf = `[network]
dns_servers = ["${dnsServer}", "8.8.8.8", "1.1.1.1"]
dns_searches = ["${serviceDomain}"]
dns_options = ["ndots:1"]
`;

    await ssh.executeCommand(
      `mkdir -p /etc/containers && cat > /etc/containers/containers.conf << 'EOFCONF'\n${containersConf}\nEOFCONF`,
    );

    // Restart kamal-proxy if it exists to pick up new DNS configuration
    const proxyCheck = await ssh.executeCommand(
      "podman ps --filter name=kamal-proxy --format '{{.Names}}'",
    );
    if (proxyCheck.stdout.trim() === "kamal-proxy") {
      log.info("Restarting kamal-proxy to apply DNS configuration...", "dns");
      await ssh.executeCommand("podman restart kamal-proxy");
      log.success("kamal-proxy restarted with new DNS configuration", "dns");
    }
  }
}
