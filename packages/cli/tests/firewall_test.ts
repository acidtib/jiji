/**
 * Tests for UFW firewall utility functions
 * Covers UFW status detection, forward rule checks, rule management,
 * and network status display logic
 */

import { assertEquals, assertRejects } from "@std/assert";
import type { SSHManager } from "../src/utils/ssh.ts";
import {
  addContainerForwardRules,
  ensureUfwForwardRules,
  extractHostPorts,
  hasContainerForwardRules,
  isUfwActive,
} from "../src/lib/network/firewall.ts";
import { MockSSHManager } from "./mocks.ts";

// --- isUfwActive ---

Deno.test("isUfwActive - returns true when UFW status is active", async () => {
  const mockSsh = new MockSSHManager("test-host");
  mockSsh.addMockResponse("ufw status", {
    success: true,
    stdout:
      "Status: active\n\nTo                         Action      From\n--                         ------      ----\n22/tcp                     ALLOW       Anywhere",
    stderr: "",
    code: 0,
  });

  const result = await isUfwActive(mockSsh as unknown as SSHManager);
  assertEquals(result, true);
});

Deno.test(
  "isUfwActive - returns false when UFW status is inactive",
  async () => {
    const mockSsh = new MockSSHManager("test-host");
    mockSsh.addMockResponse("ufw status", {
      success: true,
      stdout: "Status: inactive",
      stderr: "",
      code: 0,
    });

    const result = await isUfwActive(mockSsh as unknown as SSHManager);
    assertEquals(result, false);
  },
);

Deno.test(
  "isUfwActive - returns false when ufw command fails",
  async () => {
    const mockSsh = new MockSSHManager("test-host");
    mockSsh.addMockResponse("ufw status", {
      success: false,
      stdout: "",
      stderr: "ufw: command not found",
      code: 127,
    });

    const result = await isUfwActive(mockSsh as unknown as SSHManager);
    assertEquals(result, false);
  },
);

// --- hasContainerForwardRules ---

Deno.test(
  "hasContainerForwardRules - returns true when rules exist",
  async () => {
    const mockSsh = new MockSSHManager("test-host");
    mockSsh.addMockResponse(
      "grep -q 'jiji container forward rules for 10.210.128.0/24' /etc/ufw/before.rules",
      {
        success: true,
        stdout: "",
        stderr: "",
        code: 0,
      },
    );

    const result = await hasContainerForwardRules(
      mockSsh as unknown as SSHManager,
      "10.210.128.0/24",
    );
    assertEquals(result, true);
  },
);

Deno.test(
  "hasContainerForwardRules - returns false when rules are missing",
  async () => {
    const mockSsh = new MockSSHManager("test-host");
    mockSsh.addMockResponse(
      "grep -q 'jiji container forward rules for 10.210.128.0/24' /etc/ufw/before.rules",
      {
        success: false,
        stdout: "",
        stderr: "",
        code: 1,
      },
    );

    const result = await hasContainerForwardRules(
      mockSsh as unknown as SSHManager,
      "10.210.128.0/24",
    );
    assertEquals(result, false);
  },
);

// --- addContainerForwardRules ---

Deno.test(
  "addContainerForwardRules - executes correct commands (grep, sed, ufw reload)",
  async () => {
    const mockSsh = new MockSSHManager("test-host");

    // Mock finding the COMMIT line number
    mockSsh.addMockResponse(
      "grep -n '^COMMIT$' /etc/ufw/before.rules | tail -1 | cut -d: -f1",
      {
        success: true,
        stdout: "42",
        stderr: "",
        code: 0,
      },
    );

    // Mock sed insertion
    mockSsh.addMockResponse("sed -i", {
      success: true,
      stdout: "",
      stderr: "",
      code: 0,
    });

    // Mock ufw reload
    mockSsh.addMockResponse("ufw reload", {
      success: true,
      stdout: "Firewall reloaded",
      stderr: "",
      code: 0,
    });

    await addContainerForwardRules(
      mockSsh as unknown as SSHManager,
      "10.210.128.0/24",
    );

    const commands = mockSsh.getAllCommands();

    // Verify grep for COMMIT line
    assertEquals(
      commands.some((cmd) =>
        cmd.includes("grep -n '^COMMIT$' /etc/ufw/before.rules")
      ),
      true,
    );

    // Verify sed command inserts the correct rules with the right line number
    assertEquals(
      commands.some((cmd) =>
        cmd.includes("sed -i") && cmd.includes("42i\\") &&
        cmd.includes("10.210.128.0/24")
      ),
      true,
    );

    // Verify ufw reload
    assertEquals(
      commands.some((cmd) => cmd.includes("ufw reload")),
      true,
    );
  },
);

