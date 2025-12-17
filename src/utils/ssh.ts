import { NodeSSH } from "node-ssh";
import { Client, type ClientChannel } from "ssh2";
import { SSHProxy } from "./ssh_proxy.ts";
import { SSHConfigParser } from "./ssh_config_parser.ts";
import { Logger, type LogLevel } from "./logger.ts";

/**
 * Default SSH algorithms for compatibility with various SSH servers
 * These can be customized if needed for specific server configurations
 */
export const SSH_ALGORITHMS = {
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
  hmac: [
    "hmac-sha2-256",
    "hmac-sha2-512",
    "hmac-sha1",
  ],
  compress: ["none"],
} as const;

export interface SSHConnectionConfig {
  host: string;
  username: string;
  port?: number;
  useAgent?: boolean;
  proxy?: string;
  proxyCommand?: string;
  keys?: string[];
  keyData?: string[];
  keysOnly?: boolean;
  dnsRetries?: number;
  sshConfigFiles?: string[] | false;
  connectTimeout?: number;
  keyPath?: string;
  logLevel?: LogLevel;
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
  private ssh2Client?: Client;
  private config: SSHConnectionConfig;
  private connected = false;
  private logger: Logger;

  constructor(config: SSHConnectionConfig) {
    this.ssh = new NodeSSH();
    this.config = config;
    this.logger = Logger.forSSH(config.logLevel || "error", {
      prefix: config.host,
    });
  }

  /**
   * Establish SSH connection to the remote host using NodeSSH
   * This is the primary connection method for command execution
   */
  async connect(): Promise<void> {
    try {
      this.logger.debug(
        `Connecting to ${this.config.host}:${this.config.port || 22}`,
      );
      const config = await this.getSSHConnectionConfig();
      await this.ssh.connect({
        ...config,
        debug: (msg: string) => {
          this.logger.debug(`SSH: ${msg}`);
        },
      });
      this.connected = true;
      this.logger.info(`Connected to ${this.config.host}`);
    } catch (error) {
      const errorMsg = `Failed to connect to ${this.config.host}: ${
        error instanceof Error ? error.message : String(error)
      }`;
      this.logger.error(errorMsg);
      throw new Error(errorMsg);
    }
  }

  /**
   * Connect with DNS retry logic
   * Retries connection on DNS-related errors
   */
  async connectWithRetry(): Promise<void> {
    const maxRetries = this.config.dnsRetries ?? 3;
    let lastError: Error | undefined;

    for (let attempt = 1; attempt <= maxRetries; attempt++) {
      try {
        await this.connect();
        return; // Success
      } catch (error) {
        lastError = error instanceof Error ? error : new Error(String(error));

        // Check if it's a DNS error
        if (!this.isDNSError(lastError)) {
          throw lastError; // Non-DNS error, don't retry
        }

        if (attempt < maxRetries) {
          const delay = this.calculateRetryDelay(attempt);
          this.logger.warn(
            `DNS retry ${attempt}/${maxRetries} for ${this.config.host} in ${delay}ms`,
          );
          await this.sleep(delay);
        }
      }
    }

    throw lastError || new Error("Failed to connect after retries");
  }

  /**
   * Check if an error is DNS-related
   */
  private isDNSError(error: Error): boolean {
    const dnsErrorPatterns = [
      /getaddrinfo/i,
      /ENOTFOUND/i,
      /ENOENT/i,
      /temporary failure in name resolution/i,
      /name or service not known/i,
      /EAI_AGAIN/i,
    ];

    return dnsErrorPatterns.some((pattern) => pattern.test(error.message));
  }

  /**
   * Calculate retry delay with exponential backoff and jitter
   */
  private calculateRetryDelay(attempt: number): number {
    const baseDelay = 100; // 100ms base
    const maxDelay = 2000; // 2s max
    const jitter = Math.random() * 100; // 0-100ms jitter

    const delay = Math.min(baseDelay * Math.pow(2, attempt - 1), maxDelay);
    return delay + jitter;
  }

