import { assert, assertEquals, assertThrows } from "@std/assert";
import { SSHConfiguration } from "../ssh.ts";
import { ConfigurationError } from "../base.ts";

Deno.test("SSHConfiguration - basic configuration", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    port: 2222,
  });

  assertEquals(config.user, "testuser");
  assertEquals(config.port, 2222);
  assertEquals(config.connectTimeout, 30); // default
  assertEquals(config.commandTimeout, 300); // default
  assertEquals(config.keyPath, undefined);
  assertEquals(config.keyPassphrase, undefined);
  assertEquals(Object.keys(config.options).length, 0);
});

Deno.test("SSHConfiguration - default port", () => {
  const config = new SSHConfiguration({
    user: "testuser",
  });

  assertEquals(config.user, "testuser");
  assertEquals(config.port, 22); // default port
});

Deno.test("SSHConfiguration - with all options", () => {
  const config = new SSHConfiguration({
    user: "admin",
    port: 2222,
    key_path: "/home/user/.ssh/id_rsa",
    key_passphrase: "secret123",
    connect_timeout: 60,
    command_timeout: 600,
    options: {
      "StrictHostKeyChecking": "yes",
      "UserKnownHostsFile": "/home/user/.ssh/known_hosts",
    },
  });

  assertEquals(config.user, "admin");
  assertEquals(config.port, 2222);
  assertEquals(config.keyPath, "/home/user/.ssh/id_rsa");
  assertEquals(config.keyPassphrase, "secret123");
  assertEquals(config.connectTimeout, 60);
  assertEquals(config.commandTimeout, 600);
  assertEquals(config.options["StrictHostKeyChecking"], "yes");
  assertEquals(
    config.options["UserKnownHostsFile"],
    "/home/user/.ssh/known_hosts",
  );
});

Deno.test("SSHConfiguration - validation passes for valid config", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    port: 22,
    connect_timeout: 30,
    command_timeout: 300,
    key_path: "/home/user/.ssh/id_rsa",
    options: {
      "StrictHostKeyChecking": "no",
    },
  });

  // Should not throw
  config.validate();
});

Deno.test("SSHConfiguration - validation fails without user", () => {
  const config = new SSHConfiguration({
    port: 22,
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Missing required configuration: 'user' in ssh",
  );
});

Deno.test("SSHConfiguration - validation fails with invalid port", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    port: "not-a-number",
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "'port' in ssh must be a number",
  );
});

Deno.test("SSHConfiguration - validation fails with port out of range", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    port: 70000,
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "'port' in ssh must be a valid port number (1-65535)",
  );
});

Deno.test("SSHConfiguration - validation fails with negative connect timeout", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    connect_timeout: -5,
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "'connect_timeout' in ssh must be greater than 0",
  );
});

Deno.test("SSHConfiguration - validation fails with zero command timeout", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    command_timeout: 0,
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "'command_timeout' in ssh must be greater than 0",
  );
});

Deno.test("SSHConfiguration - validation fails with empty key path", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    key_path: "",
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "'key_path' in ssh cannot be empty",
  );
});

Deno.test("SSHConfiguration - validation fails with whitespace-only key path", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    key_path: "   ",
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "'key_path' in ssh cannot be empty",
  );
});

Deno.test("SSHConfiguration - validation fails with non-string option value", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    options: {
      "StrictHostKeyChecking": 123,
    },
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "SSH option 'StrictHostKeyChecking' must be a string",
  );
});

Deno.test("SSHConfiguration - toObject returns correct structure", () => {
  const config = new SSHConfiguration({
    user: "admin",
    port: 2222,
    key_path: "/path/to/key",
    connect_timeout: 60,
    command_timeout: 600,
    options: {
      "StrictHostKeyChecking": "no",
    },
  });

  const obj = config.toObject();

  assertEquals(obj, {
    user: "admin",
    port: 2222,
    key_path: "/path/to/key",
    connect_timeout: 60,
    command_timeout: 600,
    options: {
      "StrictHostKeyChecking": "no",
    },
  });
});

Deno.test("SSHConfiguration - toObject excludes defaults", () => {
  const config = new SSHConfiguration({
    user: "admin",
    // port defaults to 22
    // timeouts use defaults
  });

  const obj = config.toObject();

  assertEquals(obj, {
    user: "admin",
    port: 22,
  });
});

Deno.test("SSHConfiguration - buildSSHArgs basic", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    port: 22,
  });

  const args = config.buildSSHArgs("example.com");

  assertEquals(args, [
    "-o",
    "StrictHostKeyChecking=no",
    "-o",
    "UserKnownHostsFile=/dev/null",
    "-o",
    "ConnectTimeout=30",
    "-p",
    "22",
    "testuser@example.com",
  ]);
});

