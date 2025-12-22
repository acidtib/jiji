import type { LogLevel } from "./common.ts";

/**
 * SSH-related type definitions
 */

/**
 * Default SSH algorithms for compatibility with various SSH servers
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

/**
 * SSH connection configuration
 */
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

/**
 * Result of executing a command via SSH
 */
export interface CommandResult {
  stdout: string;
  stderr: string;
  success: boolean;
  code: number | null;
}