  /**
   * Sleep for specified milliseconds
   */
  private sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
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
      const errorMsg = "SSH connection not established. Call connect() first.";
      this.logger.error(errorMsg);
      throw new Error(errorMsg);
    }

    this.logger.debug(`Executing command: ${command}`);
    const startTime = Date.now();

    try {
      const result = await this.ssh.execCommand(command);
      const duration = Date.now() - startTime;

      if (result.code === 0) {
        this.logger.debug(`Command completed successfully in ${duration}ms`);
        if (result.stdout) {
          this.logger.trace(`Command stdout: ${result.stdout.trim()}`);
        }
      } else {
        this.logger.warn(
          `Command failed with exit code ${result.code} in ${duration}ms`,
        );
        if (result.stderr) {
          this.logger.warn(`Command stderr: ${result.stderr.trim()}`);
        }
      }

      return {
        stdout: result.stdout,
        stderr: result.stderr,
        success: result.code === 0,
        code: result.code,
      };
    } catch (error) {
      const duration = Date.now() - startTime;
      const errorMsg = error instanceof Error ? error.message : String(error);
      this.logger.error(
        `Command execution failed after ${duration}ms: ${errorMsg}`,
      );

      return {
        stdout: "",
        stderr: errorMsg,
        success: false,
        code: null,
      };
    }
  }

  /**
   * Execute multiple commands in sequence
   */
  async executeCommands(commands: string[]): Promise<CommandResult[]> {
    this.logger.info(`Executing ${commands.length} commands in sequence`);
    const results: CommandResult[] = [];
    let successCount = 0;

    for (let i = 0; i < commands.length; i++) {
      const command = commands[i];
      this.logger.debug(`Command ${i + 1}/${commands.length}: ${command}`);

      const result = await this.executeCommand(command);
      results.push(result);

      if (result.success) {
        successCount++;
      } else {
        this.logger.error(
          `Command sequence stopped at step ${i + 1} due to failure`,
        );
        break;
      }
    }

    this.logger.info(
      `Command sequence completed: ${successCount}/${commands.length} successful`,
    );
    return results;
  }

  /**
   * Create and connect ssh2 client for interactive sessions (on-demand)
   */
  private ensureSsh2Client(): Promise<void> {
    if (this.ssh2Client) {
      return Promise.resolve();
    }

    return new Promise((resolve, reject) => {
      this.getSSHConnectionConfig().then((config) => {
        this.ssh2Client = new Client();

        this.ssh2Client.on("ready", () => {
          resolve();
        });

        this.ssh2Client.on("error", (err: Error) => {
          reject(new Error(`SSH connection failed: ${err.message}`));
        });

        this.ssh2Client.connect(config);
      }).catch((error) => {
        reject(error);
      });
    });
  }

  /**
   * Get the shared SSH connection configuration
   */
  private async getSSHConnectionConfig() {
    const sshAuthSock = Deno.env.get("SSH_AUTH_SOCK");

    let baseConfig: Record<string, unknown> = {
      host: this.config.host,
      username: this.config.username,
      port: this.config.port || 22,
      algorithms: SSH_ALGORITHMS,
      readyTimeout: 60000,
      keepaliveInterval: 30000,
    };

    // Load and merge SSH config file settings
    if (this.config.sshConfigFiles) {
      baseConfig = await this.mergeSSHConfigFiles(baseConfig);
    }

    // Handle private keys
    await this.configurePrivateKeys(baseConfig, sshAuthSock);

    // Add proxy support
    if (this.config.proxy) {
      const proxyConfig = await this.buildProxyJumpConfig(
        this.config.proxy,
        sshAuthSock || "",
      );
      return { ...baseConfig, ...proxyConfig };
    }

    if (this.config.proxyCommand) {
      const proxyConfig = this.buildProxyCommandConfig(
        this.config.proxyCommand,
      );
      return { ...baseConfig, ...proxyConfig };
    }

    // Handle SSH config proxy settings if no Jiji proxy specified
    if (
      baseConfig._sshConfigProxyJump &&
      typeof baseConfig._sshConfigProxyJump === "string"
    ) {
      const proxyConfig = await this.buildProxyJumpConfig(
        baseConfig._sshConfigProxyJump,
        sshAuthSock || "",
      );
      // Clean up temporary property
      delete baseConfig._sshConfigProxyJump;
      return { ...baseConfig, ...proxyConfig };
    }

    if (
      baseConfig._sshConfigProxyCommand &&
      typeof baseConfig._sshConfigProxyCommand === "string"
    ) {
      const proxyConfig = this.buildProxyCommandConfig(
        baseConfig._sshConfigProxyCommand,
      );
      // Clean up temporary property
      delete baseConfig._sshConfigProxyCommand;
      return { ...baseConfig, ...proxyConfig };
    }

    return baseConfig;
  }

  /**
   * Load SSH config files and merge with Jiji configuration
   * Jiji config takes precedence over SSH config files
   */
  private async mergeSSHConfigFiles(
    jijiConfig: Record<string, unknown>,
  ): Promise<Record<string, unknown>> {
    if (this.config.sshConfigFiles === false || !this.config.sshConfigFiles) {
      return jijiConfig;
    }

    const parser = new SSHConfigParser();

    // Parse all specified config files
    for (const file of this.config.sshConfigFiles) {
      await parser.parseFile(file);
    }

    // Get SSH config for this host
    const fileConfig = parser.getJijiRelevantConfig(this.config.host);

    // Merge configurations (Jiji config takes precedence)
    return {
      ...jijiConfig,

      // Only apply file config if not already set by Jiji
      hostname: jijiConfig.host, // Always use Jiji's host
      port: jijiConfig.port || fileConfig.port || 22,
      username: jijiConfig.username || fileConfig.user,

      // Connection settings from SSH config
      ...(fileConfig.connectTimeout && !this.config.connectTimeout
        ? {
          readyTimeout: fileConfig.connectTimeout * 1000, // Convert to milliseconds
        }
        : {}),

      // Proxy settings from SSH config (only if not set in Jiji)
      ...(fileConfig.proxyJump && !this.config.proxy &&
          !this.config.proxyCommand
        ? {
          _sshConfigProxyJump: fileConfig.proxyJump,
        }
        : {}),

      ...(fileConfig.proxyCommand && !this.config.proxy &&
          !this.config.proxyCommand
        ? {
          _sshConfigProxyCommand: fileConfig.proxyCommand,
        }
        : {}),

      // Private key from SSH config (only if no keys specified in Jiji)
      ...(fileConfig.identityFile && !this.config.keys && !this.config.keyPath
        ? {
          _sshConfigIdentityFile: fileConfig.identityFile,
        }
        : {}),
    };
  }

  /**
   * Configure private keys for SSH connection
   */
  private async configurePrivateKeys(
    config: Record<string, unknown>,
    sshAuthSock: string | undefined,
  ): Promise<void> {
    const hasExplicitKeys = (this.config.keys && this.config.keys.length > 0) ||
      (this.config.keyData && this.config.keyData.length > 0) ||
      config._sshConfigIdentityFile;

    if (hasExplicitKeys) {
      const privateKeys: string[] = [];

      // Load private key files
      if (this.config.keys && this.config.keys.length > 0) {
        for (const keyPath of this.config.keys) {
          try {
            const keyContent = await Deno.readTextFile(keyPath);
            privateKeys.push(keyContent);
          } catch (error) {
            throw new Error(
              `Failed to read SSH key file '${keyPath}': ${
                error instanceof Error ? error.message : String(error)
              }`,
            );
          }
        }
      }

      // Add inline key data
      if (this.config.keyData && this.config.keyData.length > 0) {
        privateKeys.push(...this.config.keyData);
      }

      // Add SSH config identity file if no Jiji keys specified
      if (
        config._sshConfigIdentityFile &&
        typeof config._sshConfigIdentityFile === "string" &&
        (!this.config.keys || this.config.keys.length === 0) &&
        (!this.config.keyData || this.config.keyData.length === 0)
      ) {
        try {
          const keyContent = await Deno.readTextFile(
            config._sshConfigIdentityFile,
          );
          privateKeys.push(keyContent);
        } catch (error) {
          console.warn(
            `Failed to read SSH config identity file '${config._sshConfigIdentityFile}': ${
              error instanceof Error ? error.message : String(error)
            }`,
          );
        }
      }

      // Set private keys in config
      if (privateKeys.length > 0) {
        config.privateKey = privateKeys;
      }

      // Clean up SSH config temporary properties
      delete config._sshConfigIdentityFile;

      // Handle keys_only flag
      if (this.config.keysOnly) {
        // Don't use ssh-agent
        delete config.agent;
      } else if (sshAuthSock) {
        // Use both agent and explicit keys
        config.agent = sshAuthSock;
      }
    } else {
      // No explicit keys - use ssh-agent (existing behavior)
      if (!sshAuthSock) {
        throw new Error("SSH_AUTH_SOCK environment variable not set");
      }
      config.agent = sshAuthSock;
    }
  }

  /**
   * Build configuration for ProxyJump (SSH connection through bastion)
   */
  private async buildProxyJumpConfig(proxy: string, agentSocket: string) {
    const proxyInfo = SSHProxy.parseProxyString(proxy, this.config.username);
    const sshProxy = new SSHProxy();

    // Create the tunnel through the bastion host
    const sock = await sshProxy.createProxySocket(
      proxyInfo.host,
      proxyInfo.port,
      proxyInfo.user,
      this.config.host,
      this.config.port || 22,
      agentSocket,
    );

    return { sock };
  }

  /**
   * Build configuration for ProxyCommand (custom proxy command)
   */
  private buildProxyCommandConfig(proxyCommand: string) {
    // Replace %h and %p with actual host/port
    const command = proxyCommand
      .replace(/%h/g, this.config.host)
      .replace(/%p/g, String(this.config.port || 22));

    return {
      sock: {
        type: "exec",
        command,
      },
    };
  }

  /**
   * Start an interactive shell session
   */
  async startInteractiveSession(command: string): Promise<void> {
    // Ensure ssh2 client is connected for interactive sessions
    await this.ensureSsh2Client();

    return new Promise((resolve, reject) => {
      console.log(`\nConnected to ${this.config.host}`);
      console.log(`Starting interactive session: ${command}`);
      console.log(
        `To disconnect: Press Ctrl+C, Ctrl+D, Ctrl+\\, or type 'exit'\n`,
      );

      this.ssh2Client!.shell(
        { pty: true },
        (err: Error | undefined, stream: ClientChannel) => {
          if (err) {
            reject(err);
            return;
          }

          // Handle terminal resize
          const resizeHandler = () => {
            if (Deno.stdout.isTerminal()) {
              const size = Deno.consoleSize();
              stream.setWindow(size.rows, size.columns);
            }
          };

          // Set initial size
          resizeHandler();

          // Handle data from remote host
          stream.on("data", (data: Uint8Array) => {
            Deno.stdout.writeSync(data);
          });

          stream.stderr.on("data", (data: Uint8Array) => {
            Deno.stderr.writeSync(data);
          });

          // Handle stream close
          stream.on("close", () => {
            cleanup();
            if (this.ssh2Client) {
              this.ssh2Client.end();
              this.ssh2Client = undefined;
            }
            resolve();
          });

          stream.on("error", (err: Error) => {
            cleanup();
            if (this.ssh2Client) {
              this.ssh2Client.end();
              this.ssh2Client = undefined;
            }
            reject(err);
          });

          // Send the initial command
          stream.write(`${command}\r`);

          // Handle user input
          Deno.stdin.setRaw(true);
          let inputActive = true;

          // Set up a connection timeout (5 minutes)
          const connectionTimeout = setTimeout(() => {
            console.log(
              "\n⏰ Interactive session timed out after 5 minutes of inactivity",
            );
            inputActive = false;
            cleanup();
            stream.end();
          }, 5 * 60 * 1000);

          // Clear timeout on any activity
          stream.on("data", () => {
            clearTimeout(connectionTimeout);
          });

          const cleanup = () => {
            if (!inputActive) return; // Already cleaned up
            inputActive = false;
            try {
              Deno.stdin.setRaw(false);
            } catch {
              // Terminal may already be reset
            }
            try {
              clearTimeout(connectionTimeout);
            } catch {
              // Timeout may not be set
            }
            try {
              Deno.removeSignalListener("SIGINT", signalHandler);
              Deno.removeSignalListener("SIGTERM", signalHandler);
            } catch {
              // Signal listeners may already be removed
            }
          };

          const handleInput = async () => {
            const buffer = new Uint8Array(1024);
            try {
              while (inputActive) {
                const n = await Deno.stdin.read(buffer);
                if (n === null || !inputActive) break;

                const input = buffer.subarray(0, n);

                // Check for Ctrl+C (0x03), Ctrl+D (0x04), or Ctrl+\ (0x1C)
                if (
                  input.length === 1 &&
                  (input[0] === 3 || input[0] === 4 || input[0] === 28)
                ) {
                  inputActive = false;
                  cleanup();
                  stream.end();
                  setTimeout(() => {
                    if (this.ssh2Client) {
                      this.ssh2Client.end();
                      this.ssh2Client = undefined;
                    }
                    resolve();
                  }, 100);
                  break;
                }

                // Check for exit command (when user types "exit" followed by Enter)
                const inputStr = new TextDecoder().decode(input);
                if (
                  inputStr.includes("exit\r") || inputStr.includes("exit\n")
                ) {
                  inputActive = false;
                  cleanup();
                  stream.end();
                  setTimeout(() => {
                    if (this.ssh2Client) {
                      this.ssh2Client.end();
                      this.ssh2Client = undefined;
                    }
                    resolve();
                  }, 100);
                  break;
                }

                if (inputActive) {
                  stream.write(input);
                }
              }
            } catch (_error) {
              // Input stream closed or interrupted
            }
          };

          // Handle process signals
          const signalHandler = () => {
            if (!inputActive) return; // Already handled
            inputActive = false;
            cleanup();
            stream.destroy(); // Force close the stream
            if (this.ssh2Client) {
              this.ssh2Client.end();
              this.ssh2Client = undefined;
            }
            resolve();
          };

          Deno.addSignalListener("SIGINT", signalHandler);
          Deno.addSignalListener("SIGTERM", signalHandler);

          // Start handling input
          handleInput().catch(() => {
            // Input handling error, ensure cleanup
            cleanup();
          });
        },
      );
    });
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
    if (this.ssh2Client) {
      this.ssh2Client.end();
      this.ssh2Client = undefined;
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
  jijiSSHConfig?: {
    user: string;
    port?: number;
    proxy?: string;
    proxy_command?: string;
    keys?: string[];
    keyData?: string[];
    keysOnly?: boolean;
    dnsRetries?: number;
    sshConfigFiles?: string[] | false;
    connectTimeout?: number;
    keyPath?: string;
  },
): Omit<SSHConnectionConfig, "host"> {
  const defaults = getDefaultSSHConfig();

  if (!jijiSSHConfig) {
    return defaults;
  }

  return {
    username: jijiSSHConfig.user,
    port: jijiSSHConfig.port || defaults.port,
    useAgent: true,
    ...(jijiSSHConfig.proxy && { proxy: jijiSSHConfig.proxy }),
    ...(jijiSSHConfig.proxy_command &&
      { proxyCommand: jijiSSHConfig.proxy_command }),
    ...(jijiSSHConfig.keys && { keys: jijiSSHConfig.keys }),
    ...(jijiSSHConfig.keyData && { keyData: jijiSSHConfig.keyData }),
    ...(jijiSSHConfig.keysOnly !== undefined &&
      { keysOnly: jijiSSHConfig.keysOnly }),
    ...(jijiSSHConfig.dnsRetries !== undefined &&
      { dnsRetries: jijiSSHConfig.dnsRetries }),
    ...(jijiSSHConfig.sshConfigFiles !== undefined &&
      { sshConfigFiles: jijiSSHConfig.sshConfigFiles }),
    ...(jijiSSHConfig.connectTimeout !== undefined &&
      { connectTimeout: jijiSSHConfig.connectTimeout }),
    ...(jijiSSHConfig.keyPath && { keyPath: jijiSSHConfig.keyPath }),
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
 * Uses DNS retry logic and connection pooling
 */
export async function testConnections(
  sshManagers: SSHManager[],
  maxConcurrent?: number,
): Promise<{ host: string; connected: boolean; error?: string }[]> {
  console.log("Testing connections to all hosts...");

  // Import the pool dynamically to avoid circular dependencies
  const { SSHConnectionPool } = await import("./ssh_pool.ts");
  const pool = new SSHConnectionPool(maxConcurrent || 30);

  return pool.executeConcurrent(
    sshManagers.map((ssh) => async () => {
      try {
        await ssh.connectWithRetry(); // Use retry logic
        console.log(`✓ Connected to ${ssh.getHost()}`);
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
    tips.push("Cipher/algorithm compatibility issue detected");
    tips.push("   The server may be using unsupported encryption methods");
    tips.push("   Try connecting manually first: ssh -v user@host");
    tips.push("   Or add to your SSH client config (~/.ssh/config):");
    tips.push("   Host *");
    tips.push(`     Ciphers ${SSH_ALGORITHMS.cipher.join(",")}`);
    tips.push(
      `     KexAlgorithms ${SSH_ALGORITHMS.kex.slice(0, 2).join(",")}`,
    );
    tips.push(
      `     HostKeyAlgorithms ${
        SSH_ALGORITHMS.serverHostKey.slice(0, 3).join(",")
      }`,
    );
  }

  if (error.includes("ECONNREFUSED") || error.includes("connect")) {
    tips.push("Connection refused - check:");
    tips.push("   • Host IP address is correct");
    tips.push("   • SSH service is running on target host");
    tips.push("   • Port " + "22" + " is open and accessible");
    tips.push("   • No firewall blocking the connection");
  }

  if (error.includes("auth") || error.includes("permission")) {
    tips.push("Authentication failed - try:");
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

/**
 * Setup SSH connections with validation and testing
 * This consolidates the common SSH setup pattern used across commands
 */
export async function setupSSHConnections(
  hosts: string[],
  sshConfig: {
    user: string;
    port?: number;
    proxy?: string;
    proxy_command?: string;
    keys?: string[];
    keyData?: string[];
    keysOnly?: boolean;
    dnsRetries?: number;
    logLevel?: LogLevel;
  },
  options: {
    skipValidation?: boolean;
    allowPartialConnection?: boolean;
  } = {},
): Promise<{
  managers: SSHManager[];
  connectedHosts: string[];
  failedHosts: string[];
}> {
  const log = new Logger({ prefix: "ssh" });

  // Validate SSH setup unless explicitly skipped
  if (!options.skipValidation) {
    log.debug("Validating SSH configuration");
    const sshValidation = await validateSSHSetup();
    if (!sshValidation.valid) {
      log.error(`SSH setup validation failed:`);
      log.error(`   ${sshValidation.message}`);
      throw new Error(`SSH validation failed: ${sshValidation.message}`);
    }
    log.success("SSH setup validation passed");
  }

  // Create SSH connection configuration
  const connectionConfig = {
    ...createSSHConfigFromJiji({
      user: sshConfig.user,
      port: sshConfig.port,
      proxy: sshConfig.proxy,
      proxy_command: sshConfig.proxy_command,
      keys: sshConfig.keys && sshConfig.keys.length > 0
        ? sshConfig.keys
        : undefined,
      keyData: sshConfig.keyData,
      keysOnly: sshConfig.keysOnly,
      dnsRetries: sshConfig.dnsRetries,
    }),
    logLevel: sshConfig.logLevel,
    useAgent: true,
  };

  // Create SSH managers and test connections
  log.debug(`Testing connections to ${hosts.length} host(s)...`);
  const sshManagers = createSSHManagers(hosts, connectionConfig);
  const connectionTests = await testConnections(sshManagers);

  const { connectedManagers, connectedHosts, failedHosts } =
    filterConnectedHosts(sshManagers, connectionTests);

  // Handle connection results
  if (connectedHosts.length === 0) {
    log.error("No hosts are reachable. Cannot proceed.");
    throw new Error("No SSH connections could be established");
  }

  if (failedHosts.length > 0) {
    const message = `Some hosts are unreachable: ${failedHosts.join(", ")}`;

    if (options.allowPartialConnection) {
      log.warn(message);
      log.warn(`Continuing with ${connectedHosts.length} connected host(s)`);
    } else {
      log.error(message);
      throw new Error(message);
    }
  }

  log.success(
    `Connected to ${connectedHosts.length} host(s): ${
      connectedHosts.join(", ")
    }`,
  );

  return {
    managers: connectedManagers,
    connectedHosts,
    failedHosts,
  };
}
