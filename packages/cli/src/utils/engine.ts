import type { SSHManager } from "./ssh.ts";
import { getSSHTroubleshootingTips } from "./ssh.ts";
import { executeHostOperations } from "./promise_helpers.ts";
import { log } from "./logger.ts";
import {
  DOCKER_MIN_VERSION,
  DOCKER_VERSION,
  PODMAN_MIN_VERSION,
} from "../constants.ts";

export interface EngineInstallResult {
  success: boolean;
  host: string;
  output: string;
  error?: string;
  version?: string;
  message?: string;
}

/**
 * Parse a version string into comparable parts
 * Handles formats like "28.2.0", "podman version 4.9.3", "Docker version 28.2.0, build xyz"
 */
function parseVersion(versionString: string): number[] {
  // Extract version number pattern (e.g., "28.2.0")
  const match = versionString.match(/(\d+)\.(\d+)\.(\d+)/);
  if (!match) return [0, 0, 0];
  return [parseInt(match[1]), parseInt(match[2]), parseInt(match[3])];
}

/**
 * Compare two version strings
 * Returns: -1 if v1 < v2, 0 if equal, 1 if v1 > v2
 */
function compareVersions(v1: string, v2: string): number {
  const parts1 = parseVersion(v1);
  const parts2 = parseVersion(v2);

  for (let i = 0; i < 3; i++) {
    if (parts1[i] < parts2[i]) return -1;
    if (parts1[i] > parts2[i]) return 1;
  }
  return 0;
}

/**
 * Check if a version meets the minimum requirement
 */
function meetsMinVersion(current: string, minimum: string): boolean {
  return compareVersions(current, minimum) >= 0;
}

/**
 * Container engine installer
 */
export class EngineInstaller {
  private ssh: SSHManager;

  constructor(ssh: SSHManager) {
    this.ssh = ssh;
  }

  /**
   * Check if a container engine is already installed
   */
  async isEngineInstalled(engine: "podman" | "docker"): Promise<boolean> {
    const result = await this.ssh.executeCommand(`which ${engine}`);
    return result.success;
  }

  /**
   * Get the version of an installed engine
   */
  async getEngineVersion(engine: "podman" | "docker"): Promise<string | null> {
    const result = await this.ssh.executeCommand(`${engine} --version`);
    return result.success ? result.stdout.trim() : null;
  }

  /**
   * Install Podman on the remote host
   * Installs the specific version defined in PODMAN_VERSION constant
   */
  async installPodman(): Promise<EngineInstallResult> {
    const host = this.ssh.getHost();
    let fullOutput = "";
    let hasError = false;
    let errorMessage = "";

    // Detect OS
    const osResult = await this.ssh.executeCommand(
      "cat /etc/os-release | grep -E '^(ID|VERSION_ID)=' | tr '\\n' ' '",
    );
    const osInfo = osResult.stdout.toLowerCase();

    // Install Podman from official repos with version pinning
    // Ubuntu 24.04+ has Podman 4.9+ in repos
    const commands: string[] = [];

    if (osInfo.includes("ubuntu") || osInfo.includes("debian")) {
      commands.push(
        "export DEBIAN_FRONTEND=noninteractive",
        "apt-get update -qq",
        // Install podman - Ubuntu 24.04 has 4.9.3 in repos
        "apt-get install -y -qq podman curl git",
      );
    } else if (
      osInfo.includes("fedora") || osInfo.includes("centos") ||
      osInfo.includes("rhel")
    ) {
      commands.push(
        "dnf install -y podman curl git",
      );
    } else {
      return {
        success: false,
        host,
        output: "",
        error: `Unsupported OS for Podman installation. Detected: ${osInfo}`,
        message: "Unsupported OS for Podman installation",
      };
    }

    for (const command of commands) {
      log.debug(`Running: ${command}`, "engine");
      const result = await this.ssh.executeCommand(command);

      fullOutput += `$ ${command}\n${result.stdout}\n`;
      if (result.stderr) {
        fullOutput += `STDERR: ${result.stderr}\n`;
      }

      if (!result.success) {
        hasError = true;
        errorMessage = result.stderr || "Command failed";
        break;
      }
    }

    // Ensure containers directory exists and create default policy.json if missing
    if (!hasError) {
      const ensurePolicyResult = await this.ssh.executeCommand(
        `mkdir -p /etc/containers && test -f /etc/containers/policy.json || echo '{"default":[{"type":"insecureAcceptAnything"}]}' | tee /etc/containers/policy.json > /dev/null`,
      );
      if (!ensurePolicyResult.success) {
        log.warn(
          `Warning: Could not ensure policy.json exists: ${ensurePolicyResult.stderr}`,
          "engine",
        );
        fullOutput += `\nWarning: ${ensurePolicyResult.stderr}\n`;
      } else {
        fullOutput += `\n$ Created /etc/containers/policy.json\n`;
      }
    }

    // Verify installation and check version
    let version: string | undefined;
    if (!hasError) {
      const verifyResult = await this.ssh.executeCommand("podman --version");
      if (verifyResult.success) {
        version = verifyResult.stdout.trim();
        fullOutput += `\n$ podman --version\n${version}\n`;

        // Validate minimum version
        if (!meetsMinVersion(version, PODMAN_MIN_VERSION)) {
          hasError = true;
          errorMessage =
            `Podman version ${version} does not meet minimum requirement ${PODMAN_MIN_VERSION}`;
          fullOutput += `\nError: ${errorMessage}\n`;
        }
      } else {
        hasError = true;
        errorMessage = "Podman installation verification failed";
        fullOutput += `\nVerification failed: ${verifyResult.stderr}\n`;
      }
    }

    return {
      success: !hasError,
      host,
      output: fullOutput,
      error: hasError ? errorMessage : undefined,
      version,
      message: hasError
        ? `Podman installation failed: ${errorMessage}`
        : `Podman installed successfully (${version})`,
    };
  }