Deno.test("SSHConfiguration - buildSSHArgs with custom port and key", () => {
  const config = new SSHConfiguration({
    user: "admin",
    port: 2222,
    key_path: "/home/user/.ssh/id_rsa",
    connect_timeout: 60,
  });

  const args = config.buildSSHArgs("server.example.com");

  assertEquals(args, [
    "-o",
    "StrictHostKeyChecking=no",
    "-o",
    "UserKnownHostsFile=/dev/null",
    "-o",
    "ConnectTimeout=60",
    "-p",
    "2222",
    "-i",
    "/home/user/.ssh/id_rsa",
    "admin@server.example.com",
  ]);
});

Deno.test("SSHConfiguration - buildSSHArgs with custom options", () => {
  const config = new SSHConfiguration({
    user: "deploy",
    options: {
      "StrictHostKeyChecking": "yes",
      "UserKnownHostsFile": "/etc/ssh/known_hosts",
      "ForwardAgent": "yes",
    },
  });

  const args = config.buildSSHArgs("deploy.example.com");

  assertEquals(args, [
    "-o",
    "StrictHostKeyChecking=no",
    "-o",
    "UserKnownHostsFile=/dev/null",
    "-o",
    "ConnectTimeout=30",
    "-p",
    "22",
    "-o",
    "StrictHostKeyChecking=yes",
    "-o",
    "UserKnownHostsFile=/etc/ssh/known_hosts",
    "-o",
    "ForwardAgent=yes",
    "deploy@deploy.example.com",
  ]);
});

Deno.test("SSHConfiguration - withDefaults creates default config", () => {
  const config = SSHConfiguration.withDefaults();

  assertEquals(config.user, "root");
  assertEquals(config.port, 22);
  assertEquals(config.connectTimeout, 30);
  assertEquals(config.commandTimeout, 300);
});

Deno.test("SSHConfiguration - withDefaults accepts overrides", () => {
  const config = SSHConfiguration.withDefaults({
    user: "deploy",
    port: 2222,
    connect_timeout: 60,
  });

  assertEquals(config.user, "deploy");
  assertEquals(config.port, 2222);
  assertEquals(config.connectTimeout, 60);
  assertEquals(config.commandTimeout, 300); // still default
});

Deno.test("SSHConfiguration - lazy loading of properties", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    port: 2222,
  });

  // Access properties multiple times to ensure they're cached
  assertEquals(config.user, "testuser");
  assertEquals(config.user, "testuser");
  assertEquals(config.port, 2222);
  assertEquals(config.port, 2222);
});

Deno.test("SSHConfiguration - optional properties return undefined when not set", () => {
  const config = new SSHConfiguration({
    user: "testuser",
  });

  assertEquals(config.keyPath, undefined);
  assertEquals(config.keyPassphrase, undefined);
  assertEquals(Object.keys(config.options).length, 0);
});

// ============================================================================
// Proxy Configuration Tests
// ============================================================================

Deno.test("SSHConfiguration - proxy with hostname only", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    proxy: "bastion.example.com",
  });

  assertEquals(config.proxy, "bastion.example.com");
  assertEquals(config.proxyCommand, undefined);
});

Deno.test("SSHConfiguration - proxy with user and hostname", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    proxy: "root@bastion.example.com",
  });

  assertEquals(config.proxy, "root@bastion.example.com");
  assertEquals(config.proxyCommand, undefined);
});

Deno.test("SSHConfiguration - proxy with user, hostname, and port", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    proxy: "deploy@bastion.example.com:2222",
  });

  assertEquals(config.proxy, "deploy@bastion.example.com:2222");
  assertEquals(config.proxyCommand, undefined);
});

Deno.test("SSHConfiguration - proxy with hostname and port", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    proxy: "bastion.example.com:2222",
  });

  assertEquals(config.proxy, "bastion.example.com:2222");
  assertEquals(config.proxyCommand, undefined);
});

Deno.test("SSHConfiguration - proxy_command with placeholders", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    proxy_command: "ssh -W %h:%p user@proxy.example.com",
  });

  assertEquals(config.proxyCommand, "ssh -W %h:%p user@proxy.example.com");
  assertEquals(config.proxy, undefined);
});

Deno.test("SSHConfiguration - proxy_command with netcat", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    proxy_command: "nc -X connect -x proxy.example.com:3128 %h %p",
  });

  assertEquals(
    config.proxyCommand,
    "nc -X connect -x proxy.example.com:3128 %h %p",
  );
  assertEquals(config.proxy, undefined);
});