Deno.test(
  "addContainerForwardRules - throws when COMMIT line not found",
  async () => {
    const mockSsh = new MockSSHManager("test-host");

    mockSsh.addMockResponse(
      "grep -n '^COMMIT$' /etc/ufw/before.rules | tail -1 | cut -d: -f1",
      {
        success: false,
        stdout: "",
        stderr: "No match",
        code: 1,
      },
    );

    await assertRejects(
      async () => {
        await addContainerForwardRules(
          mockSsh as unknown as SSHManager,
          "10.210.128.0/24",
        );
      },
      Error,
      "Failed to find COMMIT line",
    );
  },
);

Deno.test(
  "addContainerForwardRules - throws when sed fails",
  async () => {
    const mockSsh = new MockSSHManager("test-host");

    mockSsh.addMockResponse(
      "grep -n '^COMMIT$' /etc/ufw/before.rules | tail -1 | cut -d: -f1",
      {
        success: true,
        stdout: "42",
        stderr: "",
        code: 0,
      },
    );

    mockSsh.addMockResponse("sed -i", {
      success: false,
      stdout: "",
      stderr: "Permission denied",
      code: 1,
    });

    await assertRejects(
      async () => {
        await addContainerForwardRules(
          mockSsh as unknown as SSHManager,
          "10.210.128.0/24",
        );
      },
      Error,
      "Failed to add UFW forward rules",
    );
  },
);

Deno.test(
  "addContainerForwardRules - throws when ufw reload fails",
  async () => {
    const mockSsh = new MockSSHManager("test-host");

    mockSsh.addMockResponse(
      "grep -n '^COMMIT$' /etc/ufw/before.rules | tail -1 | cut -d: -f1",
      {
        success: true,
        stdout: "42",
        stderr: "",
        code: 0,
      },
    );

    mockSsh.addMockResponse("sed -i", {
      success: true,
      stdout: "",
      stderr: "",
      code: 0,
    });

    mockSsh.addMockResponse("ufw reload", {
      success: false,
      stdout: "",
      stderr: "ERROR: problem running ufw",
      code: 1,
    });

    await assertRejects(
      async () => {
        await addContainerForwardRules(
          mockSsh as unknown as SSHManager,
          "10.210.128.0/24",
        );
      },
      Error,
      "Failed to reload UFW",
    );
  },
);

// --- ensureUfwForwardRules ---

Deno.test(
  "ensureUfwForwardRules - skips when UFW is not active",
  async () => {
    const mockSsh = new MockSSHManager("test-host");

    // UFW not active
    mockSsh.addMockResponse("ufw status", {
      success: true,
      stdout: "Status: inactive",
      stderr: "",
      code: 0,
    });

    await ensureUfwForwardRules(
      mockSsh as unknown as SSHManager,
      "10.210.128.0/24",
    );

    const commands = mockSsh.getAllCommands();

    // Should only check UFW status, nothing else
    assertEquals(commands.length, 1);
    assertEquals(commands[0].includes("ufw status"), true);
  },
);

