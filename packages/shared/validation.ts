/**
 * Shared input validation utilities.
 *
 * All data from external sources (Corrosion, user input, network) is untrusted
 * and must be validated before use in SQL queries, shell commands, or network calls.
 */

/**
 * Escape single quotes for SQL string interpolation.
 * Doubles single quotes to prevent SQL injection.
 */
export function escapeSql(str: string): string {
  return str.replace(/'/g, "''");
}

/**
 * Validate a Docker/Podman container ID (hex string, 12-64 chars).
 */
export function isValidContainerId(id: string): boolean {
  return /^[a-f0-9]{12,64}$/.test(id);
}

/**
 * Validate a server ID (alphanumeric, hyphens, underscores, dots; 1-128 chars).
 */
export function isValidServerId(id: string): boolean {
  return /^[a-zA-Z0-9._-]{1,128}$/.test(id);
}

/**
 * Validate a WireGuard public key (base64, 44 chars ending with =).
 */
export function isValidWireGuardKey(key: string): boolean {
  return /^[A-Za-z0-9+/]{43}=$/.test(key);
}

/**
 * Validate an IPv4 address.
 */
export function isValidIPv4(ip: string): boolean {
  const parts = ip.split(".");
  if (parts.length !== 4) return false;
  return parts.every((part) => {
    const num = parseInt(part, 10);
    return num >= 0 && num <= 255 && String(num) === part;
  });
}

/**
 * Validate an IPv6 address (simplified — accepts standard and compressed forms).
 */
export function isValidIPv6(ip: string): boolean {
  return /^([0-9a-fA-F]{0,4}:){2,7}[0-9a-fA-F]{0,4}$/.test(ip);
}

/**
 * Validate a CIDR notation (IPv4/prefix).
 */
export function isValidCIDR(cidr: string): boolean {
  const parts = cidr.split("/");
  if (parts.length !== 2) return false;
  const prefix = parseInt(parts[1], 10);
  if (isNaN(prefix) || prefix < 0) return false;
  if (isValidIPv4(parts[0])) return prefix <= 32;
  if (isValidIPv6(parts[0])) return prefix <= 128;
  return false;
}

/**
 * Validate a network endpoint (ip:port or [ipv6]:port).
 */
export function isValidEndpoint(endpoint: string): boolean {
  // IPv6: [addr]:port
  const ipv6Match = endpoint.match(/^\[([^\]]+)\]:(\d+)$/);
  if (ipv6Match) {
    const port = parseInt(ipv6Match[2], 10);
    return isValidIPv6(ipv6Match[1]) && port >= 1 && port <= 65535;
  }

  // IPv4: addr:port
  const parts = endpoint.split(":");
  if (parts.length !== 2) return false;
  const port = parseInt(parts[1], 10);
  return isValidIPv4(parts[0]) && port >= 1 && port <= 65535;
}
