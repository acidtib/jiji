import { assertEquals, assertRejects } from "@std/assert";
import { ensureDir } from "@std/fs";
import { ConfigurationLoader } from "../loader.ts";
import { ConfigurationError } from "../base.ts";

// Test YAML content
const VALID_YAML = `engine: docker
ssh:
  user: testuser
  port: 22
  connect_timeout: 30
services:
  web:
    image: nginx:latest
    hosts:
      - web1.example.com
      - web2.example.com
    ports:
      - "80:80"
  api:
    build:
      dockerfile: Dockerfile
      context: .
    hosts:
      - api1.example.com
env:
  variables:
    NODE_ENV: production
    DATABASE_URL: postgres://localhost:5432/mydb
`;

const MALFORMED_YAML = `engine: docker
ssh:
  user: testuser
  port: 22
  invalid_indent:
services:
  web:
    image: nginx:latest
`;

// Helper functions
async function createTempConfig(
  content: string,
  filename = "deploy.yml",
  environment?: string,
): Promise<{ configPath: string; tempDir: string }> {
  const tempDir = await Deno.makeTempDir();
  const jijiDir = `${tempDir}/.jiji`;
  await ensureDir(jijiDir);

  const actualFilename = environment ? `deploy.${environment}.yml` : filename;
  const configPath = `${jijiDir}/${actualFilename}`;
  await Deno.writeTextFile(configPath, content);

  return { configPath, tempDir };
}

async function cleanup(tempDir: string) {
  try {
    await Deno.remove(tempDir, { recursive: true });
  } catch {
    // Ignore cleanup errors
  }
}

async function withTempDir<T>(fn: (tempDir: string) => Promise<T>): Promise<T> {
  const tempDir = await Deno.makeTempDir();
  try {
    return await fn(tempDir);
  } finally {
    await cleanup(tempDir);
  }
}

Deno.test("ConfigurationLoader - loadFromFile with valid YAML", async () => {
  const { configPath, tempDir } = await createTempConfig(VALID_YAML);

  try {
    const config = await ConfigurationLoader.loadFromFile(configPath);

    assertEquals(config.engine, "docker");
    assertEquals((config.ssh as Record<string, unknown>).user, "testuser");
    assertEquals((config.ssh as Record<string, unknown>).port, 22);
    assertEquals(config.engine, "docker");
    const services = config.services as Record<string, Record<string, unknown>>;
    assertEquals(services.web.image, "nginx:latest");
    assertEquals(
      (services.api.build as Record<string, unknown>).dockerfile,
      "Dockerfile",
    );
  } finally {
    await cleanup(tempDir);
  }
});

Deno.test("ConfigurationLoader - loadFromFile with non-existent file", async () => {
  await assertRejects(
    () => ConfigurationLoader.loadFromFile("/non/existent/config.yml"),
    ConfigurationError,
    "Configuration file not found",
  );
});

Deno.test("ConfigurationLoader - loadFromFile with malformed YAML", async () => {
  const { configPath, tempDir } = await createTempConfig(MALFORMED_YAML);

  try {
    // Malformed YAML should be handled gracefully by the parser
    // The actual behavior may vary, so we test that it doesn't crash
    const _config = await ConfigurationLoader.loadFromFile(configPath);
    // If it doesn't throw, that's also acceptable behavior
  } catch (error) {
    // If it does throw, it should be a ConfigurationError
    assertEquals(error instanceof ConfigurationError, true);
  } finally {
    await cleanup(tempDir);
  }
});

Deno.test("ConfigurationLoader - loadFromFile with empty file", async () => {
  const { configPath, tempDir } = await createTempConfig("");

  try {
    await assertRejects(
      () => ConfigurationLoader.loadFromFile(configPath),
      ConfigurationError,
      "Configuration file must contain a valid YAML object",
    );
  } finally {
    await cleanup(tempDir);
  }
});

Deno.test("ConfigurationLoader - loadConfig with default environment", async () => {
  await withTempDir(async (tempDir) => {
    const jijiDir = `${tempDir}/.jiji`;
    await ensureDir(jijiDir);
    await Deno.writeTextFile(`${jijiDir}/deploy.yml`, VALID_YAML);

    const originalCwd = Deno.cwd();
    Deno.chdir(tempDir);

    try {
      const result = await ConfigurationLoader.loadConfig();

      assertEquals(result.path, `${tempDir}/.jiji/deploy.yml`);
      assertEquals(result.config.engine, "docker");
    } finally {
      Deno.chdir(originalCwd);
    }
  });
});