Deno.test(
  "ensureUfwForwardRules - skips when rules already exist",
  async () => {
    const mockSsh = new MockSSHManager("test-host");

    // UFW is active
    mockSsh.addMockResponse("ufw status", {
      success: true,
      stdout: "Status: active\n",
      stderr: "",
      code: 0,
    });

    // Rules already exist
    mockSsh.addMockResponse(
      "grep -q 'jiji container forward rules for 10.210.128.0/24' /etc/ufw/before.rules",
      {
        success: true,
        stdout: "",
        stderr: "",
        code: 0,
      },
    );

    await ensureUfwForwardRules(
      mockSsh as unknown as SSHManager,
      "10.210.128.0/24",
    );

    const commands = mockSsh.getAllCommands();

    // Should check UFW status and grep for existing rules, but not add anything
    assertEquals(commands.length, 2);
    assertEquals(
      commands.some((cmd) => cmd.includes("sed -i")),
      false,
    );
    assertEquals(
      commands.some((cmd) => cmd.includes("ufw reload")),
      false,
    );
  },
);

Deno.test(
  "ensureUfwForwardRules - adds rules when UFW is active and rules are missing",
  async () => {
    const mockSsh = new MockSSHManager("test-host");

    // UFW is active
    mockSsh.addMockResponse("ufw status", {
      success: true,
      stdout: "Status: active\n",
      stderr: "",
      code: 0,
    });

    // Rules do NOT exist
    mockSsh.addMockResponse(
      "grep -q 'jiji container forward rules for 10.210.128.0/24' /etc/ufw/before.rules",
      {
        success: false,
        stdout: "",
        stderr: "",
        code: 1,
      },
    );

    // Mock addContainerForwardRules sub-commands
    mockSsh.addMockResponse(
      "grep -n '^COMMIT$' /etc/ufw/before.rules | tail -1 | cut -d: -f1",
      {
        success: true,
        stdout: "55",
        stderr: "",
        code: 0,
      },
    );

    mockSsh.addMockResponse("sed -i", {
      success: true,
      stdout: "",
      stderr: "",
      code: 0,
    });

    mockSsh.addMockResponse("ufw reload", {
      success: true,
      stdout: "Firewall reloaded",
      stderr: "",
      code: 0,
    });

    await ensureUfwForwardRules(
      mockSsh as unknown as SSHManager,
      "10.210.128.0/24",
    );

    const commands = mockSsh.getAllCommands();

    // Should have executed: ufw status, grep for rules, grep COMMIT, sed, ufw reload
    assertEquals(commands.length, 5);
    assertEquals(
      commands.some((cmd) => cmd.includes("sed -i")),
      true,
    );
    assertEquals(
      commands.some((cmd) => cmd.includes("ufw reload")),
      true,
    );
  },
);

// --- Network status display logic ---
// These tests verify the UFW status check pattern used in `jiji network status`

Deno.test(
  "network status - UFW active with forward rules configured",
  async () => {
    const mockSsh = new MockSSHManager("192.168.1.10");

    mockSsh.addMockResponse("ufw status", {
      success: true,
      stdout: "Status: active\n",
      stderr: "",
      code: 0,
    });

    mockSsh.addMockResponse(
      "grep -q 'jiji container forward rules for 10.210.128.0/24' /etc/ufw/before.rules",
      {
        success: true,
        stdout: "",
        stderr: "",
        code: 0,
      },
    );

    const ufwActive = await isUfwActive(mockSsh as unknown as SSHManager);
    assertEquals(ufwActive, true);

    // Simulate subnet calculation from server.subnet "10.210.0.0/24"
    const serverSubnet = "10.210.0.0/24";
    const serverIndex = parseInt(serverSubnet.split(".")[2]);
    const containerThirdOctet = 128 + serverIndex;
    const baseNetwork = serverSubnet.split(".").slice(0, 2).join(".");
    const containerSubnet = `${baseNetwork}.${containerThirdOctet}.0/24`;

    assertEquals(containerSubnet, "10.210.128.0/24");

    const hasRules = await hasContainerForwardRules(
      mockSsh as unknown as SSHManager,
      containerSubnet,
    );
    assertEquals(hasRules, true);

    // Verify the display string
    const status = `UFW: ACTIVE (forward rules ${
      hasRules ? "configured" : "missing!"
    })`;
    assertEquals(status, "UFW: ACTIVE (forward rules configured)");
  },
);

