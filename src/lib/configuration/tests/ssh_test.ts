import { assertEquals, assertThrows } from "@std/assert";
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