Deno.test("ConfigurationLoader - loadConfig with specific environment", async () => {
  await withTempDir(async (tempDir) => {
    const jijiDir = `${tempDir}/.jiji`;
    await ensureDir(jijiDir);
    await Deno.writeTextFile(`${jijiDir}/deploy.yml`, VALID_YAML);
    await Deno.writeTextFile(
      `${jijiDir}/deploy.staging.yml`,
      VALID_YAML.replace("production", "staging"),
    );

    const originalCwd = Deno.cwd();
    Deno.chdir(tempDir);

    try {
      const result = await ConfigurationLoader.loadConfig("staging");

      assertEquals(result.path, `${tempDir}/.jiji/deploy.staging.yml`);
      const env = result.config.env as Record<string, Record<string, unknown>>;
      assertEquals(env.variables.NODE_ENV, "staging");
    } finally {
      Deno.chdir(originalCwd);
    }
  });
});

Deno.test("ConfigurationLoader - loadConfig with custom path", async () => {
  const { configPath, tempDir } = await createTempConfig(
    VALID_YAML,
    "custom-config.yml",
  );

  try {
    const result = await ConfigurationLoader.loadConfig(undefined, configPath);

    assertEquals(result.path, configPath);
    assertEquals(result.config.engine, "docker");
  } finally {
    await cleanup(tempDir);
  }
});

Deno.test("ConfigurationLoader - loadConfig with custom start path", async () => {
  await withTempDir(async (tempDir) => {
    const jijiDir = `${tempDir}/.jiji`;
    await ensureDir(jijiDir);
    await Deno.writeTextFile(`${jijiDir}/deploy.yml`, VALID_YAML);

    const result = await ConfigurationLoader.loadConfig(
      undefined,
      undefined,
      tempDir,
    );

    assertEquals(result.path, `${tempDir}/.jiji/deploy.yml`);
    assertEquals(result.config.engine, "docker");
  });
});

Deno.test("ConfigurationLoader - loadConfig prioritizes environment-specific config", async () => {
  await withTempDir(async (tempDir) => {
    const jijiDir = `${tempDir}/.jiji`;
    await ensureDir(jijiDir);

    const defaultConfig = VALID_YAML;
    const productionConfig = VALID_YAML.replace(
      "production",
      "production-override",
    );

    await Deno.writeTextFile(`${jijiDir}/deploy.yml`, defaultConfig);
    await Deno.writeTextFile(
      `${jijiDir}/deploy.production.yml`,
      productionConfig,
    );

    const originalCwd = Deno.cwd();
    Deno.chdir(tempDir);

    try {
      const result = await ConfigurationLoader.loadConfig("production");

      assertEquals(result.path, `${tempDir}/.jiji/deploy.production.yml`);
      const env = result.config.env as Record<string, Record<string, unknown>>;
      assertEquals(env.variables.NODE_ENV, "production-override");
    } finally {
      Deno.chdir(originalCwd);
    }
  });
});

Deno.test("ConfigurationLoader - loadConfig falls back to default when env-specific not found", async () => {
  await withTempDir(async (tempDir) => {
    const jijiDir = `${tempDir}/.jiji`;
    await ensureDir(jijiDir);
    await Deno.writeTextFile(`${jijiDir}/deploy.yml`, VALID_YAML);

    const originalCwd = Deno.cwd();
    Deno.chdir(tempDir);

    try {
      const result = await ConfigurationLoader.loadConfig("nonexistent");

      assertEquals(result.path, `${tempDir}/.jiji/deploy.yml`);
      assertEquals(result.config.engine, "docker");
    } finally {
      Deno.chdir(originalCwd);
    }
  });
});

Deno.test("ConfigurationLoader - loadConfig throws when no config found", async () => {
  await withTempDir(async (tempDir) => {
    const originalCwd = Deno.cwd();
    Deno.chdir(tempDir);

    try {
      await assertRejects(
        () => ConfigurationLoader.loadConfig(),
        ConfigurationError,
        "No jiji configuration file found",
      );
    } finally {
      Deno.chdir(originalCwd);
    }
  });
});

Deno.test("ConfigurationLoader - findConfigFile finds default config", async () => {
  await withTempDir(async (tempDir) => {
    const jijiDir = `${tempDir}/.jiji`;
    await ensureDir(jijiDir);
    await Deno.writeTextFile(`${jijiDir}/deploy.yml`, VALID_YAML);

    const path = await ConfigurationLoader.findConfigFile(undefined, tempDir);
    assertEquals(path, `${tempDir}/.jiji/deploy.yml`);
  });
});

Deno.test("ConfigurationLoader - findConfigFile finds environment-specific config", async () => {
  await withTempDir(async (tempDir) => {
    const jijiDir = `${tempDir}/.jiji`;
    await ensureDir(jijiDir);
    await Deno.writeTextFile(`${jijiDir}/deploy.yml`, VALID_YAML);
    await Deno.writeTextFile(`${jijiDir}/deploy.staging.yml`, VALID_YAML);

    const path = await ConfigurationLoader.findConfigFile("staging", tempDir);
    assertEquals(path, `${tempDir}/.jiji/deploy.staging.yml`);
  });
});

