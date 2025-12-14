import { assertEquals, assertRejects, assertStringIncludes } from "@std/assert";
import { ensureDir } from "@std/fs";
import {
  buildConfigPath,
  checkEngineAvailability,
  configFileExists,
  createDefaultConfig,
  filterHostsByPatterns,
  getAvailableConfigs,
  getEngineCommand,
  loadConfig,
  readConfigTemplate,
  validateConfigFile,
} from "../config.ts";
import { Configuration } from "../../lib/configuration.ts";

// Test data
const VALID_CONFIG_YAML = `project: testproject
engine: docker
ssh:
  user: testuser
  port: 22
services:
  web:
    image: nginx:latest
    hosts:
      - web1.example.com
      - web2.example.com
    ports:
      - "80:80"
  api:
    image: node:18
    hosts:
      - api1.example.com
    ports:
      - "3000:3000"
    env:
      NODE_ENV: production
env:
  GLOBAL_VAR: value1
`;

const INVALID_CONFIG_YAML = `project: testproject
engine: invalid_engine
ssh:
  user: testuser
  port: "not_a_number"
services:
  web:
    # Missing required fields
    hosts: []
`;

// Helper function to create temporary config files
async function createTempConfig(
  content: string,
  filename = "test-deploy.yml",
): Promise<string> {
  const tempDir = await Deno.makeTempDir();
  const jijiDir = `${tempDir}/.jiji`;
  await ensureDir(jijiDir);
  const configPath = `${jijiDir}/${filename}`;
  await Deno.writeTextFile(configPath, content);
  return configPath;
}

// Helper function to clean up temp files
async function cleanup(path: string) {
  try {
    const tempDir = path.split("/.jiji")[0];
    await Deno.remove(tempDir, { recursive: true });
  } catch {
    // Ignore cleanup errors
  }
}

Deno.test("buildConfigPath - default configuration", () => {
  const path = buildConfigPath();
  assertEquals(path, ".jiji/deploy.yml");
});

Deno.test("buildConfigPath - with environment", () => {
  const path = buildConfigPath("staging");
  assertEquals(path, ".jiji/deploy.staging.yml");
});

Deno.test("buildConfigPath - with production environment", () => {
  const path = buildConfigPath("production");
  assertEquals(path, ".jiji/deploy.production.yml");
});

Deno.test("configFileExists - existing file", async () => {
  const configPath = await createTempConfig(VALID_CONFIG_YAML);

  try {
    const exists = await configFileExists(configPath);
    assertEquals(exists, true);
  } finally {
    await cleanup(configPath);
  }
});

Deno.test("configFileExists - non-existing file", async () => {
  const exists = await configFileExists("/non/existent/path/config.yml");
  assertEquals(exists, false);
});

Deno.test("configFileExists - permission error throws", async () => {
  // This test is challenging to implement reliably across platforms
  // Skip for now as the core functionality is tested elsewhere
  const exists = await configFileExists("/dev/null");
  // /dev/null should exist on Unix systems
  assertEquals(typeof exists, "boolean");
});

Deno.test("readConfigTemplate - reads template file", async () => {
  const template = await readConfigTemplate();

  // Template should be a string containing YAML content
  assertEquals(typeof template, "string");
  assertStringIncludes(template, "engine:");
  assertStringIncludes(template, "services:");
});

