/**
 * Subnet allocator for WireGuard private network
 *
 * Allocates /24 subnets from a larger CIDR block (e.g., 10.210.0.0/16)
 * for each server in the cluster.
 */

/**
 * Parse an IPv4 address into octets
 */
function parseIp(ip: string): number[] {
  return ip.split(".").map((octet) => parseInt(octet, 10));
}

/**
 * Convert octets back to IP string
 */
function octetsToIp(octets: number[]): string {
  return octets.join(".");
}

/**
 * Increment an IP address by a given amount
 */
function incrementIp(ip: string, increment: number): string {
  const octets = parseIp(ip);
  let value = (octets[0] << 24) | (octets[1] << 16) | (octets[2] << 8) |
    octets[3];
  value += increment;
  return octetsToIp([
    (value >>> 24) & 0xff,
    (value >>> 16) & 0xff,
    (value >>> 8) & 0xff,
    value & 0xff,
  ]);
}

/**
 * Allocate subnets for servers
 */
export class SubnetAllocator {
  private baseIp: string;
  private prefixLength: number;

  constructor(clusterCidr: string) {
    const [ip, prefix] = clusterCidr.split("/");
    this.baseIp = ip;
    this.prefixLength = parseInt(prefix, 10);

    if (this.prefixLength > 24) {
      throw new Error(
        `Cluster CIDR prefix /${this.prefixLength} is too small. Use at least /24.`,
      );
    }
  }

  /**
   * Allocate a /24 subnet for a server at the given index
   *
   * @param serverIndex - Zero-based index of the server
   * @returns Subnet CIDR (e.g., "10.210.1.0/24")
   */
  allocateSubnet(serverIndex: number): string {
    const maxServers = this.getMaxServers();
    if (serverIndex >= maxServers) {
      throw new Error(
        `Server index ${serverIndex} exceeds maximum of ${maxServers} servers for cluster CIDR`,
      );
    }

    // Each /24 subnet contains 256 addresses
    // For server 0: use base + 0*256 = base
    // For server 1: use base + 1*256 = base + 256
    const subnetBase = incrementIp(this.baseIp, serverIndex * 256);
    return `${subnetBase}/24`;
  }

  /**
   * Get the WireGuard IP for a server (first usable IP in its subnet)
   *
   * @param serverIndex - Zero-based index of the server
   * @returns WireGuard IP (e.g., "10.210.1.1")
   */
  getServerWireGuardIp(serverIndex: number): string {
    const subnet = this.allocateSubnet(serverIndex);
    const [subnetBase] = subnet.split("/");
    // Server IP is .1 in the subnet
    return incrementIp(subnetBase, 1);
  }

  /**
   * Get the first container IP in a server's subnet
   * Containers start at .2 (server uses .1)
   *
   * @param serverIndex - Zero-based index of the server
   * @returns First container IP (e.g., "10.210.1.2")
   */
  getFirstContainerIp(serverIndex: number): string {
    const subnet = this.allocateSubnet(serverIndex);
    const [subnetBase] = subnet.split("/");
    // First container IP is .2 in the subnet
    return incrementIp(subnetBase, 2);
  }

  /**
   * Get the maximum number of servers supported by the cluster CIDR
   */
  getMaxServers(): number {
    // Each server gets a /24 subnet (256 addresses)
    // For /16: 2^(24-16) = 256 servers
    // For /20: 2^(24-20) = 16 servers
    return Math.pow(2, 24 - this.prefixLength);
  }

  /**
   * Get all allocated subnets for a list of server indices
   *
   * @param serverCount - Number of servers to allocate subnets for
   * @returns Array of subnet allocations
   */
  allocateAll(serverCount: number): Array<{
    serverIndex: number;
    subnet: string;
    wireguardIp: string;
    firstContainerIp: string;
  }> {
    const allocations = [];
    for (let i = 0; i < serverCount; i++) {
      allocations.push({
        serverIndex: i,
        subnet: this.allocateSubnet(i),
        wireguardIp: this.getServerWireGuardIp(i),
        firstContainerIp: this.getFirstContainerIp(i),
      });
    }
    return allocations;
  }

  /**
   * Check if an IP is within a given subnet
   *
   * @param ip - IP address to check
   * @param subnet - Subnet CIDR (e.g., "10.210.1.0/24")
   * @returns True if IP is in subnet
   */
  static isIpInSubnet(ip: string, subnet: string): boolean {
    const [subnetBase, prefix] = subnet.split("/");
    const prefixNum = parseInt(prefix, 10);

    const ipOctets = parseIp(ip);
    const subnetOctets = parseIp(subnetBase);

    const ipValue = (ipOctets[0] << 24) | (ipOctets[1] << 16) |
      (ipOctets[2] << 8) | ipOctets[3];
    const subnetValue = (subnetOctets[0] << 24) | (subnetOctets[1] << 16) |
      (subnetOctets[2] << 8) | subnetOctets[3];

    const mask = ~((1 << (32 - prefixNum)) - 1);

    return (ipValue & mask) === (subnetValue & mask);
  }

  /**
   * Get the next available IP in a subnet
   *
   * @param subnet - Subnet CIDR
   * @param usedIps - Array of IPs already allocated
   * @returns Next available IP or null if subnet is full
   */
  static getNextAvailableIp(subnet: string, usedIps: string[]): string | null {
    const [subnetBase, prefix] = subnet.split("/");
    const prefixNum = parseInt(prefix, 10);

    // For /24, we have 254 usable IPs (.1 to .254)
    // .0 is network address, .255 is broadcast
    const maxHosts = Math.pow(2, 32 - prefixNum) - 2;

    const used = new Set(usedIps);

    for (let i = 2; i <= maxHosts; i++) {
      const ip = incrementIp(subnetBase, i);
      if (!used.has(ip)) {
        return ip;
      }
    }

    return null; // Subnet is full
  }
}
