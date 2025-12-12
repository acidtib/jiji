import { NodeSSH } from "node-ssh";

export interface SSHConnectionConfig {
  host: string;
  username: string;
  port?: number;
  useAgent?: boolean;
}

export interface CommandResult {
  stdout: string;
  stderr: string;
  success: boolean;
  code: number | null;
}

/**
 * SSH connection manager for remote operations
 */
export class SSHManager {
  private ssh: NodeSSH;
  private config: SSHConnectionConfig;
  private connected = false;

  constructor(config: SSHConnectionConfig) {
    this.ssh = new NodeSSH();
    this.config = config;
  }

  /**
   * Establish SSH connection to the remote host
   */
  async connect(): Promise<void> {
    const sshAuthSock = Deno.env.get("SSH_AUTH_SOCK");
    if (!sshAuthSock) {
      throw new Error("SSH_AUTH_SOCK environment variable not set");
    }

    try {
      await this.ssh.connect({
        host: this.config.host,
        username: this.config.username,
        port: this.config.port || 22,
        agent: sshAuthSock,
        algorithms: {
          serverHostKey: [
            "ssh-rsa",
            "ecdsa-sha2-nistp256",
            "ecdsa-sha2-nistp384",
            "ecdsa-sha2-nistp521",
            "ssh-ed25519",
          ],
          kex: [
            "ecdh-sha2-nistp256",
            "ecdh-sha2-nistp384",
            "ecdh-sha2-nistp521",
            "diffie-hellman-group14-sha256",
            "diffie-hellman-group16-sha512",
            "diffie-hellman-group1-sha1",
          ],
          cipher: [
            "aes128-ctr",
            "aes256-ctr",
            "aes128-cbc",
          ],
          hmac: ["hmac-sha2-256", "hmac-sha2-512", "hmac-sha1"],
          compress: ["none"],
        },
        readyTimeout: 60000,
        keepaliveInterval: 30000,
        debug: (msg: string) => {
          if (msg.includes("error") || msg.includes("fail")) {
            console.log(`SSH Debug: ${msg}`);
          }
        },
      });
      this.connected = true;
    } catch (error) {
      throw new Error(
        `Failed to connect to ${this.config.host}: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    }
  }

  /**
   * Check if connected
   */
  isConnected(): boolean {
    return this.connected && this.ssh.isConnected();
  }

  /**
   * Execute a command on the remote host
   */
  async executeCommand(command: string): Promise<CommandResult> {
    if (!this.isConnected()) {
      throw new Error("SSH connection not established. Call connect() first.");
    }

    try {
      const result = await this.ssh.execCommand(command);
      return {
        stdout: result.stdout,
        stderr: result.stderr,
        success: result.code === 0,
        code: result.code,
      };
    } catch (error) {
      return {
        stdout: "",
        stderr: error instanceof Error ? error.message : String(error),
        success: false,
        code: null,
      };
    }
  }

  /**
   * Execute multiple commands in sequence
   */
  async executeCommands(commands: string[]): Promise<CommandResult[]> {
    const results: CommandResult[] = [];

    for (const command of commands) {
      const result = await this.executeCommand(command);
      results.push(result);

      // Stop on first failure
      if (!result.success) {
        break;
      }
    }

    return results;
  }

  /**
   * Check if a command exists on the remote system
   */
  async commandExists(command: string): Promise<boolean> {
    const result = await this.executeCommand(`which ${command}`);
    return result.success;
  }

  /**
   * Upload a file to the remote host
   */
  async uploadFile(localPath: string, remotePath: string): Promise<void> {
    if (!this.isConnected()) {
      throw new Error("SSH connection not established. Call connect() first.");
    }

    try {
      await this.ssh.putFile(localPath, remotePath);
    } catch (error) {
      throw new Error(
        `Failed to upload file to ${this.config.host}: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    }
  }

  /**
   * Download a file from the remote host
   */
  async downloadFile(remotePath: string, localPath: string): Promise<void> {
    if (!this.isConnected()) {
      throw new Error("SSH connection not established. Call connect() first.");
    }

    try {
      await this.ssh.getFile(localPath, remotePath);
    } catch (error) {
      throw new Error(
        `Failed to download file from ${this.config.host}: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    }
  }

  /**
   * Get host information
   */
  getHost(): string {
    return this.config.host;
  }

  /**
   * Close the SSH connection
   */
  dispose(): void {
    if (this.ssh) {
      this.ssh.dispose();
      this.connected = false;
    }
  }
}

/**
 * Check if ssh-agent is available
 */
export function isSSHAgentAvailable(): boolean {
  const sshAuthSock = Deno.env.get("SSH_AUTH_SOCK");
  return !!sshAuthSock;
}

/**
 * Get SSH configuration from environment variables or use defaults
 */
export function getDefaultSSHConfig(): Omit<SSHConnectionConfig, "host"> {
  return {
    username: Deno.env.get("SSH_USERNAME") || "root",
    port: parseInt(Deno.env.get("SSH_PORT") || "22"),
    useAgent: true,
  };
}

/**
 * Create SSH managers for multiple hosts
 */
export function createSSHManagers(
  hosts: string[],
  sshConfig: Omit<SSHConnectionConfig, "host">,
): SSHManager[] {
  return hosts.map((host) => new SSHManager({ ...sshConfig, host }));
}

/**
 * Create SSH configuration from Jiji config
 */
export function createSSHConfigFromJiji(
  jijiSSHConfig?: { user: string; port?: number },
): Omit<SSHConnectionConfig, "host"> {
  const defaults = getDefaultSSHConfig();

  if (!jijiSSHConfig) {
    return defaults;
  }

  return {
    username: jijiSSHConfig.user,
    port: jijiSSHConfig.port || defaults.port,
    useAgent: true,
  };
}

/**
 * Validate SSH agent setup
 */
export async function validateSSHSetup(): Promise<
  { valid: boolean; message?: string }
> {
  if (!isSSHAgentAvailable()) {
    return {
      valid: false,
      message:
        "SSH_AUTH_SOCK environment variable not set - ssh-agent not available",
    };
  }

  // Try to list keys in ssh-agent
  try {
    const command = new Deno.Command("ssh-add", {
      args: ["-l"],
      stdout: "piped",
      stderr: "piped",
    });

    const { success, stdout } = await command.output();

    if (!success) {
      return {
        valid: false,
        message: "No SSH keys found in ssh-agent",
      };
    }

    const keyOutput = new TextDecoder().decode(stdout);
    const _keyCount = keyOutput.trim().split("\n").filter((line) =>
      line.length > 0
    ).length;

    return {
      valid: true,
    };
  } catch (error) {
    return {
      valid: false,
      message: `Failed to check SSH keys: ${
        error instanceof Error ? error.message : String(error)
      }`,
    };
  }
}

/**
 * Test connections to multiple hosts and return results
 */
export async function testConnections(
  sshManagers: SSHManager[],
): Promise<{ host: string; connected: boolean; error?: string }[]> {
  console.log("Testing connections to all hosts...");

  const connectionTests = await Promise.all(
    sshManagers.map(async (ssh) => {
      try {
        await ssh.connect();
        console.log(`Connected to ${ssh.getHost()}`);
        return { host: ssh.getHost(), connected: true };
      } catch (error) {
        const errorMessage = error instanceof Error
          ? error.message
          : String(error);
        console.log(
          `❌ Failed to connect to ${ssh.getHost()}: ${errorMessage}`,
        );
        return {
          host: ssh.getHost(),
          connected: false,
          error: errorMessage,
        };
      }
    }),
  );

  return connectionTests;
}

/**
 * Filter SSH managers to only include successfully connected hosts
 */
export function filterConnectedHosts(
  sshManagers: SSHManager[],
  connectionResults: { host: string; connected: boolean }[],
): {
  connectedManagers: SSHManager[];
  connectedHosts: string[];
  failedHosts: string[];
} {
  const connectedHosts = connectionResults
    .filter((test) => test.connected)
    .map((test) => test.host);

  const failedHosts = connectionResults
    .filter((test) => !test.connected)
    .map((test) => test.host);

  const connectedManagers = sshManagers.filter((ssh) =>
    connectedHosts.includes(ssh.getHost())
  );

  return { connectedManagers, connectedHosts, failedHosts };
}

/**
 * Get SSH troubleshooting suggestions for common connection errors
 */
export function getSSHTroubleshootingTips(error: string): string[] {
  const tips: string[] = [];

  if (
    error.includes("setAutoPadding") || error.includes("Aes128Gcm") ||
    error.includes("Unsupported algorithm") || error.includes("cipher")
  ) {
    tips.push("🔧 Cipher/algorithm compatibility issue detected");
    tips.push("   The server may be using unsupported encryption methods");
    tips.push("   Try connecting manually first: ssh -v user@host");
    tips.push("   Or add to your SSH client config (~/.ssh/config):");
    tips.push("   Host *");
    tips.push("     Ciphers aes128-ctr,aes256-ctr,aes128-cbc");
    tips.push(
      "     KexAlgorithms ecdh-sha2-nistp256,diffie-hellman-group14-sha256",
    );
    tips.push("     HostKeyAlgorithms ssh-rsa,ecdsa-sha2-nistp256,ssh-ed25519");
  }

  if (error.includes("ECONNREFUSED") || error.includes("connect")) {
    tips.push("Connection refused - check:");
    tips.push("   • Host IP address is correct");
    tips.push("   • SSH service is running on target host");
    tips.push("   • Port " + "22" + " is open and accessible");
    tips.push("   • No firewall blocking the connection");
  }

  if (error.includes("auth") || error.includes("permission")) {
    tips.push("🔑 Authentication failed - try:");
    tips.push("   • ssh-add -l (verify keys are loaded)");
    tips.push("   • ssh-add ~/.ssh/id_rsa (add your key)");
    tips.push("   • ssh -T user@host (test connection manually)");
  }

  if (error.includes("timeout") || error.includes("ETIMEDOUT")) {
    tips.push("⏱️  Connection timeout - check:");
    tips.push("   • Network connectivity to host");
    tips.push("   • SSH port accessibility");
    tips.push("   • Host is powered on and reachable");
  }

  if (tips.length === 0) {
    tips.push("❓ For general SSH issues:");
    tips.push("   • Test connection: ssh -v user@host");
    tips.push("   • Check SSH agent: ssh-add -l");
    tips.push("   • Verify host accessibility: ping host");
  }

  return tips;
}