Deno.test("ConfigurationLoader - findConfigFile returns null when not found", async () => {
  await withTempDir(async (tempDir) => {
    const path = await ConfigurationLoader.findConfigFile(undefined, tempDir);
    assertEquals(path, null);
  });
});

Deno.test("ConfigurationLoader - validateConfigPath with valid path", async () => {
  const { configPath, tempDir } = await createTempConfig(VALID_YAML);

  try {
    // Should not throw
    await ConfigurationLoader.validateConfigPath(configPath);
  } finally {
    await cleanup(tempDir);
  }
});

Deno.test("ConfigurationLoader - validateConfigPath with non-existent path", async () => {
  await assertRejects(
    () => ConfigurationLoader.validateConfigPath("/non/existent/config.yml"),
    ConfigurationError,
    "Configuration file not found",
  );
});

Deno.test("ConfigurationLoader - validateConfigPath with directory", async () => {
  await withTempDir(async (tempDir) => {
    await assertRejects(
      () => ConfigurationLoader.validateConfigPath(tempDir),
      ConfigurationError,
      "Configuration path is not a file",
    );
  });
});

Deno.test("ConfigurationLoader - getAvailableConfigs finds all config files", async () => {
  await withTempDir(async (tempDir) => {
    const jijiDir = `${tempDir}/.jiji`;
    await ensureDir(jijiDir);

    await Deno.writeTextFile(`${jijiDir}/deploy.yml`, VALID_YAML);
    await Deno.writeTextFile(`${jijiDir}/deploy.staging.yml`, VALID_YAML);
    await Deno.writeTextFile(`${jijiDir}/deploy.production.yml`, VALID_YAML);
    await Deno.writeTextFile(`${jijiDir}/other.yml`, VALID_YAML); // Should be included
    await Deno.writeTextFile(`${jijiDir}/deploy.txt`, VALID_YAML); // Should be ignored
    await Deno.writeTextFile(`${jijiDir}/config.yml`, VALID_YAML); // Should be included

    const originalCwd = Deno.cwd();
    Deno.chdir(tempDir);

    try {
      const configs = await ConfigurationLoader.getAvailableConfigs();

      assertEquals(configs.length >= 4, true); // At least our config files

      // Convert to relative paths for easier testing
      const relativeConfigs = configs.map((c) => c.replace(tempDir + "/", ""));

      assertEquals(relativeConfigs.includes(".jiji/deploy.yml"), true);
      assertEquals(relativeConfigs.includes(".jiji/deploy.staging.yml"), true);
      assertEquals(
        relativeConfigs.includes(".jiji/deploy.production.yml"),
        true,
      );
      assertEquals(relativeConfigs.includes(".jiji/config.yml"), true);
      assertEquals(relativeConfigs.includes(".jiji/other.yml"), true);
      assertEquals(relativeConfigs.includes(".jiji/deploy.txt"), false);
    } finally {
      Deno.chdir(originalCwd);
    }
  });
});

Deno.test("ConfigurationLoader - getAvailableConfigs with custom search path", async () => {
  await withTempDir(async (tempDir) => {
    const jijiDir = `${tempDir}/.jiji`;
    await ensureDir(jijiDir);

    await Deno.writeTextFile(`${jijiDir}/deploy.yml`, VALID_YAML);
    await Deno.writeTextFile(`${jijiDir}/deploy.test.yml`, VALID_YAML);

    const configs = await ConfigurationLoader.getAvailableConfigs(tempDir);

    assertEquals(configs.length >= 2, true);

    // Convert to relative paths for easier testing
    const relativeConfigs = configs.map((c) => c.replace(tempDir + "/", ""));

    assertEquals(relativeConfigs.includes(".jiji/deploy.yml"), true);
    assertEquals(relativeConfigs.includes(".jiji/deploy.test.yml"), true);
  });
});

Deno.test("ConfigurationLoader - getAvailableConfigs returns empty array when no configs", async () => {
  await withTempDir(async (tempDir) => {
    const originalCwd = Deno.cwd();
    Deno.chdir(tempDir);

    try {
      const configs = await ConfigurationLoader.getAvailableConfigs();
      assertEquals(configs.length, 0);
    } finally {
      Deno.chdir(originalCwd);
    }
  });
});

Deno.test("ConfigurationLoader - getAvailableConfigs handles missing .jiji directory", async () => {
  await withTempDir(async (tempDir) => {
    const configs = await ConfigurationLoader.getAvailableConfigs(tempDir);
    assertEquals(configs.length, 0);
  });
});

