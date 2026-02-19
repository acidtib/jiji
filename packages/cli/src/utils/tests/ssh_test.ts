import { assertEquals } from "@std/assert";
import {
  type CommandResult,
  createSSHConfigFromJiji,
  createSSHManagers,
  getDefaultSSHConfig,
  isSSHAgentAvailable,
  type SSHConnectionConfig,
  SSHManager,
} from "../ssh.ts";

// Mock environment variables for testing
let originalEnv: Record<string, string | undefined> = {};

function setTestEnv(env: Record<string, string | undefined>) {
  // Save original values first time
  if (Object.keys(originalEnv).length === 0) {
    originalEnv = {
      SSH_AUTH_SOCK: Deno.env.get("SSH_AUTH_SOCK"),
      SSH_USERNAME: Deno.env.get("SSH_USERNAME"),
      SSH_PORT: Deno.env.get("SSH_PORT"),
    };
  }

  for (const [key, value] of Object.entries(env)) {
    if (value === undefined) {
      Deno.env.delete(key);
    } else {
      Deno.env.set(key, value);
    }
  }
}

function restoreEnv() {
  setTestEnv(originalEnv);
}

Deno.test("isSSHAgentAvailable - returns true when SSH_AUTH_SOCK is set", () => {
  setTestEnv({ SSH_AUTH_SOCK: "/tmp/ssh-agent.sock" });
  assertEquals(isSSHAgentAvailable(), true);
  restoreEnv();
});

Deno.test("isSSHAgentAvailable - returns false when SSH_AUTH_SOCK is not set", () => {
  setTestEnv({ SSH_AUTH_SOCK: undefined });
  assertEquals(isSSHAgentAvailable(), false);
  restoreEnv();
});

Deno.test("getDefaultSSHConfig - uses environment variables", () => {
  setTestEnv({
    SSH_USERNAME: "deploy",
    SSH_PORT: "2222",
  });

  const config = getDefaultSSHConfig();

  assertEquals(config.username, "deploy");
  assertEquals(config.port, 2222);
  assertEquals(config.useAgent, true);

  restoreEnv();
});

Deno.test("getDefaultSSHConfig - uses defaults when env vars not set", () => {
  setTestEnv({
    SSH_USERNAME: undefined,
    SSH_PORT: undefined,
  });

  const config = getDefaultSSHConfig();

  assertEquals(config.username, "root");
  assertEquals(config.port, 22);
  assertEquals(config.useAgent, true);

  restoreEnv();
});

Deno.test("getDefaultSSHConfig - handles invalid port gracefully", () => {
  setTestEnv({
    SSH_PORT: "invalid",
  });

  const config = getDefaultSSHConfig();

  assertEquals(config.port, NaN); // parseInt returns NaN for invalid strings
  assertEquals(config.username, "root");
  assertEquals(config.useAgent, true);

  restoreEnv();
});

Deno.test("createSSHConfigFromJiji - with jiji config", () => {
  const jijiConfig = {
    user: "admin",
    port: 2222,
  };

  const config = createSSHConfigFromJiji(jijiConfig);

  assertEquals(config.username, "admin");
  assertEquals(config.port, 2222);
  assertEquals(config.useAgent, true);
});

Deno.test("createSSHConfigFromJiji - with partial jiji config", () => {
  setTestEnv({
    SSH_USERNAME: "deploy",
    SSH_PORT: "3333",
  });

  const jijiConfig = {
    user: "admin",
    // no port specified
  };

  const config = createSSHConfigFromJiji(jijiConfig);

  assertEquals(config.username, "admin");
  assertEquals(config.port, 3333); // falls back to default (from env)
  assertEquals(config.useAgent, true);

  restoreEnv();
});

Deno.test("createSSHConfigFromJiji - without jiji config", () => {
  setTestEnv({
    SSH_USERNAME: "deploy",
    SSH_PORT: "4444",
  });

  const config = createSSHConfigFromJiji();

  assertEquals(config.username, "deploy");
  assertEquals(config.port, 4444);
  assertEquals(config.useAgent, true);

  restoreEnv();
});

Deno.test("createSSHConfigFromJiji - with undefined jiji config", () => {
  setTestEnv({
    SSH_USERNAME: undefined,
    SSH_PORT: undefined,
  });

  const config = createSSHConfigFromJiji(undefined);

  assertEquals(config.username, "root");
  assertEquals(config.port, 22);
  assertEquals(config.useAgent, true);

  restoreEnv();
});

Deno.test("createSSHManagers - creates managers for multiple hosts", () => {
  const hosts = ["server1.com", "server2.com", "server3.com"];
  const sshConfig = {
    username: "admin",
    port: 2222,
    useAgent: true,
  };

  const managers = createSSHManagers(hosts, sshConfig);

  assertEquals(managers.length, 3);
  assertEquals(managers[0].getHost(), "server1.com");
  assertEquals(managers[1].getHost(), "server2.com");
  assertEquals(managers[2].getHost(), "server3.com");

  // Clean up
  managers.forEach((manager) => manager.dispose());
});

Deno.test("createSSHManagers - creates managers with correct config", () => {
  const hosts = ["test.com"];
  const sshConfig = {
    username: "testuser",
    port: 3333,
    useAgent: false,
  };

  const managers = createSSHManagers(hosts, sshConfig);

  assertEquals(managers.length, 1);
  assertEquals(managers[0].getHost(), "test.com");

  // Clean up
  managers[0].dispose();
});