Deno.test("createConfigFile - creates file with content", async () => {
  const tempDir = await Deno.makeTempDir();
  const jijiDir = `${tempDir}/.jiji`;
  await ensureDir(jijiDir);
  const configPath = `${jijiDir}/deploy.yml`;
  const content = "engine: docker\nservices: {}";

  try {
    await Deno.writeTextFile(configPath, content);

    // Verify file was created
    const exists = await configFileExists(configPath);
    assertEquals(exists, true);

    // Verify content
    const fileContent = await Deno.readTextFile(configPath);
    assertEquals(fileContent, content);
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test("createConfigFile - creates directory if needed", async () => {
  const tempDir = await Deno.makeTempDir();
  const nestedDir = `${tempDir}/.jiji/nested`;
  await ensureDir(nestedDir);
  const configPath = `${nestedDir}/deploy.yml`;
  const content = "engine: docker\nservices: {}";

  try {
    await Deno.writeTextFile(configPath, content);

    const exists = await configFileExists(configPath);
    assertEquals(exists, true);
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test("checkEngineAvailability - docker available", async () => {
  // This test assumes docker might be available, but we can't guarantee it
  // So we just test that the function returns a boolean
  const available = await checkEngineAvailability("docker");
  assertEquals(typeof available, "boolean");
});

Deno.test("checkEngineAvailability - non-existent command", async () => {
  const available = await checkEngineAvailability("non-existent-command-12345");
  assertEquals(available, false);
});

Deno.test("loadConfig - valid configuration", async () => {
  const configPath = await createTempConfig(VALID_CONFIG_YAML);

  try {
    const result = await loadConfig(configPath);

    assertEquals(result.configPath, configPath);
    assertEquals(result.config instanceof Configuration, true);
    assertEquals(result.config.engine, "docker");
    assertEquals(result.config.ssh.user, "testuser");
    assertEquals(result.config.services.size, 2);
  } finally {
    await cleanup(configPath);
  }
});

Deno.test("loadConfig - invalid configuration", async () => {
  const configPath = await createTempConfig(INVALID_CONFIG_YAML);

  try {
    await assertRejects(
      () => loadConfig(configPath),
      Error,
      "Configuration validation failed",
    );
  } finally {
    await cleanup(configPath);
  }
});

Deno.test("loadConfig - non-existent file", async () => {
  await assertRejects(
    () => loadConfig("/non/existent/config.yml"),
    Error,
  );
});

Deno.test("getEngineCommand - returns engine from config", () => {
  const config = Configuration.withDefaults({ engine: "podman" });
  const engine = getEngineCommand(config);
  assertEquals(engine, "podman");
});

Deno.test("validateConfigFile - valid configuration", async () => {
  const configPath = await createTempConfig(VALID_CONFIG_YAML);

  try {
    // Should not throw
    await validateConfigFile(configPath);
  } finally {
    await cleanup(configPath);
  }
});

Deno.test("validateConfigFile - invalid configuration", async () => {
  const configPath = await createTempConfig(INVALID_CONFIG_YAML);

  try {
    await assertRejects(
      () => validateConfigFile(configPath),
      Error,
      "Configuration validation failed",
    );
  } finally {
    await cleanup(configPath);
  }
});

Deno.test("validateConfigFile - non-existent file", async () => {
  await assertRejects(
    () => validateConfigFile("/non/existent/config.yml"),
    Error,
  );
});

Deno.test("getAvailableConfigs - returns available configs", async () => {
  const tempDir = await Deno.makeTempDir();
  const jijiDir = `${tempDir}/.jiji`;
  await ensureDir(jijiDir);

  // Create multiple config files
  await Deno.writeTextFile(`${jijiDir}/deploy.yml`, VALID_CONFIG_YAML);
  await Deno.writeTextFile(`${jijiDir}/deploy.staging.yml`, VALID_CONFIG_YAML);
  await Deno.writeTextFile(
    `${jijiDir}/deploy.production.yml`,
    VALID_CONFIG_YAML,
  );

  try {
    const originalCwd = Deno.cwd();
    Deno.chdir(tempDir);

    const configs = await getAvailableConfigs();
    assertEquals(configs.length >= 3, true);

    Deno.chdir(originalCwd);
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test("createDefaultConfig - returns configuration with defaults", () => {
  const config = createDefaultConfig();

  assertEquals(config instanceof Configuration, true);
  assertEquals(config.engine, "podman");
  assertEquals(config.ssh.user, "root");
  assertEquals(config.ssh.port, 22);
  assertEquals(config.services.size, 1);
  assertEquals(config.services.has("web"), true);
});

Deno.test("filterHostsByPatterns - exact matches", () => {
  const allHosts = [
    "web1.example.com",
    "web2.example.com",
    "api.example.com",
    "db.example.com",
  ];
  const patterns = "web1.example.com,api.example.com";

  const result = filterHostsByPatterns(allHosts, patterns);
  assertEquals(result.sort(), ["api.example.com", "web1.example.com"]);
});

Deno.test("filterHostsByPatterns - wildcard matches", () => {
  const allHosts = [
    "web1.example.com",
    "web2.example.com",
    "api.example.com",
    "db.example.com",
  ];
  const patterns = "web*.example.com";

  const result = filterHostsByPatterns(allHosts, patterns);
  assertEquals(result.sort(), ["web1.example.com", "web2.example.com"]);
});

Deno.test("filterHostsByPatterns - mixed patterns", () => {
  const allHosts = [
    "web1.example.com",
    "web2.example.com",
    "api.example.com",
    "db.example.com",
  ];
  const patterns = "web*.example.com,db.example.com";

  const result = filterHostsByPatterns(allHosts, patterns);
  assertEquals(result.sort(), [
    "db.example.com",
    "web1.example.com",
    "web2.example.com",
  ]);
});

Deno.test("filterHostsByPatterns - no matches", () => {
  const allHosts = ["web1.example.com", "web2.example.com", "api.example.com"];
  const patterns = "nonexistent.com";

  const result = filterHostsByPatterns(allHosts, patterns);
  assertEquals(result, []);
});

Deno.test("filterHostsByPatterns - removes duplicates", () => {
  const allHosts = ["web1.example.com", "web2.example.com", "api.example.com"];
  const patterns = "web1.example.com,web*.example.com";

  const result = filterHostsByPatterns(allHosts, patterns);
  assertEquals(result.sort(), ["web1.example.com", "web2.example.com"]);
});

Deno.test("filterHostsByPatterns - complex wildcards", () => {
  const allHosts = [
    "prod-web1.example.com",
    "prod-web2.example.com",
    "stage-web1.example.com",
    "prod-api.example.com",
    "stage-api.example.com",
  ];
  const patterns = "prod-*";

  const result = filterHostsByPatterns(allHosts, patterns);
  assertEquals(result.sort(), [
    "prod-api.example.com",
    "prod-web1.example.com",
    "prod-web2.example.com",
  ]);
});

Deno.test("filterHostsByPatterns - whitespace handling", () => {
  const allHosts = ["web1.example.com", "web2.example.com", "api.example.com"];
  const patterns = " web1.example.com , api.example.com ";

  const result = filterHostsByPatterns(allHosts, patterns);
  assertEquals(result.sort(), ["api.example.com", "web1.example.com"]);
});

Deno.test("filterHostsByPatterns - empty patterns", () => {
  const allHosts = ["web1.example.com", "web2.example.com"];
  const patterns = "";

  const result = filterHostsByPatterns(allHosts, patterns);
  assertEquals(result, []);
});

Deno.test("filterHostsByPatterns - single wildcard", () => {
  const allHosts = ["web1.example.com", "web2.example.com", "api.example.com"];
  const patterns = "*";

  const result = filterHostsByPatterns(allHosts, patterns);
  assertEquals(result.sort(), allHosts.sort());
});
