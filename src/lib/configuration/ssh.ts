import { BaseConfiguration, ConfigurationError } from "./base.ts";
import type { Validatable } from "./base.ts";

/**
 * SSH configuration for connecting to remote hosts
 */
export class SSHConfiguration extends BaseConfiguration implements Validatable {
  private _user?: string;
  private _port?: number;
  private _keyPath?: string;
  private _keyPassphrase?: string;
  private _connectTimeout?: number;
  private _commandTimeout?: number;
  private _options?: Record<string, string>;
  private _proxy?: string;
  private _proxyCommand?: string;
  private _keys?: string[];
  private _keyData?: string[];
  private _keysOnly?: boolean;

  /**
   * SSH user for connections
   */
  get user(): string {
    if (!this._user) {
      this._user = this.getRequired<string>("user", "ssh");
    }
    return this._user;
  }

  /**
   * SSH port (defaults to 22)
   */
  get port(): number {
    if (!this._port) {
      this._port = this.has("port")
        ? this.validatePort(this.get("port"), "port", "ssh")
        : 22;
    }
    return this._port;
  }

  /**
   * Path to SSH private key
   */
  get keyPath(): string | undefined {
    if (!this._keyPath && this.has("key_path")) {
      this._keyPath = this.validateString(
        this.get("key_path"),
        "key_path",
        "ssh",
      );
    }
    return this._keyPath;
  }

  /**
   * SSH key passphrase
   */
  get keyPassphrase(): string | undefined {
    if (!this._keyPassphrase && this.has("key_passphrase")) {
      this._keyPassphrase = this.validateString(
        this.get("key_passphrase"),
        "key_passphrase",
        "ssh",
      );
    }
    return this._keyPassphrase;
  }

  /**
   * Connection timeout in seconds (defaults to 30)
   */
  get connectTimeout(): number {
    if (!this._connectTimeout) {
      this._connectTimeout = this.has("connect_timeout")
        ? this.validateNumber(
          this.get("connect_timeout"),
          "connect_timeout",
          "ssh",
        )
        : 30;
    }
    return this._connectTimeout;
  }

  /**
   * Command timeout in seconds (defaults to 300)
   */
  get commandTimeout(): number {
    if (!this._commandTimeout) {
      this._commandTimeout = this.has("command_timeout")
        ? this.validateNumber(
          this.get("command_timeout"),
          "command_timeout",
          "ssh",
        )
        : 300;
    }
    return this._commandTimeout;
  }

  /**
   * Additional SSH options
   */
  get options(): Record<string, string> {
    if (!this._options) {
      this._options = this.has("options")
        ? this.validateObject(this.get("options"), "options", "ssh") as Record<
          string,
          string
        >
        : {};
    }
    return this._options;
  }

  /**
   * SSH proxy/jump host (format: [user@]hostname[:port])
   */
  get proxy(): string | undefined {
    if (!this._proxy && this.has("proxy")) {
      this._proxy = this.validateString(
        this.get("proxy"),
        "proxy",
        "ssh",
      );
      if (!this._proxy || this._proxy.trim().length === 0) {
        throw new ConfigurationError("'proxy' in ssh cannot be empty");
      }
    }
    return this._proxy;
  }

  /**
   * SSH proxy command (must contain %h and %p placeholders)
   */
  get proxyCommand(): string | undefined {
    if (!this._proxyCommand && this.has("proxy_command")) {
      this._proxyCommand = this.validateString(
        this.get("proxy_command"),
        "proxy_command",
        "ssh",
      );
      // Validate contains %h and %p
      if (
        this._proxyCommand &&
        (!this._proxyCommand.includes("%h") ||
          !this._proxyCommand.includes("%p"))
      ) {
        throw new ConfigurationError(
          "'proxy_command' in ssh must contain %h and %p placeholders",
        );
      }
    }
    return this._proxyCommand;
  }

  /**
   * Array of SSH private key file paths
   */
  get keys(): string[] | undefined {
    if (!this._keys && this.has("keys")) {
      const keysValue = this.get("keys");
      if (!Array.isArray(keysValue)) {
        throw new ConfigurationError("'keys' in ssh must be an array");
      }
      this._keys = keysValue.map((key) => {
        if (typeof key !== "string") {
          throw new ConfigurationError(
            "'keys' in ssh must be an array of strings",
          );
        }
        return this.expandPath(key);
      });
    }
    return this._keys;
  }

  /**
   * Array of environment variable names containing SSH key data
   */
  get keyData(): string[] | undefined {
    if (!this._keyData && this.has("key_data")) {
      const keyDataValue = this.get("key_data");
      if (!Array.isArray(keyDataValue)) {
        throw new ConfigurationError("'key_data' in ssh must be an array");
      }

      this._keyData = keyDataValue.map((envVarName) => {
        if (typeof envVarName !== "string") {
          throw new ConfigurationError(
            "'key_data' in ssh must be an array of environment variable names",
          );
        }
        const keyContent = Deno.env.get(envVarName);
        if (!keyContent) {
          throw new ConfigurationError(
            `Environment variable '${envVarName}' not found for key_data`,
          );
        }
        return keyContent;
      });
    }
    return this._keyData;
  }

  /**
   * If true, ignore ssh-agent and use only specified keys
   */
  get keysOnly(): boolean {
    if (this._keysOnly === undefined && this.has("keys_only")) {
      const value = this.get("keys_only");
      if (typeof value !== "boolean") {
        throw new ConfigurationError("'keys_only' in ssh must be a boolean");
      }
      this._keysOnly = value;
    }
    return this._keysOnly ?? false;
  }