Deno.test("createSSHManagers - handles empty host list", () => {
  const hosts: string[] = [];
  const sshConfig = {
    username: "admin",
    port: 22,
    useAgent: true,
  };

  const managers = createSSHManagers(hosts, sshConfig);

  assertEquals(managers.length, 0);
});

Deno.test("SSHManager - constructor initializes with correct config", () => {
  const config: SSHConnectionConfig = {
    host: "example.com",
    username: "testuser",
    port: 2222,
    useAgent: true,
  };

  const manager = new SSHManager(config);

  assertEquals(manager.getHost(), "example.com");
  assertEquals(manager.isConnected(), false);

  manager.dispose();
});

Deno.test("SSHManager - getHost returns correct host", () => {
  const config: SSHConnectionConfig = {
    host: "test-server.com",
    username: "admin",
  };

  const manager = new SSHManager(config);

  assertEquals(manager.getHost(), "test-server.com");

  manager.dispose();
});

Deno.test("SSHManager - isConnected returns false initially", () => {
  const config: SSHConnectionConfig = {
    host: "example.com",
    username: "user",
  };

  const manager = new SSHManager(config);

  assertEquals(manager.isConnected(), false);

  manager.dispose();
});

Deno.test("SSHManager - dispose can be called safely multiple times", () => {
  const config: SSHConnectionConfig = {
    host: "example.com",
    username: "user",
  };

  const manager = new SSHManager(config);

  // Should not throw
  manager.dispose();
  manager.dispose();
  manager.dispose();
});

// Integration-style tests that don't require actual SSH connections
Deno.test("SSHManager - configuration validation", () => {
  // Test various valid configurations
  const validConfigs: SSHConnectionConfig[] = [
    {
      host: "localhost",
      username: "user",
    },
    {
      host: "example.com",
      username: "admin",
      port: 22,
    },
    {
      host: "192.168.1.100",
      username: "root",
      port: 2222,
      useAgent: false,
    },
  ];

  for (const config of validConfigs) {
    const manager = new SSHManager(config);
    assertEquals(manager.getHost(), config.host);
    manager.dispose();
  }
});

Deno.test("CommandResult interface properties", () => {
  // Test that we can create CommandResult objects with expected structure
  const successResult: CommandResult = {
    stdout: "command output",
    stderr: "",
    success: true,
    code: 0,
  };

  assertEquals(successResult.success, true);
  assertEquals(successResult.code, 0);
  assertEquals(successResult.stdout, "command output");
  assertEquals(successResult.stderr, "");

  const errorResult: CommandResult = {
    stdout: "",
    stderr: "command failed",
    success: false,
    code: 1,
  };

  assertEquals(errorResult.success, false);
  assertEquals(errorResult.code, 1);
  assertEquals(errorResult.stdout, "");
  assertEquals(errorResult.stderr, "command failed");
});

Deno.test("SSHConnectionConfig interface properties", () => {
  // Test minimal config
  const minimalConfig: SSHConnectionConfig = {
    host: "example.com",
    username: "user",
  };

  assertEquals(minimalConfig.host, "example.com");
  assertEquals(minimalConfig.username, "user");
  assertEquals(minimalConfig.port, undefined);
  assertEquals(minimalConfig.useAgent, undefined);

  // Test full config
  const fullConfig: SSHConnectionConfig = {
    host: "server.example.com",
    username: "admin",
    port: 2222,
    useAgent: true,
  };

  assertEquals(fullConfig.host, "server.example.com");
  assertEquals(fullConfig.username, "admin");
  assertEquals(fullConfig.port, 2222);
  assertEquals(fullConfig.useAgent, true);
});

// Test edge cases and error conditions
Deno.test("createSSHManagers - handles special characters in hostnames", () => {
  const hosts = [
    "server-1.example.com",
    "server_2.example.com",
    "192.168.1.100",
  ];
  const sshConfig = {
    username: "admin",
    port: 22,
    useAgent: true,
  };

  const managers = createSSHManagers(hosts, sshConfig);

  assertEquals(managers.length, 3);
  assertEquals(managers[0].getHost(), "server-1.example.com");
  assertEquals(managers[1].getHost(), "server_2.example.com");
  assertEquals(managers[2].getHost(), "192.168.1.100");

  // Clean up
  managers.forEach((manager) => manager.dispose());
});

Deno.test("Environment variable parsing edge cases", () => {
  // Test empty string environment variables
  setTestEnv({
    SSH_USERNAME: "",
    SSH_PORT: "",
  });

  const config = getDefaultSSHConfig();

  assertEquals(config.username, "root"); // Empty string falls back to "root"
  assertEquals(config.port, 22); // Empty string falls back to "22"
  assertEquals(config.useAgent, true);

  restoreEnv();
});

Deno.test("createSSHConfigFromJiji - preserves useAgent setting", () => {
  const config1 = createSSHConfigFromJiji({ user: "admin" });
  assertEquals(config1.useAgent, true);

  const config2 = createSSHConfigFromJiji();
  assertEquals(config2.useAgent, true);

  // The function always sets useAgent to true regardless of input
  const config3 = createSSHConfigFromJiji({ user: "admin", port: 22 });
  assertEquals(config3.useAgent, true);
});
