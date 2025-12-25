/**
 * Tests for network utility functions
 * Covers IP discovery and bridge interface detection
 */

import { assertEquals, assertRejects } from "@std/assert";
import type { SSHManager } from "../src/utils/ssh.ts";
import {
  discoverPrivateIPs,
  isPrivateIP,
} from "../src/lib/network/ip_discovery.ts";
import { getDockerBridgeInterface } from "../src/lib/network/routes.ts";
import { MockSSHManager } from "./mocks.ts";

Deno.test("isPrivateIP - should return true for 10.0.0.0/8 range", () => {
  assertEquals(isPrivateIP("10.0.0.1"), true);
  assertEquals(isPrivateIP("10.255.255.254"), true);
  assertEquals(isPrivateIP("10.120.0.2"), true);
});

Deno.test("isPrivateIP - should return true for 172.16.0.0/12 range", () => {
  assertEquals(isPrivateIP("172.16.0.1"), true);
  assertEquals(isPrivateIP("172.31.255.254"), true);
  assertEquals(isPrivateIP("172.20.10.5"), true);
});

Deno.test("isPrivateIP - should return true for 192.168.0.0/16 range", () => {
  assertEquals(isPrivateIP("192.168.0.1"), true);
  assertEquals(isPrivateIP("192.168.255.254"), true);
  assertEquals(isPrivateIP("192.168.1.100"), true);
});

Deno.test("isPrivateIP - should return false for public IPs", () => {
  assertEquals(isPrivateIP("143.110.143.43"), false);
  assertEquals(isPrivateIP("157.230.205.235"), false);
  assertEquals(isPrivateIP("8.8.8.8"), false);
  assertEquals(isPrivateIP("1.1.1.1"), false);
});

Deno.test("isPrivateIP - should return false for loopback", () => {
  assertEquals(isPrivateIP("127.0.0.1"), false);
});

Deno.test(
  "isPrivateIP - should return false for 172.15.x.x (outside 172.16-31 range)",
  () => {
    assertEquals(isPrivateIP("172.15.0.1"), false);
  },
);

Deno.test(
  "isPrivateIP - should return false for 172.32.x.x (outside 172.16-31 range)",
  () => {
    assertEquals(isPrivateIP("172.32.0.1"), false);
  },
);

Deno.test("isPrivateIP - should return false for invalid IP formats", () => {
  assertEquals(isPrivateIP("not-an-ip"), false);
  assertEquals(isPrivateIP("256.256.256.256"), false);
  assertEquals(isPrivateIP(""), false);
});

Deno.test(
  "discoverPrivateIPs - should filter out public IPs configured on interfaces",
  async () => {
    const mockSSH = new MockSSHManager("test-server");

    // Mock the ip addr command to return both public and private IPs
    mockSSH.addMockResponse("ip -4 addr show", {
      success: true,
      stdout: "10.120.0.2\n143.110.143.43\n192.168.1.5\n127.0.0.1",
      stderr: "",
      code: 0,
    });

    // Mock interface lookups for each IP
    // 10.120.0.2 - private IP on eth0 (should be included)
    mockSSH.addMockResponse('ip addr show | grep -B 2 "10.120.0.2"', {
      success: true,
      stdout: "eth0",
      stderr: "",
      code: 0,
    });
    mockSSH.addMockResponse("ip link show eth0", {
      success: true,
      stdout: "up",
      stderr: "",
      code: 0,
    });

    // 143.110.143.43 - public IP (should be filtered out by isPrivateIP check)
    // Mock won't be called because isPrivateIP returns false

    // 192.168.1.5 - private IP on eth1 (should be included)
    mockSSH.addMockResponse('ip addr show | grep -B 2 "192.168.1.5"', {
      success: true,
      stdout: "eth1",
      stderr: "",
      code: 0,
    });
    mockSSH.addMockResponse("ip link show eth1", {
      success: true,
      stdout: "up",
      stderr: "",
      code: 0,
    });

    // 127.0.0.1 - loopback (filtered out before isPrivateIP check)

    const privateIPs = await discoverPrivateIPs(
      mockSSH as unknown as SSHManager,
    );

    // Should only include private IPs, not public ones
    assertEquals(privateIPs.includes("10.120.0.2"), true);
    assertEquals(privateIPs.includes("192.168.1.5"), true);
    assertEquals(privateIPs.includes("143.110.143.43"), false); // Public IP filtered
    assertEquals(privateIPs.includes("127.0.0.1"), false); // Loopback filtered
  },
);

Deno.test(
  "discoverPrivateIPs - should skip Docker and WireGuard interfaces",
  async () => {
    const mockSSH = new MockSSHManager("test-server");

    mockSSH.addMockResponse("ip -4 addr show", {
      success: true,
      stdout: "10.0.0.1\n10.0.0.2\n10.0.0.3",
      stderr: "",
      code: 0,
    });

    // Docker bridge IP
    mockSSH.addMockResponse('ip addr show | grep -B 2 "10.0.0.1"', {
      success: true,
      stdout: "docker0",
      stderr: "",
      code: 0,
    });

    // WireGuard interface IP
    mockSSH.addMockResponse('ip addr show | grep -B 2 "10.0.0.2"', {
      success: true,
      stdout: "jiji0",
      stderr: "",
      code: 0,
    });

    // Regular private IP
    mockSSH.addMockResponse('ip addr show | grep -B 2 "10.0.0.3"', {
      success: true,
      stdout: "eth0",
      stderr: "",
      code: 0,
    });
    mockSSH.addMockResponse("ip link show eth0", {
      success: true,
      stdout: "up",
      stderr: "",
      code: 0,
    });

    const privateIPs = await discoverPrivateIPs(
      mockSSH as unknown as SSHManager,
    );

    // Should skip docker and jiji interfaces
    assertEquals(privateIPs.includes("10.0.0.1"), false); // Docker bridge
    assertEquals(privateIPs.includes("10.0.0.2"), false); // WireGuard
    assertEquals(privateIPs.includes("10.0.0.3"), true); // Regular interface
  },
);

