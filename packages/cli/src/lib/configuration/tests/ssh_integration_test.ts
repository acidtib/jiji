import { assertEquals, assertThrows } from "@std/assert";
import { SSHConfiguration } from "../ssh.ts";

Deno.test("SSH Config Integration - End-to-end SSH config file loading", async () => {
  // Create a temporary SSH config file
  const tempDir = await Deno.makeTempDir();
  const sshConfigPath = `${tempDir}/ssh_config`;

  const sshConfigContent = `
# Test SSH configuration
Host web*.example.com
    User webdeploy
    Port 2222
    ProxyJump bastion.example.com
    IdentityFile ~/.ssh/web_key
    ConnectTimeout 45

Host db.example.com
    User dbadmin
    Port 5432
    ProxyCommand ssh -W %h:%p gateway.example.com
    IdentityFile ~/.ssh/db_key

Host bastion.example.com
    User admin
    Port 22
    IdentityFile ~/.ssh/bastion_key

Host *
    User root
    ConnectTimeout 30
    ServerAliveInterval 60
`;

  await Deno.writeTextFile(sshConfigPath, sshConfigContent);

  try {
    // Test with SSH config file enabled
    const config = new SSHConfiguration({
      user: "deploy", // This should override SSH config
      config: sshConfigPath,
    });

    // Verify config file is loaded
    const configFiles = config.sshConfigFiles;
    assertEquals(Array.isArray(configFiles), true);
    if (Array.isArray(configFiles)) {
      assertEquals(configFiles.length, 1);
      assertEquals(configFiles[0], sshConfigPath);
    }

    // Test that basic properties work
    assertEquals(config.user, "deploy");
    assertEquals(config.port, 22);
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test("SSH Config Integration - Default config files detection", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    config: true, // Load default config files
  });

  const configFiles = config.sshConfigFiles;
  assertEquals(Array.isArray(configFiles), true);

  if (Array.isArray(configFiles)) {
    // Should include user's SSH config
    const hasUserConfig = configFiles.some((file) =>
      file.includes("/.ssh/config")
    );
    assertEquals(hasUserConfig, true);

    // On non-Windows systems, should include system config
    if (Deno.build.os !== "windows") {
      const hasSystemConfig = configFiles.some((file) =>
        file === "/etc/ssh/ssh_config"
      );
      assertEquals(hasSystemConfig, true);
    }
  }
});

Deno.test("SSH Config Integration - Multiple config files", async () => {
  const tempDir = await Deno.makeTempDir();
  const config1Path = `${tempDir}/config1`;
  const config2Path = `${tempDir}/config2`;

  const config1Content = `
Host web.example.com
    User webuser
    Port 8080
`;

  const config2Content = `
Host api.example.com
    User apiuser
    Port 8443
`;

  await Deno.writeTextFile(config1Path, config1Content);
  await Deno.writeTextFile(config2Path, config2Content);

  try {
    const config = new SSHConfiguration({
      user: "defaultuser",
      config: [config1Path, config2Path],
    });

    const configFiles = config.sshConfigFiles;
    assertEquals(Array.isArray(configFiles), true);
    if (Array.isArray(configFiles)) {
      assertEquals(configFiles.length, 2);
      assertEquals(configFiles[0], config1Path);
      assertEquals(configFiles[1], config2Path);
    }
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test("SSH Config Integration - Path expansion", () => {
  // Mock HOME environment variable for testing
  const originalHome = Deno.env.get("HOME");
  const testHome = "/home/testuser";

  try {
    Deno.env.set("HOME", testHome);

    const config = new SSHConfiguration({
      user: "testuser",
      config: "~/.ssh/custom_config",
    });

    const configFiles = config.sshConfigFiles;
    assertEquals(Array.isArray(configFiles), true);
    if (Array.isArray(configFiles)) {
      assertEquals(configFiles[0], `${testHome}/.ssh/custom_config`);
    }
  } finally {
    // Restore original HOME
    if (originalHome) {
      Deno.env.set("HOME", originalHome);
    } else {
      Deno.env.delete("HOME");
    }
  }
});

Deno.test("SSH Config Integration - Config disabled by default", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    // No config property specified
  });

  const configFiles = config.sshConfigFiles;
  assertEquals(configFiles, false);
});

Deno.test("SSH Config Integration - Config explicitly disabled", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    config: false,
  });

  const configFiles = config.sshConfigFiles;
  assertEquals(configFiles, false);
});

Deno.test("SSH Config Integration - Invalid config types", () => {
  // Test with number (invalid type)
  const config1 = new SSHConfiguration({
    user: "testuser",
    config: 123,
  });

  assertThrows(
    () => config1.sshConfigFiles,
    Error,
    "'config' in ssh must be boolean, string, or array of strings",
  );

  // Test with mixed array types (invalid)
  const config2 = new SSHConfiguration({
    user: "testuser",
    config: ["valid.config", 456],
  });

  assertThrows(
    () => config2.sshConfigFiles,
    Error,
    "'config' in ssh must be boolean, string, or array of strings",
  );
});

Deno.test("SSH Config Integration - toObject serialization", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    port: 2222,
    config: ["~/.ssh/config1", "~/.ssh/config2"],
  });

  const obj = config.toObject();

  assertEquals(obj.user, "testuser");
  assertEquals(obj.port, 2222);
  assertEquals(Array.isArray(obj.config), true);

  if (Array.isArray(obj.config)) {
    assertEquals(obj.config.length, 2);
    assertEquals(obj.config[0].endsWith("/.ssh/config1"), true);
    assertEquals(obj.config[1].endsWith("/.ssh/config2"), true);
  }
});

Deno.test("SSH Config Integration - toObject excludes false config", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    config: false,
  });

  const obj = config.toObject();

  assertEquals(obj.user, "testuser");
  assertEquals(obj.config, undefined); // Should be excluded when false
});

Deno.test("SSH Config Integration - Validation passes with config files", () => {
  const config = new SSHConfiguration({
    user: "testuser",
    port: 22,
    config: true,
  });

  // Should not throw
  config.validate();

  // Properties should be accessible
  assertEquals(config.user, "testuser");
  assertEquals(config.port, 22);
  assertEquals(typeof config.sshConfigFiles, "object");
});

Deno.test("SSH Config Integration - Home directory expansion without HOME env", () => {
  const originalHome = Deno.env.get("HOME");
  const originalUserProfile = Deno.env.get("USERPROFILE");

  try {
    // Remove both HOME and USERPROFILE
    Deno.env.delete("HOME");
    Deno.env.delete("USERPROFILE");

    const config = new SSHConfiguration({
      user: "testuser",
      config: "~/.ssh/config",
    });

    assertThrows(
      () => config.sshConfigFiles,
      Error,
      "Cannot expand ~ without HOME environment variable",
    );
  } finally {
    // Restore environment variables
    if (originalHome) {
      Deno.env.set("HOME", originalHome);
    }
    if (originalUserProfile) {
      Deno.env.set("USERPROFILE", originalUserProfile);
    }
  }
});
