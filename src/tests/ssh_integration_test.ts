import { assertEquals, assertStringIncludes } from "@std/assert";
import { SSHConfiguration } from "../lib/configuration/ssh.ts";
import { createSSHConfigFromJiji, createSSHManagers } from "../utils/ssh.ts";

Deno.test("SSH Integration - configuration to utils bridge", () => {
  // Create SSH configuration from config file data
  const sshConfig = new SSHConfiguration({
    user: "deploy",
    port: 2222,
    key_path: "/home/deploy/.ssh/id_rsa",
    connect_timeout: 45,
    command_timeout: 600,
    options: {
      "StrictHostKeyChecking": "no",
      "UserKnownHostsFile": "/dev/null",
    },
  });

  // Convert to utils format using the bridge function
  const utilsConfig = createSSHConfigFromJiji({
    user: sshConfig.user,
    port: sshConfig.port,
  });

  assertEquals(utilsConfig.username, "deploy");
  assertEquals(utilsConfig.port, 2222);
  assertEquals(utilsConfig.useAgent, true);
});

Deno.test("SSH Integration - configuration buildSSHArgs matches expected format", () => {
  const sshConfig = new SSHConfiguration({
    user: "admin",
    port: 2222,
    key_path: "/path/to/key",
    connect_timeout: 60,
    options: {
      "ForwardAgent": "yes",
    },
  });

  const args = sshConfig.buildSSHArgs("production.example.com");

  // Verify standard options are included
  assertStringIncludes(args.join(" "), "StrictHostKeyChecking=no");
  assertStringIncludes(args.join(" "), "UserKnownHostsFile=/dev/null");
  assertStringIncludes(args.join(" "), "ConnectTimeout=60");
  assertStringIncludes(args.join(" "), "-p 2222");
  assertStringIncludes(args.join(" "), "-i /path/to/key");
  assertStringIncludes(args.join(" "), "ForwardAgent=yes");
  assertStringIncludes(args.join(" "), "admin@production.example.com");
});

Deno.test("SSH Integration - configuration to managers workflow", () => {
  // Simulate loading configuration from file
  const configData = {
    user: "webadmin",
    port: 2222,
    connect_timeout: 30,
  };

  const sshConfig = new SSHConfiguration(configData);

  // Validate configuration
  sshConfig.validate();

  // Convert to utils format for creating managers
  const utilsConfig = createSSHConfigFromJiji({
    user: sshConfig.user,
    port: sshConfig.port,
  });

  // Create managers for multiple hosts
  const hosts = ["web-1.example.com", "web-2.example.com"];
  const managers = createSSHManagers(hosts, utilsConfig);

  assertEquals(managers.length, 2);
  assertEquals(managers[0].getHost(), "web-1.example.com");
  assertEquals(managers[1].getHost(), "web-2.example.com");

  // Clean up
  managers.forEach((manager) => manager.dispose());
});

Deno.test("SSH Integration - configuration defaults work with utils", () => {
  // Use configuration defaults
  const sshConfig = SSHConfiguration.withDefaults({
    user: "root",
  });

  const utilsConfig = createSSHConfigFromJiji({
    user: sshConfig.user,
    port: sshConfig.port,
  });

  assertEquals(utilsConfig.username, "root");
  assertEquals(utilsConfig.port, 22);
  assertEquals(utilsConfig.useAgent, true);

  // Verify it works with manager creation
  const managers = createSSHManagers(["test.com"], utilsConfig);
  assertEquals(managers.length, 1);
  assertEquals(managers[0].getHost(), "test.com");

  managers[0].dispose();
});

Deno.test("SSH Integration - configuration options are preserved in args", () => {
  const sshConfig = new SSHConfiguration({
    user: "deploy",
    port: 22,
    options: {
      "IdentitiesOnly": "yes",
      "ServerAliveInterval": "60",
      "ServerAliveCountMax": "3",
    },
  });

  const args = sshConfig.buildSSHArgs("server.com");
  const argsString = args.join(" ");

  assertStringIncludes(argsString, "IdentitiesOnly=yes");
  assertStringIncludes(argsString, "ServerAliveInterval=60");
  assertStringIncludes(argsString, "ServerAliveCountMax=3");
  assertStringIncludes(argsString, "deploy@server.com");
});