Deno.test(
  "getDockerBridgeInterface - should discover bridge interface via gateway IP for Docker",
  async () => {
    const mockSSH = new MockSSHManager("test-server");

    // Mock network inspect to return gateway IP
    mockSSH.addMockResponse("docker network inspect jiji", {
      success: true,
      stdout: "172.18.0.1",
      stderr: "",
      code: 0,
    });

    // Mock finding the interface with that gateway IP
    // The command extracts just the interface name, so return only that
    mockSSH.addMockResponse('ip addr show | grep -B 2 "inet 172.18.0.1"', {
      success: true,
      stdout: "br-abc123456789",
      stderr: "",
      code: 0,
    });

    const bridge = await getDockerBridgeInterface(
      mockSSH as unknown as SSHManager,
      "jiji",
      "docker",
    );

    assertEquals(bridge, "br-abc123456789");
  },
);

Deno.test(
  "getDockerBridgeInterface - should discover bridge interface via gateway IP for Podman with Netavark",
  async () => {
    const mockSSH = new MockSSHManager("test-server");

    // Mock network inspect to return gateway IP
    mockSSH.addMockResponse("podman network inspect jiji", {
      success: true,
      stdout: "10.89.0.1",
      stderr: "",
      code: 0,
    });

    // Mock finding the Netavark bridge interface (podman0, podman1, etc.)
    // The command extracts just the interface name, so return only that
    mockSSH.addMockResponse('ip addr show | grep -B 2 "inet 10.89.0.1"', {
      success: true,
      stdout: "podman1",
      stderr: "",
      code: 0,
    });

    const bridge = await getDockerBridgeInterface(
      mockSSH as unknown as SSHManager,
      "jiji",
      "podman",
    );

    assertEquals(bridge, "podman1");
  },
);

Deno.test(
  "getDockerBridgeInterface - should discover bridge interface via gateway IP for Podman with CNI",
  async () => {
    const mockSSH = new MockSSHManager("test-server");

    // Mock network inspect to return gateway IP
    mockSSH.addMockResponse("podman network inspect jiji", {
      success: true,
      stdout: "10.88.0.1",
      stderr: "",
      code: 0,
    });

    // Mock finding the CNI bridge interface
    // The command extracts just the interface name, so return only that
    mockSSH.addMockResponse('ip addr show | grep -B 2 "inet 10.88.0.1"', {
      success: true,
      stdout: "cni-podman20d923ad",
      stderr: "",
      code: 0,
    });

    const bridge = await getDockerBridgeInterface(
      mockSSH as unknown as SSHManager,
      "jiji",
      "podman",
    );

    assertEquals(bridge, "cni-podman20d923ad");
  },
);

Deno.test(
  "getDockerBridgeInterface - should fallback to docker0 if bridge cannot be found",
  async () => {
    const mockSSH = new MockSSHManager("test-server");

    // Mock network inspect to return gateway IP
    mockSSH.addMockResponse("docker network inspect jiji", {
      success: true,
      stdout: "172.18.0.1",
      stderr: "",
      code: 0,
    });

    // Mock interface lookup failing
    mockSSH.addMockResponse('grep -B 2 "inet 172.18.0.1"', {
      success: false,
      stdout: "",
      stderr: "",
      code: 1,
    });

    const bridge = await getDockerBridgeInterface(
      mockSSH as unknown as SSHManager,
      "jiji",
      "docker",
    );

    assertEquals(bridge, "docker0");
  },
);

Deno.test(
  "getDockerBridgeInterface - should throw error if network inspect fails",
  async () => {
    const mockSSH = new MockSSHManager("test-server");

    // Mock network inspect failure
    mockSSH.addMockResponse("docker network inspect nonexistent", {
      success: false,
      stdout: "",
      stderr: "Error: No such network: nonexistent",
      code: 1,
    });

    await assertRejects(
      async () => {
        await getDockerBridgeInterface(
          mockSSH as unknown as SSHManager,
          "nonexistent",
          "docker",
        );
      },
      Error,
      "Failed to inspect network",
    );
  },
);

Deno.test(
  "getDockerBridgeInterface - should throw error if gateway IP is empty",
  async () => {
    const mockSSH = new MockSSHManager("test-server");

    // Mock network inspect returning empty gateway
    mockSSH.addMockResponse("docker network inspect jiji", {
      success: true,
      stdout: "",
      stderr: "",
      code: 0,
    });

    await assertRejects(
      async () => {
        await getDockerBridgeInterface(
          mockSSH as unknown as SSHManager,
          "jiji",
          "docker",
        );
      },
      Error,
      "Could not determine gateway IP",
    );
  },
);