  /**
   * Install Docker on the remote host
   * Installs the specific version defined in DOCKER_VERSION constant from Docker's official repo
   */
  async installDocker(): Promise<EngineInstallResult> {
    const host = this.ssh.getHost();
    let fullOutput = "";
    let hasError = false;
    let errorMessage = "";

    // Detect OS
    const osResult = await this.ssh.executeCommand(
      "cat /etc/os-release | grep -E '^(ID|VERSION_CODENAME)=' | tr '\\n' ' '",
    );
    const osInfo = osResult.stdout.toLowerCase();
    fullOutput += `OS detected: ${osInfo}\n`;

    // Install Docker from official Docker repo for version control
    const commands: string[] = [];

    if (osInfo.includes("ubuntu") || osInfo.includes("debian")) {
      // Extract codename (e.g., "noble" for Ubuntu 24.04)
      const codenameMatch = osResult.stdout.match(/VERSION_CODENAME=(\w+)/i);
      const codename = codenameMatch ? codenameMatch[1].toLowerCase() : "noble";
      const distro = osInfo.includes("ubuntu") ? "ubuntu" : "debian";

      commands.push(
        // Install prerequisites
        "export DEBIAN_FRONTEND=noninteractive",
        "apt-get update -qq",
        "apt-get install -y -qq ca-certificates curl gnupg",
        // Add Docker's official GPG key
        "install -m 0755 -d /etc/apt/keyrings",
        `curl -fsSL https://download.docker.com/linux/${distro}/gpg -o /etc/apt/keyrings/docker.asc`,
        "chmod a+r /etc/apt/keyrings/docker.asc",
        // Add Docker repo
        `echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/${distro} ${codename} stable" | tee /etc/apt/sources.list.d/docker.list > /dev/null`,
        "apt-get update -qq",
        // Install Docker with specific version
        `apt-get install -y -qq docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin curl git`,
      );
    } else if (osInfo.includes("fedora")) {
      commands.push(
        "dnf -y install dnf-plugins-core",
        "dnf config-manager --add-repo https://download.docker.com/linux/fedora/docker-ce.repo",
        "dnf install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin curl git",
        "systemctl start docker",
        "systemctl enable docker",
      );
    } else if (osInfo.includes("centos") || osInfo.includes("rhel")) {
      commands.push(
        "yum install -y yum-utils",
        "yum-config-manager --add-repo https://download.docker.com/linux/centos/docker-ce.repo",
        "yum install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin curl git",
        "systemctl start docker",
        "systemctl enable docker",
      );
    } else {
      return {
        success: false,
        host,
        output: fullOutput,
        error: `Unsupported OS for Docker installation. Detected: ${osInfo}`,
        message: "Unsupported OS for Docker installation",
      };
    }

    for (const command of commands) {
      log.debug(`Running: ${command}`, "engine");
      const result = await this.ssh.executeCommand(command);

      fullOutput += `$ ${command}\n${result.stdout}\n`;
      if (result.stderr) {
        fullOutput += `STDERR: ${result.stderr}\n`;
      }

      if (!result.success) {
        hasError = true;
        errorMessage = result.stderr || "Command failed";
        break;
      }
    }

    // Ensure Docker is running
    if (!hasError) {
      await this.ssh.executeCommand("systemctl start docker");
      await this.ssh.executeCommand("systemctl enable docker");
    }

    // Verify installation and check version
    let version: string | undefined;
    if (!hasError) {
      const verifyResult = await this.ssh.executeCommand("docker --version");
      if (verifyResult.success) {
        version = verifyResult.stdout.trim();
        fullOutput += `\n$ docker --version\n${version}\n`;

        // Validate minimum version
        if (!meetsMinVersion(version, DOCKER_MIN_VERSION)) {
          hasError = true;
          errorMessage =
            `Docker version ${version} does not meet minimum requirement ${DOCKER_MIN_VERSION}. ` +
            `Please upgrade Docker to ${DOCKER_VERSION} or later.`;
          fullOutput += `\nError: ${errorMessage}\n`;
        }
      } else {
        hasError = true;
        errorMessage = "Docker installation verification failed";
        fullOutput += `\nVerification failed: ${verifyResult.stderr}\n`;
      }
    }

    return {
      success: !hasError,
      host,
      output: fullOutput,
      error: hasError ? errorMessage : undefined,
      version,
      message: hasError
        ? `Docker installation failed: ${errorMessage}`
        : `Docker installed successfully (${version})`,
    };
  }