Deno.test(
  "network status - UFW active with forward rules missing",
  async () => {
    const mockSsh = new MockSSHManager("192.168.1.10");

    mockSsh.addMockResponse("ufw status", {
      success: true,
      stdout: "Status: active\n",
      stderr: "",
      code: 0,
    });

    mockSsh.addMockResponse(
      "grep -q 'jiji container forward rules for 10.210.129.0/24' /etc/ufw/before.rules",
      {
        success: false,
        stdout: "",
        stderr: "",
        code: 1,
      },
    );

    const ufwActive = await isUfwActive(mockSsh as unknown as SSHManager);
    assertEquals(ufwActive, true);

    // Server at index 1 → container subnet 10.210.129.0/24
    const serverSubnet = "10.210.1.0/24";
    const serverIndex = parseInt(serverSubnet.split(".")[2]);
    const containerThirdOctet = 128 + serverIndex;
    const baseNetwork = serverSubnet.split(".").slice(0, 2).join(".");
    const containerSubnet = `${baseNetwork}.${containerThirdOctet}.0/24`;

    assertEquals(containerSubnet, "10.210.129.0/24");

    const hasRules = await hasContainerForwardRules(
      mockSsh as unknown as SSHManager,
      containerSubnet,
    );
    assertEquals(hasRules, false);

    const status = `UFW: ACTIVE (forward rules ${
      hasRules ? "configured" : "missing!"
    })`;
    assertEquals(status, "UFW: ACTIVE (forward rules missing!)");
  },
);

Deno.test(
  "network status - UFW inactive shows INACTIVE",
  async () => {
    const mockSsh = new MockSSHManager("192.168.1.10");

    mockSsh.addMockResponse("ufw status", {
      success: true,
      stdout: "Status: inactive",
      stderr: "",
      code: 0,
    });

    const ufwActive = await isUfwActive(mockSsh as unknown as SSHManager);
    assertEquals(ufwActive, false);

    // When UFW is inactive, no forward rule check should be needed
    const commands = mockSsh.getAllCommands();
    assertEquals(commands.length, 1);
    assertEquals(commands[0].includes("ufw status"), true);
  },
);

// --- extractHostPorts ---

Deno.test("extractHostPorts - extracts host:container port mappings", () => {
  const ports = extractHostPorts(["80:8080", "443:8443"]);
  assertEquals(ports, [
    { port: 80, protocol: "tcp" },
    { port: 443, protocol: "tcp" },
  ]);
});

Deno.test("extractHostPorts - skips container-only ports", () => {
  const ports = extractHostPorts(["8080", "3000"]);
  assertEquals(ports, []);
});

Deno.test("extractHostPorts - skips localhost-only bindings", () => {
  const ports = extractHostPorts([
    "127.0.0.1:3000:3000",
    "127.0.0.1:5432:5432",
  ]);
  assertEquals(ports, []);
});

Deno.test("extractHostPorts - respects protocol suffix", () => {
  const ports = extractHostPorts(["53:53/udp", "80:80/tcp", "443:443"]);
  assertEquals(ports, [
    { port: 53, protocol: "udp" },
    { port: 80, protocol: "tcp" },
    { port: 443, protocol: "tcp" },
  ]);
});

Deno.test("extractHostPorts - includes non-localhost IP bindings", () => {
  const ports = extractHostPorts(["0.0.0.0:8080:80", "192.168.1.1:3000:3000"]);
  assertEquals(ports, [
    { port: 8080, protocol: "tcp" },
    { port: 3000, protocol: "tcp" },
  ]);
});

Deno.test("extractHostPorts - handles mixed port formats", () => {
  const ports = extractHostPorts([
    "80:8080", // host-mapped
    "3000", // container-only, skip
    "127.0.0.1:5432:5432", // localhost-only, skip
    "443:8443/tcp", // host-mapped with protocol
    "1900/udp", // container-only with protocol, skip
  ]);
  assertEquals(ports, [
    { port: 80, protocol: "tcp" },
    { port: 443, protocol: "tcp" },
  ]);
});