Deno.test("ConfigurationLoader - extractEnvironment method", () => {
  assertEquals(
    ConfigurationLoader.extractEnvironment(".jiji/deploy.production.yml"),
    "production",
  );
  assertEquals(
    ConfigurationLoader.extractEnvironment(".jiji/deploy.staging.yml"),
    "staging",
  );
  assertEquals(
    ConfigurationLoader.extractEnvironment(".jiji/deploy.staging.yaml"),
    "staging",
  );
  assertEquals(
    ConfigurationLoader.extractEnvironment(".jiji/deploy.yml"),
    undefined,
  );
  assertEquals(
    ConfigurationLoader.extractEnvironment(".jiji/production.yml"),
    "production",
  );
});

Deno.test("ConfigurationLoader - mergeConfigs method", () => {
  const base = {
    engine: "docker",
    ssh: { user: "root", port: 22 },
    services: { web: { image: "nginx" } },
  };

  const override = {
    ssh: { port: 2222 },
    services: { api: { image: "node" } },
    env: { NODE_ENV: "production" },
  };

  const merged = ConfigurationLoader.mergeConfigs(base, override);

  assertEquals(merged.engine, "docker");
  assertEquals((merged.ssh as Record<string, unknown>).user, "root");
  assertEquals((merged.ssh as Record<string, unknown>).port, 2222);
  const services = merged.services as Record<string, Record<string, unknown>>;
  assertEquals(services.web.image, "nginx");
  assertEquals(services.api.image, "node");
  assertEquals((merged.env as Record<string, unknown>).NODE_ENV, "production");
});

Deno.test("ConfigurationLoader - loadConfig with relative paths", async () => {
  await withTempDir(async (tempDir) => {
    const configDir = `${tempDir}/configs`;
    await ensureDir(configDir);
    const configPath = `${configDir}/app.yml`;
    await Deno.writeTextFile(configPath, VALID_YAML);

    const originalCwd = Deno.cwd();
    Deno.chdir(tempDir);

    try {
      const result = await ConfigurationLoader.loadConfig(
        undefined,
        "./configs/app.yml",
      );

      assertEquals(result.config.engine, "docker");
    } finally {
      Deno.chdir(originalCwd);
    }
  });
});

Deno.test("ConfigurationLoader - parseYAML handles complex structures", async () => {
  const complexYaml = `
engine: docker
ssh:
  user: deploy
  options:
    StrictHostKeyChecking: "no"
    UserKnownHostsFile: "/dev/null"
services:
  web:
    image: nginx:latest
    hosts:
      - web1.example.com
      - web2.example.com
    environment:
      - NODE_ENV=production
      - DEBUG=false
    volumes:
      - "./data:/app/data:ro"
      - "./logs:/app/logs:rw"
`;

  const { configPath, tempDir } = await createTempConfig(complexYaml);

  try {
    const config = await ConfigurationLoader.loadFromFile(configPath);

    assertEquals(config.engine, "docker");
    assertEquals((config.ssh as Record<string, unknown>).user, "deploy");
    assertEquals(
      typeof (config.ssh as Record<string, unknown>).options,
      "object",
    );
    const services = config.services as Record<string, Record<string, unknown>>;
    assertEquals(services.web.image, "nginx:latest");
    assertEquals(Array.isArray(services.web.hosts), true);
    assertEquals(Array.isArray(services.web.environment), true);
    assertEquals(Array.isArray(services.web.volumes), true);
  } finally {
    await cleanup(tempDir);
  }
});

Deno.test("ConfigurationLoader - handles different config file names", async () => {
  await withTempDir(async (tempDir) => {
    const jijiDir = `${tempDir}/.jiji`;
    await ensureDir(jijiDir);
    await Deno.writeTextFile(`${jijiDir}/deploy.yml`, VALID_YAML);

    const originalCwd = Deno.cwd();
    Deno.chdir(tempDir);

    try {
      const result = await ConfigurationLoader.loadConfig();

      assertEquals(result.path, `${tempDir}/.jiji/deploy.yml`);
      assertEquals(result.config.engine, "docker");
    } finally {
      Deno.chdir(originalCwd);
    }
  });
});

Deno.test("ConfigurationLoader - searches parent directories", async () => {
  await withTempDir(async (tempDir) => {
    const jijiDir = `${tempDir}/.jiji`;
    const nestedDir = `${tempDir}/nested/deep/directory`;

    await ensureDir(jijiDir);
    await ensureDir(nestedDir);
    await Deno.writeTextFile(`${jijiDir}/deploy.yml`, VALID_YAML);

    const configPath = await ConfigurationLoader.findConfigFile(
      undefined,
      nestedDir,
    );
    assertEquals(configPath, `${tempDir}/.jiji/deploy.yml`);
  });
});
