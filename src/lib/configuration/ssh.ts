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