  /**
   * Get all keys (returns keys array or empty array)
   */
  get allKeys(): string[] {
    return this.keys || [];
  }

  /**
   * Expand ~ in file paths to home directory
   */
  private expandPath(path: string): string {
    if (path.startsWith("~/")) {
      const home = Deno.env.get("HOME") || Deno.env.get("USERPROFILE");
      if (!home) {
        throw new ConfigurationError(
          "Cannot expand ~ without HOME environment variable",
        );
      }
      return path.replace("~", home);
    }
    return path;
  }

  /**
   * Validates the SSH configuration
   */
  validate(): void {
    // Validate required fields
    this.user; // This will throw if not present

    // Validate port if provided
    if (this.has("port")) {
      this.port; // This will validate the port
    }

    // Validate connect timeout if provided
    if (this.has("connect_timeout")) {
      const timeout = this.connectTimeout;
      if (timeout <= 0) {
        throw new ConfigurationError(
          "'connect_timeout' in ssh must be greater than 0",
        );
      }
    }

    // Validate command timeout if provided
    if (this.has("command_timeout")) {
      const timeout = this.commandTimeout;
      if (timeout <= 0) {
        throw new ConfigurationError(
          "'command_timeout' in ssh must be greater than 0",
        );
      }
    }

    // Validate key path if provided
    if (this.has("key_path")) {
      const keyPath = this.keyPath;
      if (!keyPath || keyPath.trim().length === 0) {
        throw new ConfigurationError(
          "'key_path' in ssh cannot be empty",
        );
      }
    }

    // Validate options if provided
    if (this.has("options")) {
      const options = this.options;
      for (const [key, value] of Object.entries(options)) {
        if (typeof value !== "string") {
          throw new ConfigurationError(
            `SSH option '${key}' must be a string`,
          );
        }
      }
    }

    // Validate proxy configuration
    if (this.proxy && this.proxyCommand) {
      throw new ConfigurationError(
        "Cannot specify both 'proxy' and 'proxy_command' in ssh configuration",
      );
    }

    // Validate proxy format if present
    if (this.proxy) {
      this.validateProxyFormat(this.proxy);
    }

    // Trigger proxy_command validation (already validates in getter)
    if (this.proxyCommand) {
      // Validation happens in the getter
    }
  }

  /**
   * Validates proxy format: [user@]hostname[:port]
   */
  private validateProxyFormat(proxy: string): void {
    const proxyRegex = /^(?:([^@]+)@)?([^:]+)(?::(\d+))?$/;
    if (!proxyRegex.test(proxy)) {
      throw new ConfigurationError(
        `Invalid proxy format: '${proxy}'. Expected format: [user@]hostname[:port]`,
      );
    }

    // Validate port if specified
    const match = proxy.match(proxyRegex);
    if (match && match[3]) {
      const port = parseInt(match[3]);
      if (port < 1 || port > 65535) {
        throw new ConfigurationError(
          `Invalid proxy port: ${port}. Port must be between 1 and 65535`,
        );
      }
    }
  }

  /**
   * Returns the SSH configuration as a plain object
   */
  toObject(): Record<string, unknown> {
    const result: Record<string, unknown> = {
      user: this.user,
      port: this.port,
    };

    if (this.keyPath) {
      result.key_path = this.keyPath;
    }

    if (this.keyPassphrase) {
      result.key_passphrase = this.keyPassphrase;
    }

    if (this.connectTimeout !== 30) {
      result.connect_timeout = this.connectTimeout;
    }

    if (this.commandTimeout !== 300) {
      result.command_timeout = this.commandTimeout;
    }

    if (Object.keys(this.options).length > 0) {
      result.options = this.options;
    }

    if (this.proxy) {
      result.proxy = this.proxy;
    }

    if (this.proxyCommand) {
      result.proxy_command = this.proxyCommand;
    }

    if (this.keys && this.keys.length > 0) {
      result.keys = this.keys;
    }

    if (this.keyData && this.keyData.length > 0) {
      // Don't serialize actual key data, just indicate it's set
      result.key_data = `[${this.keyData.length} key(s) from environment]`;
    }

    if (this.keysOnly) {
      result.keys_only = this.keysOnly;
    }

    return result;
  }

  /**
   * Builds SSH connection arguments for command execution
   */
  buildSSHArgs(hostname: string): string[] {
    const args = [
      "-o",
      "StrictHostKeyChecking=no",
      "-o",
      "UserKnownHostsFile=/dev/null",
      "-o",
      `ConnectTimeout=${this.connectTimeout}`,
      "-p",
      this.port.toString(),
    ];

    // Add key path if specified
    if (this.keyPath) {
      args.push("-i", this.keyPath);
    }

    // Add custom options
    for (const [key, value] of Object.entries(this.options)) {
      args.push("-o", `${key}=${value}`);
    }

    // Add user and hostname
    args.push(`${this.user}@${hostname}`);

    return args;
  }

  /**
   * Creates an SSH configuration with default values
   */
  static withDefaults(
    overrides: Record<string, unknown> = {},
  ): SSHConfiguration {
    return new SSHConfiguration({
      user: "root",
      port: 22,
      connect_timeout: 30,
      command_timeout: 300,
      ...overrides,
    });
  }
}