Deno.test("SSH Integration - configuration toObject and reconstruction", () => {
  const originalConfig = new SSHConfiguration({
    user: "operator",
    port: 2222,
    key_path: "/etc/ssh/operator_key",
    connect_timeout: 45,
    command_timeout: 300,
    options: {
      "StrictHostKeyChecking": "yes",
    },
  });

  // Convert to object (as would be done for serialization)
  const configObj = originalConfig.toObject();

  // Reconstruct configuration
  const reconstructedConfig = new SSHConfiguration(configObj);

  // Verify all properties match
  assertEquals(reconstructedConfig.user, originalConfig.user);
  assertEquals(reconstructedConfig.port, originalConfig.port);
  assertEquals(reconstructedConfig.keyPath, originalConfig.keyPath);
  assertEquals(
    reconstructedConfig.connectTimeout,
    originalConfig.connectTimeout,
  );
  assertEquals(
    reconstructedConfig.commandTimeout,
    originalConfig.commandTimeout,
  );
  assertEquals(
    reconstructedConfig.options["StrictHostKeyChecking"],
    originalConfig.options["StrictHostKeyChecking"],
  );

  // Verify utils conversion still works
  const utilsConfig = createSSHConfigFromJiji({
    user: reconstructedConfig.user,
    port: reconstructedConfig.port,
  });

  assertEquals(utilsConfig.username, "operator");
  assertEquals(utilsConfig.port, 2222);
});

Deno.test("SSH Integration - complex real-world scenario", () => {
  // Simulate a complex deployment scenario
  const environments = {
    staging: {
      user: "staging-deploy",
      port: 2222,
      key_path: "/keys/staging.pem",
      connect_timeout: 30,
      options: {
        "StrictHostKeyChecking": "no",
        "UserKnownHostsFile": "/dev/null",
      },
    },
    production: {
      user: "prod-deploy",
      port: 22,
      key_path: "/keys/production.pem",
      connect_timeout: 60,
      options: {
        "StrictHostKeyChecking": "yes",
        "UserKnownHostsFile": "/etc/ssh/known_hosts",
        "IdentitiesOnly": "yes",
      },
    },
  };

  // Process each environment
  for (const [envName, envConfig] of Object.entries(environments)) {
    const sshConfig = new SSHConfiguration(envConfig);
    sshConfig.validate();

    // Convert for utils usage
    const utilsConfig = createSSHConfigFromJiji({
      user: sshConfig.user,
      port: sshConfig.port,
    });

    // Verify configuration is correctly transformed
    assertEquals(utilsConfig.username, envConfig.user);
    assertEquals(utilsConfig.port, envConfig.port);
    assertEquals(utilsConfig.useAgent, true);

    // Test SSH args generation
    const args = sshConfig.buildSSHArgs(`${envName}.example.com`);
    const argsString = args.join(" ");

    assertStringIncludes(
      argsString,
      `${envConfig.user}@${envName}.example.com`,
    );
    assertStringIncludes(argsString, `-p ${envConfig.port}`);
    assertStringIncludes(
      argsString,
      `ConnectTimeout=${envConfig.connect_timeout}`,
    );

    if (envConfig.key_path) {
      assertStringIncludes(argsString, `-i ${envConfig.key_path}`);
    }

    // Verify custom options are included
    for (const [option, value] of Object.entries(envConfig.options)) {
      assertStringIncludes(argsString, `${option}=${value}`);
    }
  }
});

Deno.test("SSH Integration - configuration validation prevents bad utils config", () => {
  // Test that configuration validation catches issues before they reach utils
  const badConfigs = [
    {
      // Missing user
      port: 22,
    },
    {
      user: "test",
      port: "invalid", // Invalid port type
    },
    {
      user: "test",
      connect_timeout: -1, // Invalid timeout
    },
    {
      user: "test",
      key_path: "", // Empty key path
    },
  ];

  for (const badConfig of badConfigs) {
    const sshConfig = new SSHConfiguration(badConfig);

    // Configuration validation should catch these issues
    let validationFailed = false;
    try {
      sshConfig.validate();
    } catch (_error) {
      validationFailed = true;
    }

    assertEquals(
      validationFailed,
      true,
      `Configuration should have failed validation: ${
        JSON.stringify(badConfig)
      }`,
    );
  }
});

Deno.test("SSH Integration - partial configuration with defaults", () => {
  // Test minimal configuration with defaults
  const minimalConfig = new SSHConfiguration({
    user: "minimal-user",
  });

  minimalConfig.validate();

  // Convert to utils format
  const utilsConfig = createSSHConfigFromJiji({
    user: minimalConfig.user,
    port: minimalConfig.port, // Should be default 22
  });

  assertEquals(utilsConfig.username, "minimal-user");
  assertEquals(utilsConfig.port, 22);
  assertEquals(utilsConfig.useAgent, true);

  // Generate SSH args with defaults
  const args = minimalConfig.buildSSHArgs("minimal.example.com");
  const argsString = args.join(" ");

  assertStringIncludes(argsString, "minimal-user@minimal.example.com");
  assertStringIncludes(argsString, "-p 22");
  assertStringIncludes(argsString, "ConnectTimeout=30"); // default
});
