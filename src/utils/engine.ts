import type { SSHManager } from "./ssh.ts";
import { getSSHTroubleshootingTips } from "./ssh.ts";

export interface EngineInstallResult {
  success: boolean;
  host: string;
  output: string;
  error?: string;
  version?: string;
  message?: string;
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
   */
  async installPodman(): Promise<EngineInstallResult> {
    const host = this.ssh.getHost();
    console.log(`📦 Installing Podman on ${host}...`);

    const commands = [
      "sudo apt update",
      "sudo apt-get -y install podman curl git",
    ];

    let fullOutput = "";
    let hasError = false;
    let errorMessage = "";

    for (const command of commands) {
      console.log(`  Running: ${command}`);
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

    // Verify installation
    let version: string | undefined;
    if (!hasError) {
      const verifyResult = await this.ssh.executeCommand("podman --version");
      if (verifyResult.success) {
        version = verifyResult.stdout.trim();
        console.log(`  ✅ Podman installed successfully: ${version}`);
        fullOutput += `\n$ podman --version\n${verifyResult.stdout}\n`;
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
        : `Podman installed successfully${version ? ` (${version})` : ""}`,
    };
  }

  /**
   * Install Docker on the remote host
   */
  async installDocker(): Promise<EngineInstallResult> {
    const host = this.ssh.getHost();
    console.log(`🐳 Installing Docker on ${host}...`);

    const commands = [
      "sudo apt update",
      "sudo apt install -y docker.io curl git",
      "sudo usermod -a -G docker $USER",
    ];

    let fullOutput = "";
    let hasError = false;
    let errorMessage = "";

    for (const command of commands) {
      console.log(`  Running: ${command}`);
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

    // Verify installation
    let version: string | undefined;
    if (!hasError) {
      const verifyResult = await this.ssh.executeCommand("docker --version");
      if (verifyResult.success) {
        version = verifyResult.stdout.trim();
        console.log(`  ✅ Docker installed successfully: ${version}`);
        fullOutput += `\n$ docker --version\n${verifyResult.stdout}\n`;

        // Note about group membership
        fullOutput +=
          `\nNote: User added to docker group. You may need to log out and back in for group changes to take effect.\n`;
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
        : `Docker installed successfully${version ? ` (${version})` : ""}`,
    };
  }

  /**
   * Install the specified container engine
   */
  async installEngine(
    engine: "podman" | "docker",
  ): Promise<EngineInstallResult> {
    const host = this.ssh.getHost();

    // Check if already installed
    const isInstalled = await this.isEngineInstalled(engine);
    if (isInstalled) {
      const version = await this.getEngineVersion(engine);
      return {
        success: true,
        host,
        output: `${engine} is already installed: ${version}`,
        version: version || undefined,
        message: `${engine} already installed${version ? ` (${version})` : ""}`,
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
 * Install container engine on multiple hosts in parallel
 */
export async function installEngineOnHosts(
  sshManagers: SSHManager[],
  engine: "podman" | "docker",
): Promise<EngineInstallResult[]> {
  console.log(`Installing ${engine} on ${sshManagers.length} host(s)...`);

  // Process hosts in parallel
  const promises = sshManagers.map(async (ssh) => {
    const installer = new EngineInstaller(ssh);
    const host = ssh.getHost();

    try {
      if (!ssh.isConnected()) {
        await ssh.connect();
        console.log(`Connected to ${host}`);
      }

      const result = await installer.installEngine(engine);
      return result;
    } catch (error) {
      const errorMessage = error instanceof Error
        ? error.message
        : String(error);
      const troubleshootingTips = getSSHTroubleshootingTips(errorMessage);

      console.log(`Connection failed for ${host}:`);
      console.log(`   ${errorMessage}`);
      console.log(`\nTroubleshooting suggestions:`);
      troubleshootingTips.forEach((tip) => console.log(`   ${tip}`));
      console.log("");

      return {
        success: false,
        host,
        output: "",
        error: errorMessage,
        message: `Connection failed: ${errorMessage}`,
      };
    }
  });

  const results = await Promise.all(promises);

  const successful = results.filter((r) => r.success);
  const failed = results.filter((r) => !r.success);

  if (successful.length > 0) {
    console.log(`\nSuccessfully installed on:`);
    for (const result of successful) {
      console.log(
        `  - ${result.host}${result.version ? ` (${result.version})` : ""}`,
      );
    }
  }

  if (failed.length > 0) {
    console.log(`\nFailed installations:`);
    for (const result of failed) {
      console.log(`  - ${result.host}: ${result.error}`);
    }
  }

  return results;
}

/**
 * Check if container engine is available on multiple hosts
 */
export async function checkEngineOnHosts(
  sshManagers: SSHManager[],
  engine: "podman" | "docker",
): Promise<{ host: string; available: boolean; version?: string }[]> {
  const promises = sshManagers.map(async (ssh) => {
    const installer = new EngineInstaller(ssh);
    const host = ssh.getHost();

    try {
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
    } catch (_error) {
      return {
        host,
        available: false,
        version: undefined,
      };
    }
  });

  return await Promise.all(promises);
}