  /**
   * Install the specified container engine
   * Also validates that existing installations meet minimum version requirements
   */
  async installEngine(
    engine: "podman" | "docker",
  ): Promise<EngineInstallResult> {
    const host = this.ssh.getHost();
    const minVersion = engine === "docker"
      ? DOCKER_MIN_VERSION
      : PODMAN_MIN_VERSION;

    // Check if already installed
    const isInstalled = await this.isEngineInstalled(engine);
    if (isInstalled) {
      const version = await this.getEngineVersion(engine);

      // Validate version meets minimum requirement
      if (version && !meetsMinVersion(version, minVersion)) {
        return {
          success: false,
          host,
          output:
            `${engine} is installed but version ${version} does not meet minimum requirement ${minVersion}`,
          version: version,
          error:
            `${engine} version ${version} is below minimum ${minVersion}. Please upgrade.`,
          message: `${engine} version too old (${version} < ${minVersion})`,
        };
      }

      return {
        success: true,
        host,
        output: `${engine} is already installed: ${version}`,
        version: version || undefined,
        message: `${engine} already installed (${version})`,
      };
    }

    // Install the engine
    switch (engine) {
      case "podman":
        return await this.installPodman();
      case "docker":
        return await this.installDocker();
      default:
        return {
          success: false,
          host,
          output: "",
          error: `Unsupported engine: ${engine}`,
          message: `Unsupported engine: ${engine}`,
        };
    }
  }
}

/**
 * Install container engine on multiple hosts with enhanced error collection
 */
export async function installEngineOnHosts(
  sshManagers: SSHManager[],
  engine: "podman" | "docker",
  tracker?: ReturnType<typeof log.createStepTracker>,
): Promise<EngineInstallResult[]> {
  // Create host operations for error collection
  const hostOperations = sshManagers.map((ssh) => ({
    host: ssh.getHost(),
    operation: async () => {
      const installer = new EngineInstaller(ssh);
      const host = ssh.getHost();

      try {
        if (!ssh.isConnected()) {
          await ssh.connect();
          tracker?.remote(host, "Connected");
        }

        const result = await installer.installEngine(engine);
        return result;
      } catch (error) {
        const errorMessage = error instanceof Error
          ? error.message
          : String(error);
        const troubleshootingTips = getSSHTroubleshootingTips(errorMessage);

        tracker?.remote(host, `Connection failed: ${errorMessage}`);
        if (troubleshootingTips.length > 0) {
          log.warn("Troubleshooting suggestions:");
          troubleshootingTips.forEach((tip) => log.say(tip, 1));
        }

        // Return a failed result instead of throwing
        return {
          success: false,
          host,
          output: "",
          error: errorMessage,
          message: `Connection failed: ${errorMessage}`,
        } as EngineInstallResult;
      }
    },
  }));

  // Execute with error collection
  const aggregatedResults = await executeHostOperations(hostOperations);

  // Extract the actual installation results
  const results = aggregatedResults.results;

  // Handle any operation-level errors (shouldn't occur with current implementation)
  if (aggregatedResults.hostErrors.length > 0) {
    for (const { host, error } of aggregatedResults.hostErrors) {
      results.push({
        success: false,
        host,
        output: "",
        error: error.message,
        message: `Operation failed: ${error.message}`,
      });
    }
  }

  return results;
}

/**
 * Check if container engine is available on multiple hosts with error collection
 */
export async function checkEngineOnHosts(
  sshManagers: SSHManager[],
  engine: "podman" | "docker",
): Promise<{ host: string; available: boolean; version?: string }[]> {
  const hostOperations = sshManagers.map((ssh) => ({
    host: ssh.getHost(),
    operation: async () => {
      const installer = new EngineInstaller(ssh);
      const host = ssh.getHost();

      if (!ssh.isConnected()) {
        await ssh.connect();
      }

      const available = await installer.isEngineInstalled(engine);
      const version = available
        ? await installer.getEngineVersion(engine)
        : undefined;

      return {
        host,
        available,
        version: version || undefined,
      };
    },
  }));

  const aggregatedResults = await executeHostOperations(hostOperations);
  const results = aggregatedResults.results;

  for (const { host } of aggregatedResults.hostErrors) {
    results.push({
      host,
      available: false,
      version: undefined,
    });
  }

  if (aggregatedResults.errorCount > 0) {
    log.warn(
      `Engine check completed with ${aggregatedResults.errorCount} connection failures - treating as engine not available`,
      "engine",
    );
  }

  return results;
}