Deno.test("SSHConfiguration - proxy validation accepts various hostname formats", () => {
  // The regex is permissive and accepts various formats
  // Real validation happens during connection
  const validFormats = [
    "bastion.example.com",
    "root@bastion.example.com",
    "deploy@bastion.example.com:2222",
    "192.168.1.1",
    "user@192.168.1.1:22",
  ];

  for (const proxy of validFormats) {
    const config = new SSHConfiguration({
      user: "testuser",
      proxy,
    });
    // Should not throw
    config.validate();
  }
});

Deno.test("SSHConfiguration - proxy validation fails with invalid port", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    proxy: "bastion.example.com:70000",
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Invalid proxy port: 70000",
  );
});

Deno.test("SSHConfiguration - proxy validation fails with port 0", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    proxy: "bastion.example.com:0",
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Invalid proxy port: 0",
  );
});

Deno.test("SSHConfiguration - proxy_command validation fails without %h", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    proxy_command: "ssh -W proxy.example.com:%p",
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "'proxy_command' in ssh must contain %h and %p placeholders",
  );
});

Deno.test("SSHConfiguration - proxy_command validation fails without %p", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    proxy_command: "ssh -W %h:22 user@proxy.example.com",
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "'proxy_command' in ssh must contain %h and %p placeholders",
  );
});

Deno.test("SSHConfiguration - proxy_command validation fails without both placeholders", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    proxy_command: "ssh user@proxy.example.com",
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "'proxy_command' in ssh must contain %h and %p placeholders",
  );
});

Deno.test("SSHConfiguration - mutual exclusivity: proxy and proxy_command", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    proxy: "bastion.example.com",
    proxy_command: "ssh -W %h:%p user@proxy.example.com",
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Cannot specify both 'proxy' and 'proxy_command'",
  );
});

Deno.test("SSHConfiguration - proxy validation passes with valid proxy", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    proxy: "root@bastion.example.com:2222",
  });

  // Should not throw
  config.validate();
});

Deno.test("SSHConfiguration - proxy_command validation passes with valid command", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    proxy_command: "ssh -W %h:%p -i ~/.ssh/bastion_key bastion.example.com",
  });

  // Should not throw
  config.validate();
});

Deno.test("SSHConfiguration - toObject includes proxy when set", () => {
  const config = new SSHConfiguration({
    user: "admin",
    proxy: "bastion.example.com:2222",
  });

  const obj = config.toObject();

  assertEquals(obj, {
    user: "admin",
    port: 22,
    proxy: "bastion.example.com:2222",
  });
});

Deno.test("SSHConfiguration - toObject includes proxy_command when set", () => {
  const config = new SSHConfiguration({
    user: "admin",
    proxy_command: "ssh -W %h:%p user@proxy.example.com",
  });

  const obj = config.toObject();

  assertEquals(obj, {
    user: "admin",
    port: 22,
    proxy_command: "ssh -W %h:%p user@proxy.example.com",
  });
});

Deno.test("SSHConfiguration - proxy with IP address", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    proxy: "root@192.168.1.1:2222",
  });

  assertEquals(config.proxy, "root@192.168.1.1:2222");
  config.validate(); // Should not throw
});

Deno.test("SSHConfiguration - proxy without user defaults to SSH config", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    proxy: "bastion.example.com",
  });

  assertEquals(config.proxy, "bastion.example.com");
  config.validate(); // Should not throw
});

// ============================================================================
// Multiple Private Keys Tests
// ============================================================================

Deno.test("SSHConfiguration - keys array with single key", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    keys: ["~/.ssh/id_rsa"],
  });

  assertEquals(config.keys?.length, 1);
  assertEquals(config.keys?.[0].endsWith("/.ssh/id_rsa"), true);
});

Deno.test("SSHConfiguration - keys array with multiple keys", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    keys: ["~/.ssh/id_rsa", "~/.ssh/deploy_key", "/path/to/key"],
  });

  assertEquals(config.keys?.length, 3);
  assertEquals(config.keys?.[0].endsWith("/.ssh/id_rsa"), true);
  assertEquals(config.keys?.[1].endsWith("/.ssh/deploy_key"), true);
  assertEquals(config.keys?.[2], "/path/to/key");
});

Deno.test("SSHConfiguration - keys array validation fails for non-array", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    keys: "not-an-array" as unknown as string[],
  });

  assertThrows(
    () => config.keys,
    ConfigurationError,
    "'keys' in ssh must be an array",
  );
});

