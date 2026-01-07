import { assertEquals, assertThrows } from "@std/assert";
import { ServersConfiguration } from "../servers.ts";
import { ConfigurationError } from "../base.ts";

// ============================================================================
// Basic Parsing Tests
// ============================================================================

Deno.test("ServersConfiguration - parse simple server configuration", () => {
  const config = new ServersConfiguration({
    "server1": {
      host: "192.168.1.100",
    },
  });

  const server = config.getServer("server1");
  assertEquals(server?.host, "192.168.1.100");
  assertEquals(server?.arch, undefined); // Not specified, uses default elsewhere
});

Deno.test("ServersConfiguration - parse server with all optional properties", () => {
  const config = new ServersConfiguration({
    "server1": {
      host: "192.168.1.100",
      arch: "arm64",
      user: "ubuntu",
      port: 2222,
      key_path: "~/.ssh/custom_key",
    },
  });

  const server = config.getServer("server1");
  assertEquals(server?.host, "192.168.1.100");
  assertEquals(server?.arch, "arm64");
  assertEquals(server?.user, "ubuntu");
  assertEquals(server?.port, 2222);
  assertEquals(server?.key_path, "~/.ssh/custom_key");
});

Deno.test("ServersConfiguration - parse multiple servers", () => {
  const config = new ServersConfiguration({
    "server1": { host: "192.168.1.100" },
    "server2": { host: "192.168.1.101", arch: "amd64" },
    "server3": { host: "192.168.1.102", arch: "arm64" },
  });

  assertEquals(config.getAllServerNames(), ["server1", "server2", "server3"]);
  assertEquals(config.servers.size, 3);
});

Deno.test("ServersConfiguration - handle keys array", () => {
  const config = new ServersConfiguration({
    "server1": {
      host: "192.168.1.100",
      keys: ["~/.ssh/id_rsa", "~/.ssh/id_ed25519"],
    },
  });

  const server = config.getServer("server1");
  assertEquals(server?.keys, ["~/.ssh/id_rsa", "~/.ssh/id_ed25519"]);
});

Deno.test("ServersConfiguration - handle key_data array", () => {
  const config = new ServersConfiguration({
    "server1": {
      host: "192.168.1.100",
      key_data: ["SSH_KEY_1", "SSH_KEY_2"],
    },
  });

  const server = config.getServer("server1");
  assertEquals(server?.key_data, ["SSH_KEY_1", "SSH_KEY_2"]);
});

// ============================================================================
// Validation Tests
// ============================================================================

Deno.test("ServersConfiguration - require host property", () => {
  assertThrows(
    () => {
      const config = new ServersConfiguration({
        "server1": {},
      });
      // Access servers to trigger validation
      config.servers;
    },
    ConfigurationError,
    "'host' is required for server 'server1'",
  );
});

Deno.test("ServersConfiguration - validate DNS-safe server names", () => {
  // Valid names
  const validNames = [
    "server1",
    "my-server",
    "web-01",
    "a",
    "ABC-123",
  ];

  for (const name of validNames) {
    const config = new ServersConfiguration({
      [name]: { host: "192.168.1.100" },
    });
    assertEquals(config.getServer(name)?.host, "192.168.1.100");
  }
});

Deno.test("ServersConfiguration - reject non-DNS-safe server names", () => {
  const invalidNames = [
    "-server", // Starts with hyphen
    "server-", // Ends with hyphen
    "server_1", // Contains underscore
    "my server", // Contains space
    "server@host", // Contains @
  ];

  for (const name of invalidNames) {
    assertThrows(
      () => {
        const config = new ServersConfiguration({
          [name]: { host: "192.168.1.100" },
        });
        // Access servers to trigger validation
        config.servers;
      },
      ConfigurationError,
      `Server name '${name}' is not DNS-safe`,
    );
  }
});

Deno.test("ServersConfiguration - validate architecture values", () => {
  // Valid architectures
  const config1 = new ServersConfiguration({
    "server1": { host: "192.168.1.100", arch: "amd64" },
  });
  assertEquals(config1.getServer("server1")?.arch, "amd64");

  const config2 = new ServersConfiguration({
    "server2": { host: "192.168.1.101", arch: "arm64" },
  });
  assertEquals(config2.getServer("server2")?.arch, "arm64");

  // Invalid architecture
  assertThrows(
    () => {
      const config = new ServersConfiguration({
        "server1": { host: "192.168.1.100", arch: "x86" },
      });
      // Access servers to trigger validation
      config.servers;
    },
    ConfigurationError,
    "'arch' for server 'server1' must be 'amd64' or 'arm64'",
  );
});

Deno.test("ServersConfiguration - validate port numbers", () => {
  // Valid port
  const config = new ServersConfiguration({
    "server1": { host: "192.168.1.100", port: 2222 },
  });
  assertEquals(config.getServer("server1")?.port, 2222);

  // Port too low
  assertThrows(
    () => {
      const config = new ServersConfiguration({
        "server1": { host: "192.168.1.100", port: 0 },
      });
      // Access servers to trigger validation
      config.servers;
    },
    ConfigurationError,
    "must be a valid port number (1-65535)",
  );

  // Port too high
  assertThrows(
    () => {
      const config = new ServersConfiguration({
        "server1": { host: "192.168.1.100", port: 70000 },
      });
      // Access servers to trigger validation
      config.servers;
    },
    ConfigurationError,
    "must be a valid port number (1-65535)",
  );
});

