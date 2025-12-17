import { Logger } from "./logger.ts";

interface SSHConfigHost {
  pattern: string;
  config: Record<string, string>;
}

/**
 * Parser for SSH configuration files
 *
 * Supports parsing ~/.ssh/config and /etc/ssh/ssh_config files
 * to extract host-specific configuration that can be merged with
 * Jiji's SSH configuration.
 */
export class SSHConfigParser {
  private hosts: SSHConfigHost[] = [];
  private logger = new Logger({ prefix: "ssh-config" });

  /**
   * Parse an SSH config file
   */
  async parseFile(filePath: string): Promise<void> {
    try {
      const content = await Deno.readTextFile(filePath);
      this.parseContent(content);
    } catch (_error) {
      // File doesn't exist or not readable - that's OK
      this.logger.debug(`Could not read SSH config file: ${filePath}`);
    }
  }

  /**
   * Parse SSH config content
   */
  parseContent(content: string): void {
    const lines = content.split("\n");
    let currentHost: SSHConfigHost | null = null;

    for (const line of lines) {
      const trimmed = line.trim();

      // Skip comments and empty lines
      if (!trimmed || trimmed.startsWith("#")) continue;

      // Host directive
      if (trimmed.toLowerCase().startsWith("host ")) {
        if (currentHost) {
          this.hosts.push(currentHost);
        }
        const pattern = trimmed.substring(5).trim();
        currentHost = { pattern, config: {} };
        continue;
      }

      // Configuration option
      if (currentHost) {
        const [key, ...valueParts] = trimmed.split(/\s+/);
        const value = valueParts.join(" ");
        if (key && value) {
          currentHost.config[key.toLowerCase()] = value;
        }
      }
    }

    if (currentHost) {
      this.hosts.push(currentHost);
    }
  }

  /**
   * Get SSH configuration for a specific hostname
   *
   * Returns merged configuration from all matching host patterns.
   * More specific patterns override less specific ones.
   */
  getConfigForHost(hostname: string): Record<string, string> {
    const config: Record<string, string> = {};

    // Match hosts from first to last (SSH config precedence)
    // First match takes precedence for each config key
    for (const host of this.hosts) {
      if (this.matchesPattern(hostname, host.pattern)) {
        // Only set config values that haven't been set yet
        for (const [key, value] of Object.entries(host.config)) {
          if (!(key in config)) {
            config[key] = value;
          }
        }
      }
    }

    return config;
  }

  /**
   * Check if hostname matches SSH config pattern
   *
   * Supports wildcards:
   * - * matches zero or more characters
   * - ? matches exactly one character
   * - Multiple patterns separated by whitespace
   * - Negation patterns starting with !
   */
  private matchesPattern(hostname: string, pattern: string): boolean {
    // Handle multiple patterns separated by whitespace
    const patterns = pattern.split(/\s+/);

    let hasPositiveMatch = false;
    let hasNegativeMatch = false;

    for (const pat of patterns) {
      if (pat.startsWith("!")) {
        // Negation pattern
        if (this.matchesSinglePattern(hostname, pat.substring(1))) {
          hasNegativeMatch = true;
        }
      } else {
        // Positive pattern
        if (this.matchesSinglePattern(hostname, pat)) {
          hasPositiveMatch = true;
        }
      }
    }

    // Match if there's a positive match and no negative match
    return hasPositiveMatch && !hasNegativeMatch;
  }

  /**
   * Match hostname against a single pattern (without negation)
   */
  private matchesSinglePattern(hostname: string, pattern: string): boolean {
    // Convert SSH wildcards to regex
    // Escape regex special characters except * and ?
    const escaped = pattern.replace(/[.+^${}()|[\]\\]/g, "\\$&");

    // Convert SSH wildcards to regex
    const regexPattern = escaped
      .replace(/\*/g, ".*") // * matches zero or more characters
      .replace(/\?/g, "."); // ? matches exactly one character

    const regex = new RegExp("^" + regexPattern + "$", "i");
    return regex.test(hostname);
  }

  /**
   * Get all parsed host configurations (for debugging)
   */
  getHosts(): SSHConfigHost[] {
    return [...this.hosts];
  }

  /**
   * Clear all parsed configurations
   */
  clear(): void {
    this.hosts = [];
  }

  /**
   * Parse multiple SSH config files
   */
  async parseFiles(filePaths: string[]): Promise<void> {
    for (const filePath of filePaths) {
      await this.parseFile(filePath);
    }
  }

  /**
   * Get relevant SSH options that Jiji can use
   */
  getJijiRelevantConfig(hostname: string): {
    hostname?: string;
    port?: number;
    user?: string;
    identityFile?: string;
    proxyJump?: string;
    proxyCommand?: string;
    connectTimeout?: number;
    serverAliveInterval?: number;
    serverAliveCountMax?: number;
    compression?: boolean;
    forwardAgent?: boolean;
    strictHostKeyChecking?: boolean;
  } {
    const config = this.getConfigForHost(hostname);
    const result: Record<string, unknown> = {};

    // Map SSH config options to Jiji options
    if (config.hostname) result.hostname = config.hostname;
    if (config.port) result.port = parseInt(config.port, 10);
    if (config.user) result.user = config.user;
    if (config.identityfile) result.identityFile = config.identityfile;
    if (config.proxyjump) result.proxyJump = config.proxyjump;
    if (config.proxycommand) result.proxyCommand = config.proxycommand;
    if (config.connecttimeout) {
      result.connectTimeout = parseInt(config.connecttimeout, 10);
    }
    if (config.serveraliveinterval) {
      result.serverAliveInterval = parseInt(config.serveraliveinterval, 10);
    }
    if (config.serveralivecountmax) {
      result.serverAliveCountMax = parseInt(config.serveralivecountmax, 10);
    }
    if (config.compression) {
      result.compression = this.parseBoolean(config.compression);
    }
    if (config.forwardagent) {
      result.forwardAgent = this.parseBoolean(config.forwardagent);
    }
    if (config.stricthostkeychecking) {
      result.strictHostKeyChecking = this.parseBoolean(
        config.stricthostkeychecking,
      );
    }

    return result;
  }

  /**
   * Parse SSH config boolean values
   * Accepts: yes/no, true/false, on/off (case insensitive)
   */
  private parseBoolean(value: string): boolean {
    const normalized = value.toLowerCase();
    return normalized === "yes" || normalized === "true" || normalized === "on";
  }
}