Deno.test("SSHConfiguration - keys array validation fails for non-string elements", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    keys: [123, "valid"] as unknown as string[],
  });

  assertThrows(
    () => config.keys,
    ConfigurationError,
    "'keys' in ssh must be an array of strings",
  );
});

Deno.test("SSHConfiguration - key_data array with environment variables", () => {
  // Set test environment variables
  Deno.env.set("TEST_SSH_KEY_1", "fake-key-content-1");
  Deno.env.set("TEST_SSH_KEY_2", "fake-key-content-2");

  const config = new SSHConfiguration({
    user: "testuser",
    key_data: ["TEST_SSH_KEY_1", "TEST_SSH_KEY_2"],
  });

  assertEquals(config.keyData?.length, 2);
  assertEquals(config.keyData?.[0], "fake-key-content-1");
  assertEquals(config.keyData?.[1], "fake-key-content-2");

  // Cleanup
  Deno.env.delete("TEST_SSH_KEY_1");
  Deno.env.delete("TEST_SSH_KEY_2");
});

Deno.test("SSHConfiguration - key_data validation fails for missing env var", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    key_data: ["NONEXISTENT_ENV_VAR"],
  });

  assertThrows(
    () => config.keyData,
    ConfigurationError,
    "Environment variable 'NONEXISTENT_ENV_VAR' not found for key_data",
  );
});

Deno.test("SSHConfiguration - key_data validation fails for non-array", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    key_data: "not-an-array" as unknown as string[],
  });

  assertThrows(
    () => config.keyData,
    ConfigurationError,
    "'key_data' in ssh must be an array",
  );
});

Deno.test("SSHConfiguration - key_data validation fails for non-string elements", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    key_data: [123] as unknown as string[],
  });

  assertThrows(
    () => config.keyData,
    ConfigurationError,
    "'key_data' in ssh must be an array of environment variable names",
  );
});

Deno.test("SSHConfiguration - keys_only flag defaults to false", () => {
  const config = new SSHConfiguration({
    user: "testuser",
  });

  assertEquals(config.keysOnly, false);
});

Deno.test("SSHConfiguration - keys_only can be set to true", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    keys_only: true,
  });

  assertEquals(config.keysOnly, true);
});

Deno.test("SSHConfiguration - keys_only can be set to false", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    keys_only: false,
  });

  assertEquals(config.keysOnly, false);
});

Deno.test("SSHConfiguration - keys_only validation fails for non-boolean", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    keys_only: "yes" as unknown as boolean,
  });

  assertThrows(
    () => config.keysOnly,
    ConfigurationError,
    "'keys_only' in ssh must be a boolean",
  );
});

Deno.test("SSHConfiguration - allKeys returns keys array", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    keys: ["~/.ssh/id_rsa", "~/.ssh/deploy_key"],
  });

  const allKeys = config.allKeys;
  assertEquals(allKeys.length, 2);
  assertEquals(allKeys[0].endsWith("/.ssh/id_rsa"), true);
  assertEquals(allKeys[1].endsWith("/.ssh/deploy_key"), true);
});

Deno.test("SSHConfiguration - allKeys returns empty array when no keys", () => {
  const config = new SSHConfiguration({
    user: "testuser",
  });

  assertEquals(config.allKeys, []);
});

Deno.test("SSHConfiguration - toObject includes keys", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    keys: ["~/.ssh/id_rsa", "~/.ssh/deploy_key"],
  });

  const obj = config.toObject();
  assertEquals(obj.keys, config.keys);
});

Deno.test("SSHConfiguration - toObject masks key_data for security", () => {
  Deno.env.set("TEST_KEY", "secret-content");

  const config = new SSHConfiguration({
    user: "testuser",
    key_data: ["TEST_KEY"],
  });

  const obj = config.toObject();
  assertEquals(obj.key_data, "[1 key(s) from environment]");

  Deno.env.delete("TEST_KEY");
});

Deno.test("SSHConfiguration - toObject includes keys_only when true", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    keys_only: true,
  });

  const obj = config.toObject();
  assertEquals(obj.keys_only, true);
});

Deno.test("SSHConfiguration - toObject excludes keys_only when false", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    keys_only: false,
  });

  const obj = config.toObject();
  assertEquals(obj.keys_only, undefined);
});

Deno.test("SSHConfiguration - SSH config file support - boolean true", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    config: true,
  });

  const configFiles = config.sshConfigFiles;
  assertEquals(Array.isArray(configFiles), true);
  if (Array.isArray(configFiles)) {
    assertEquals(configFiles.length >= 1, true);
    assertEquals(configFiles.some((f) => f.includes("/.ssh/config")), true);
  }
});

