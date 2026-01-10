/**
 * DNS server utilities for service discovery
 *
 * Manages jiji-dns installation and configuration for resolving
 * service names via Corrosion database subscriptions.
 */

import type { SSHManager } from "../../utils/ssh.ts";
import { log } from "../../utils/logger.ts";
import type { DNSConfig } from "../../types/network.ts";

const JIJI_DNS_INSTALL_DIR = "/opt/jiji/dns";

// jiji-dns GitHub releases URL
const JIJI_DNS_REPO = "acidtib/jiji-dns";
const JIJI_DNS_VERSION = "0.3.2";

/**
 * Install jiji-dns on a remote server
 *
 * Downloads pre-built binary from GitHub releases.
 *
 * @param ssh - SSH connection to the server
 * @returns True if installation was successful
 */
export async function installJijiDns(ssh: SSHManager): Promise<boolean> {
  const host = ssh.getHost();

  // Check if jiji-dns is already installed
  const checkResult = await ssh.executeCommand(
    `test -f ${JIJI_DNS_INSTALL_DIR}/jiji-dns && echo "exists"`,
  );

  if (checkResult.stdout.includes("exists")) {
    log.debug(`jiji-dns already installed on ${host}`, "dns");
    return true;
  }

  // Create installation directory
  await ssh.executeCommand(`mkdir -p ${JIJI_DNS_INSTALL_DIR}`);

  // Detect architecture
  const archResult = await ssh.executeCommand("uname -m");
  const arch = archResult.stdout.trim();

  let downloadArch: string;
  if (arch === "x86_64" || arch === "amd64") {
    downloadArch = "linux-x64";
  } else if (arch === "aarch64" || arch === "arm64") {
    downloadArch = "linux-arm64";
  } else {
    log.error(`Unsupported architecture: ${arch}`, "dns");
    return false;
  }

  try {
    const downloadUrl =
      `https://github.com/${JIJI_DNS_REPO}/releases/download/v${JIJI_DNS_VERSION}/jiji-dns-${downloadArch}`;

    log.info(`Downloading jiji-dns from ${downloadUrl}...`, "dns");

    const downloadResult = await ssh.executeCommand(
      `cd ${JIJI_DNS_INSTALL_DIR} && curl -fsSL "${downloadUrl}" -o jiji-dns`,
    );

    if (downloadResult.code !== 0) {
      throw new Error(`Failed to download jiji-dns: ${downloadResult.stderr}`);
    }

    // Make executable
    await ssh.executeCommand(`chmod +x ${JIJI_DNS_INSTALL_DIR}/jiji-dns`);

    log.success(`jiji-dns installed on ${host}`, "dns");
    return true;
  } catch (error) {
    log.error(`Failed to install jiji-dns on ${host}: ${error}`, "dns");
    return false;
  }
}

/**
 * Create systemd service for jiji-dns
 *
 * @param ssh - SSH connection to the server
 * @param config - DNS configuration
 */
export async function createJijiDnsService(
  ssh: SSHManager,
  config: DNSConfig,
): Promise<void> {
  const serviceContent = `[Unit]
Description=Jiji DNS Server with Corrosion Subscription
After=network.target jiji-corrosion.service
Requires=jiji-corrosion.service

[Service]
Type=simple
ExecStart=${JIJI_DNS_INSTALL_DIR}/jiji-dns
Environment="CORROSION_API=${config.corrosionApiAddr}"
Environment="LISTEN_ADDR=${config.listenAddr}"
Environment="SERVICE_DOMAIN=${config.serviceDomain}"
Environment="DNS_TTL=60"
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
    throw new Error(
      `Failed to create jiji-dns service: ${serviceResult.stderr}`,
    );
  }

  // Reload systemd
  await ssh.executeCommand("systemctl daemon-reload");
}

/**
 * Start jiji-dns service
 *
 * @param ssh - SSH connection to the server
 */
export async function startJijiDnsService(ssh: SSHManager): Promise<void> {
  // Start and enable jiji-dns
  const result = await ssh.executeCommand("systemctl enable --now jiji-dns");

  if (result.code !== 0) {
    throw new Error(`Failed to start jiji-dns service: ${result.stderr}`);
  }

  // Wait a moment for service to start
  await new Promise((resolve) => setTimeout(resolve, 2000));
}

/**
 * Stop jiji-dns service
 *
 * @param ssh - SSH connection to the server
 */
export async function stopJijiDnsService(ssh: SSHManager): Promise<void> {
  await ssh.executeCommand("systemctl stop jiji-dns");
}

/**
 * Check if jiji-dns service is running
 *
 * @param ssh - SSH connection to the server
 * @returns True if service is active
 */
export async function isJijiDnsRunning(ssh: SSHManager): Promise<boolean> {
  const result = await ssh.executeCommand("systemctl is-active jiji-dns");
  return result.stdout.trim() === "active";
}

/**
 * Register a container hostname in system DNS immediately
 *
 * Note: With jiji-dns, DNS updates happen automatically via Corrosion subscription.
 * This function only logs the registration for visibility.
 *
 * @param serviceName - Service name
 * @param projectName - Project name
 * @param containerIp - Container IP address
 * @param instanceId - Optional instance identifier
 */
export function registerContainerHostname(
  serviceName: string,
  projectName: string,
  containerIp: string,
  instanceId?: string,
): void {
  const baseDomain = `${projectName}-${serviceName}.jiji`;
  if (instanceId) {
    const instanceDomain = `${projectName}-${serviceName}-${instanceId}.jiji`;
    log.say(
      `├── Registered ${baseDomain} -> ${containerIp} in jiji-dns`,
      2,
    );
    log.say(
      `├── Registered ${instanceDomain} -> ${containerIp} in jiji-dns`,
      2,
    );
  } else {
    log.say(
      `├── Registered ${baseDomain} -> ${containerIp} in jiji-dns`,
      2,
    );
  }
}

/**
 * Unregister a container hostname from system DNS
 *
 * Note: With jiji-dns, DNS entries are automatically removed when containers
 * are unregistered from Corrosion database via subscription.
 *
 * @param serviceName - Service name
 * @param projectName - Project name
 */
export function unregisterContainerHostname(
  _serviceName: string,
  _projectName?: string,
): void {
  // jiji-dns entries are automatically removed when containers are
  // unregistered from Corrosion database via real-time subscription
}

/**
 * Configure container engine to use custom DNS
 *
 * This configures DNS at the daemon level so ALL containers (on any network)
 * can use service discovery, not just containers on the jiji network.
 *
 * @param ssh - SSH connection to the server
 * @param dnsServer - DNS server IP (container gateway IP, e.g., 10.210.128.1)
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
      dns: [dnsServer],
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
dns_servers = ["${dnsServer}"]
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