Deno.test("ServersConfiguration - detect duplicate hosts", () => {
  const config = new ServersConfiguration({
    "server1": { host: "192.168.1.100" },
    "server2": { host: "192.168.1.100" }, // Duplicate host!
  });

  const result = config.validate();
  assertEquals(result.valid, false);
  assertEquals(result.errors.length, 1);
  assertEquals(result.errors[0].code, "DUPLICATE_HOST");
  assertEquals(
    result.errors[0].message,
    "Duplicate host '192.168.1.100' found in servers: server1, server2. Each server must have a unique hostname.",
  );
});

// ============================================================================
// Query Methods Tests
// ============================================================================

Deno.test("ServersConfiguration - get all server names sorted", () => {
  const config = new ServersConfiguration({
    "zebra": { host: "192.168.1.103" },
    "apple": { host: "192.168.1.101" },
    "banana": { host: "192.168.1.102" },
  });

  assertEquals(config.getAllServerNames(), ["apple", "banana", "zebra"]);
});

Deno.test("ServersConfiguration - get all unique hosts sorted", () => {
  const config = new ServersConfiguration({
    "server1": { host: "192.168.1.100" },
    "server2": { host: "192.168.1.101" },
    "server3": { host: "192.168.1.102" },
  });

  assertEquals(config.getAllHosts(), [
    "192.168.1.100",
    "192.168.1.101",
    "192.168.1.102",
  ]);
});

Deno.test("ServersConfiguration - return undefined for non-existent server", () => {
  const config = new ServersConfiguration({
    "server1": { host: "192.168.1.100" },
  });

  assertEquals(config.getServer("nonexistent"), undefined);
});

Deno.test("ServersConfiguration - handle empty configuration", () => {
  const config = new ServersConfiguration({});

  assertEquals(config.getAllServerNames(), []);
  assertEquals(config.getAllHosts(), []);
  assertEquals(config.servers.size, 0);
});

// ============================================================================
// Type Validation Tests
// ============================================================================

Deno.test("ServersConfiguration - reject non-string host", () => {
  assertThrows(
    () => {
      const config = new ServersConfiguration({
        "server1": { host: 12345 as unknown as string },
      });
      // Access servers to trigger validation
      config.servers;
    },
    ConfigurationError,
  );
});

Deno.test("ServersConfiguration - reject non-string arch", () => {
  assertThrows(
    () => {
      const config = new ServersConfiguration({
        "server1": { host: "192.168.1.100", arch: 123 as unknown as string },
      });
      // Access servers to trigger validation
      config.servers;
    },
    ConfigurationError,
    "'arch' for server 'server1' must be a string",
  );
});

Deno.test("ServersConfiguration - reject non-array keys", () => {
  assertThrows(
    () => {
      const config = new ServersConfiguration({
        "server1": {
          host: "192.168.1.100",
          keys: "single-key" as unknown as string[],
        },
      });
      // Access servers to trigger validation
      config.servers;
    },
    ConfigurationError,
    "'keys' for server 'server1' must be an array",
  );
});

Deno.test("ServersConfiguration - reject non-array key_data", () => {
  assertThrows(
    () => {
      const config = new ServersConfiguration({
        "server1": {
          host: "192.168.1.100",
          key_data: "single-key" as unknown as string[],
        },
      });
      // Access servers to trigger validation
      config.servers;
    },
    ConfigurationError,
    "'key_data' for server 'server1' must be an array",
  );
});

// ============================================================================
// Edge Cases Tests
// ============================================================================

Deno.test("ServersConfiguration - handle server names at maximum length (63 chars)", () => {
  const longName = "a" + "-".repeat(61) + "z"; // 63 chars total
  const config = new ServersConfiguration({
    [longName]: { host: "192.168.1.100" },
  });

  assertEquals(config.getServer(longName)?.host, "192.168.1.100");
});

Deno.test("ServersConfiguration - reject server names over 63 chars", () => {
  const tooLongName = "a" + "-".repeat(62) + "z"; // 64 chars
  assertThrows(
    () => {
      const config = new ServersConfiguration({
        [tooLongName]: { host: "192.168.1.100" },
      });
      // Access servers to trigger validation
      config.servers;
    },
    ConfigurationError,
    "is not DNS-safe",
  );
});

Deno.test("ServersConfiguration - handle single character server name", () => {
  const config = new ServersConfiguration({
    "a": { host: "192.168.1.100" },
  });

  assertEquals(config.getServer("a")?.host, "192.168.1.100");
});

Deno.test("ServersConfiguration - preserve insertion order in servers Map", () => {
  const config = new ServersConfiguration({
    "server3": { host: "192.168.1.103" },
    "server1": { host: "192.168.1.101" },
    "server2": { host: "192.168.1.102" },
  });

  const names = Array.from(config.servers.keys());
  assertEquals(names, ["server3", "server1", "server2"]);
});