Deno.test("SSHConfiguration - SSH config file support - single string", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    config: "~/.ssh/custom_config",
  });

  const configFiles = config.sshConfigFiles;
  assertEquals(Array.isArray(configFiles), true);
  if (Array.isArray(configFiles)) {
    assertEquals(configFiles.length, 1);
    assertEquals(configFiles[0].endsWith("/.ssh/custom_config"), true);
  }
});

Deno.test("SSHConfiguration - SSH config file support - array", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    config: ["~/.ssh/config1", "/etc/ssh/config2"],
  });

  const configFiles = config.sshConfigFiles;
  assertEquals(Array.isArray(configFiles), true);
  if (Array.isArray(configFiles)) {
    assertEquals(configFiles.length, 2);
    assertEquals(configFiles[0].endsWith("/.ssh/config1"), true);
    assertEquals(configFiles[1], "/etc/ssh/config2");
  }
});

Deno.test("SSHConfiguration - SSH config file support - false", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    config: false,
  });

  const configFiles = config.sshConfigFiles;
  assertEquals(configFiles, false);
});

Deno.test("SSHConfiguration - SSH config file support - undefined", () => {
  const config = new SSHConfiguration({
    user: "testuser",
  });

  const configFiles = config.sshConfigFiles;
  assertEquals(configFiles, false);
});

Deno.test("SSHConfiguration - SSH config file support - invalid type", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    config: 123,
  });

  assertThrows(
    () => config.sshConfigFiles,
    ConfigurationError,
    "'config' in ssh must be boolean, string, or array of strings",
  );
});

Deno.test("SSHConfiguration - SSH config file support - invalid array element", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    config: ["valid.config", 123],
  });

  assertThrows(
    () => config.sshConfigFiles,
    ConfigurationError,
    "'config' in ssh must be boolean, string, or array of strings",
  );
});

Deno.test("SSHConfiguration - SSH config file support - toObject", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    config: ["~/.ssh/config1", "~/.ssh/config2"],
  });

  const obj = config.toObject();
  assertEquals(Array.isArray(obj.config), true);
  if (Array.isArray(obj.config)) {
    assertEquals(obj.config.length, 2);
    assertEquals(obj.config[0].endsWith("/.ssh/config1"), true);
    assertEquals(obj.config[1].endsWith("/.ssh/config2"), true);
  }
});

Deno.test("SSHConfiguration - SSH config file support - toObject false", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    config: false,
  });

  const obj = config.toObject();
  assert(!("config" in obj));
});

Deno.test("SSHConfiguration - log level defaults to error", () => {
  const config = new SSHConfiguration({
    user: "testuser",
  });

  assertEquals(config.logLevel, "error");
});

Deno.test("SSHConfiguration - log level can be set to debug", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    log_level: "debug",
  });

  assertEquals(config.logLevel, "debug");
});

Deno.test("SSHConfiguration - log level can be set to info", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    log_level: "info",
  });

  assertEquals(config.logLevel, "info");
});

Deno.test("SSHConfiguration - log level can be set to warn", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    log_level: "warn",
  });

  assertEquals(config.logLevel, "warn");
});

Deno.test("SSHConfiguration - log level can be set to error", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    log_level: "error",
  });

  assertEquals(config.logLevel, "error");
});

Deno.test("SSHConfiguration - log level can be set to fatal", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    log_level: "fatal",
  });

  assertEquals(config.logLevel, "fatal");
});

Deno.test("SSHConfiguration - log level validation fails for non-string", () => {
  assertThrows(
    () => {
      new SSHConfiguration({
        user: "testuser",
        log_level: 123,
      }).logLevel;
    },
    ConfigurationError,
    "'log_level' in ssh must be a string",
  );
});

Deno.test("SSHConfiguration - log level validation fails for invalid level", () => {
  assertThrows(
    () => {
      new SSHConfiguration({
        user: "testuser",
        log_level: "invalid",
      }).logLevel;
    },
    ConfigurationError,
    "'log_level' in ssh must be one of: debug, info, warn, error, fatal. Got: invalid",
  );
});

Deno.test("SSHConfiguration - toObject includes log_level when not default", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    log_level: "debug",
  });

  const obj = config.toObject();
  assertEquals(obj.log_level, "debug");
});

Deno.test("SSHConfiguration - toObject excludes log_level when default", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    log_level: "error",
  });

  const obj = config.toObject();
  assert(!("log_level" in obj));
});

Deno.test("SSHConfiguration - toObject excludes log_level when not set", () => {
  const config = new SSHConfiguration({
    user: "testuser",
  });

  const obj = config.toObject();
  assert(!("log_level" in obj));
});
