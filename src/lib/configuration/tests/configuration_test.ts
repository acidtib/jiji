import { assertEquals, assertThrows } from "@std/assert";

import { Configuration, ConfigurationError } from "../../configuration.ts";

// Test data
const VALID_CONFIG_DATA = {
  project: "myapp",
  ssh: {
    user: "testuser",
    port: 22,
    connect_timeout: 30,
    command_timeout: 300,
  },
  builder: {
    engine: "docker",
    local: true,
    registry: {
      type: "local",
      port: 5000,
    },
  },
  services: {
    web: {
      image: "nginx:latest",
      servers: [
        { host: "web1.example.com", arch: "amd64" },
        { host: "web2.example.com", arch: "amd64" },
      ],
      ports: ["80:80", "443:443"],
      env: {
        NODE_ENV: "production",
      },
    },
    api: {
      servers: [{ host: "api1.example.com", arch: "amd64" }],
      ports: ["3000:3000"],
      build: {
        dockerfile: "Dockerfile",
        context: ".",
      },
    },
  },
  environment: {
    clear: {
      GLOBAL_VAR: "global_value",
      DATABASE_URL: "postgres://localhost:5432/mydb",
    },
  },
};

const INVALID_CONFIG_DATA = {
  project: "testproject",
  ssh: {
    user: "testuser",
    port: "not_a_number",
  },
  builder: {
    engine: "invalid_engine",
    local: true,
    registry: {
      type: "local",
      port: 5000,
    },
  },
  services: {
    web: {
      // Missing required fields
      servers: [],
    },
  },
};

const MINIMAL_CONFIG_DATA = {
  project: "testproject",
  ssh: {
    user: "root",
  },
  builder: {
    engine: "podman",
    local: true,
    registry: {
      type: "local",
      port: 5000,
    },
  },
  services: {
    simple: {
      image: "alpine:latest",
      servers: [{ host: "localhost", arch: "amd64" }],
    },
  },
};

Deno.test("Configuration - basic construction", () => {
  const config = new Configuration(VALID_CONFIG_DATA);

  assertEquals(config.project, "myapp");
  assertEquals(config.builder.engine, "docker");
  assertEquals(config.ssh.user, "testuser");
  assertEquals(config.ssh.port, 22);
  assertEquals(config.services.size, 2);
  assertEquals(config.services.has("web"), true);
  assertEquals(config.services.has("api"), true);
});

Deno.test("Configuration - construction with path and environment", () => {
  const configPath = "/path/to/config.yml";
  const environment = "staging";
  const config = new Configuration(VALID_CONFIG_DATA, configPath, environment);

  assertEquals(config.configPath, configPath);
  assertEquals(config.environmentName, environment);
});

Deno.test("Configuration - engine property", () => {
  const config = new Configuration({
    project: "test",
    builder: {
      engine: "podman",
    },
    services: {},
  });
  assertEquals(config.builder.engine, "podman");
});

Deno.test("Configuration - engine validation fails for invalid engine", () => {
  const config = new Configuration({
    builder: { engine: "invalid" },
    services: {},
  });

  assertThrows(
    () => config.builder.engine,
    ConfigurationError,
    "Invalid value for 'engine'",
  );
});

Deno.test("Configuration - ssh configuration", () => {
  const config = new Configuration(VALID_CONFIG_DATA);
  const ssh = config.ssh;

  assertEquals(ssh.user, "testuser");
  assertEquals(ssh.port, 22);
  assertEquals(ssh.connectTimeout, 30);
  assertEquals(ssh.commandTimeout, 300);
});

Deno.test("Configuration - ssh configuration with defaults", () => {
  const config = new Configuration({
    project: "test",
    engine: "docker",
    ssh: {
      user: "root",
    },
    services: {},
  });
  const ssh = config.ssh;

  assertEquals(ssh.user, "root");
  assertEquals(ssh.port, 22); // Default port
});

Deno.test("Configuration - services property", () => {
  const config = new Configuration(VALID_CONFIG_DATA);
  const services = config.services;

  assertEquals(services.size, 2);

  const webService = services.get("web");
  assertEquals(webService?.name, "web");
  assertEquals(webService?.image, "nginx:latest");
  assertEquals(webService?.servers.map((s) => s.host), [
    "web1.example.com",
    "web2.example.com",
  ]);

  const apiService = services.get("api");
  assertEquals(apiService?.name, "api");
  assertEquals(
    (apiService?.build as unknown as Record<string, unknown>)?.dockerfile,
    "Dockerfile",
  );
  assertEquals(apiService?.servers.map((s) => s.host), ["api1.example.com"]);
});

Deno.test("Configuration - environment configuration", () => {
  const config = new Configuration(VALID_CONFIG_DATA);
  const env = config.environment;

  assertEquals(env.clear.GLOBAL_VAR, "global_value");
  assertEquals(env.clear.DATABASE_URL, "postgres://localhost:5432/mydb");
});

Deno.test("Configuration - getService method", () => {
  const config = new Configuration(VALID_CONFIG_DATA);

  const webService = config.getService("web");
  assertEquals(webService.name, "web");
  assertEquals(webService.image, "nginx:latest");
});

Deno.test("Configuration - getService throws for non-existent service", () => {
  const config = new Configuration(VALID_CONFIG_DATA);

  assertThrows(
    () => config.getService("nonexistent"),
    ConfigurationError,
    "Service 'nonexistent' not found",
  );
});

Deno.test("Configuration - getServiceNames method", () => {
  const config = new Configuration(VALID_CONFIG_DATA);
  const names = config.getServiceNames();

  assertEquals(names.sort(), ["api", "web"]);
});

Deno.test("Configuration - hasService method", () => {
  const config = new Configuration(VALID_CONFIG_DATA);

  assertEquals(config.hasService("web"), true);
  assertEquals(config.hasService("api"), true);
  assertEquals(config.hasService("nonexistent"), false);
});

Deno.test("Configuration - getServicesForHost method", () => {
  const config = new Configuration(VALID_CONFIG_DATA);

  const webServices = config.getServicesForHost("web1.example.com");
  assertEquals(webServices.length, 1);
  assertEquals(webServices[0].name, "web");

  const apiServices = config.getServicesForHost("api1.example.com");
  assertEquals(apiServices.length, 1);
  assertEquals(apiServices[0].name, "api");

  const noServices = config.getServicesForHost("nonexistent.com");
  assertEquals(noServices.length, 0);
});

Deno.test("Configuration - getAllServerHosts method", () => {
  const config = new Configuration(VALID_CONFIG_DATA);
  const hosts = config.getAllServerHosts();

  assertEquals(hosts.sort(), [
    "api1.example.com",
    "web1.example.com",
    "web2.example.com",
  ]);
});

Deno.test("Configuration - getBuildServices method", () => {
  const config = new Configuration(VALID_CONFIG_DATA);
  const buildServices = config.getBuildServices();

  // Only the api service has build configuration
  assertEquals(buildServices.length, 1);
  assertEquals(buildServices[0].name, "api");
});

Deno.test("Configuration - validation passes for valid config", () => {
  const config = new Configuration(VALID_CONFIG_DATA);
  const result = config.validate();

  assertEquals(result.valid, true);
  assertEquals(result.errors.length, 0);
});

Deno.test("Configuration - validation fails for invalid config", () => {
  const config = new Configuration(INVALID_CONFIG_DATA);
  const result = config.validate();

  assertEquals(result.valid, false);
  assertEquals(result.errors.length > 0, true);
});

Deno.test("Configuration - validation includes service validation", () => {
  const invalidServiceConfig = {
    project: "test",
    engine: "docker",
    services: {
      invalid: {
        // Missing required image and hosts
      },
    },
  };

  const config = new Configuration(invalidServiceConfig);
  const result = config.validate();

  assertEquals(result.valid, false);
  assertEquals(result.errors.some((e) => e.path.includes("services")), true);
});

Deno.test("Configuration - validation checks host consistency", () => {
  const noHostsConfig = {
    project: "test",
    engine: "docker",
    services: {
      web: {
        image: "nginx:latest",
        servers: [], // Empty servers array
      },
    },
  };

  const config = new Configuration(noHostsConfig);
  const result = config.validate();

  assertEquals(result.valid, false);
  assertEquals(result.errors.some((e) => e.code === "NO_SERVERS"), true);
});

Deno.test("Configuration - validation warns about many hosts", () => {
  const manyHostsServices: Record<string, unknown> = {};

  // Create services with many hosts
  for (let i = 1; i <= 15; i++) {
    manyHostsServices[`service${i}`] = {
      image: "nginx:latest",
      servers: [{ host: `host${i}.example.com`, arch: "amd64" }],
    };
  }

  const manyHostsConfig = {
    project: "test",
    engine: "docker",
    services: manyHostsServices,
  };

  const config = new Configuration(manyHostsConfig);
  const result = config.validate();

  assertEquals(result.warnings.some((w) => w.code === "MANY_HOSTS"), true);
});

Deno.test("Configuration - toObject method", () => {
  const config = new Configuration(MINIMAL_CONFIG_DATA);
  const obj = config.toObject();

  assertEquals(obj.project, "testproject");
  assertEquals(typeof obj.services, "object");
  assertEquals(typeof obj.ssh, "object");
  assertEquals((obj.ssh as Record<string, unknown>).user, "root");
  assertEquals(typeof obj.builder, "object");
  assertEquals((obj.builder as Record<string, unknown>).engine, "podman");
});

Deno.test("Configuration - withDefaults static method", () => {
  const config = Configuration.withDefaults();

  assertEquals(config.project, "default");
  assertEquals(config.builder.engine, "podman");
  assertEquals(config.ssh.user, "root");
  assertEquals(config.ssh.port, 22);
  assertEquals(config.services.size, 1);
  assertEquals(config.services.has("web"), true);
});

Deno.test("Configuration - withDefaults overrides", () => {
  const config = Configuration.withDefaults({
    project: "override-project",
    builder: { engine: "docker" },
    ssh: { user: "deploy" },
  });

  assertEquals(config.project, "override-project");
  assertEquals(config.builder.engine, "docker");
  assertEquals(config.ssh.user, "deploy");
});

Deno.test("Configuration - lazy loading of properties", () => {
  const config = new Configuration(VALID_CONFIG_DATA);

  // Access properties multiple times to ensure they're cached properly
  assertEquals(config.project, "myapp");
  assertEquals(config.project, "myapp");
  assertEquals(config.builder.engine, "docker");
  assertEquals(config.builder.engine, "docker");

  const ssh1 = config.ssh;
  const ssh2 = config.ssh;
  assertEquals(ssh1, ssh2); // Should be the same instance

  const services1 = config.services;
  const services2 = config.services;
  assertEquals(services1, services2); // Should be the same instance
});

Deno.test("Configuration - handles missing optional sections", () => {
  const minimalConfig = {
    project: "test",
    engine: "docker",
    ssh: {
      user: "root",
    },
    services: {
      web: {
        image: "nginx:latest",
        servers: [{ host: "localhost", arch: "amd64" }],
      },
    },
  };

  const config = new Configuration(minimalConfig);

  // SSH should use defaults
  assertEquals(config.ssh.user, "root");
  assertEquals(config.ssh.port, 22);

  // Environment should be empty but functional
  assertEquals(Object.keys(config.environment.clear).length, 0);
  assertEquals(config.environment.secrets.length, 0);
});

Deno.test("Configuration - missing required engine throws", () => {
  const config = new Configuration({ project: "test" });

  assertThrows(
    () => config.builder,
    ConfigurationError,
    "Missing required configuration: 'builder'",
  );
});

Deno.test("Configuration - missing required services throws", () => {
  const config = new Configuration({ project: "test", engine: "docker" });

  assertThrows(
    () => config.services,
    ConfigurationError,
    "Missing required configuration: 'services'",
  );
});

Deno.test("Configuration - invalid services type throws", () => {
  const config = new Configuration({
    project: "test",
    engine: "docker",
    services: "not an object",
  });

  assertThrows(
    () => config.services,
    ConfigurationError,
    "'services' must be an object",
  );
});

Deno.test("Configuration - invalid service configuration throws", () => {
  const config = new Configuration({
    engine: "docker",
    services: {
      web: "not an object",
    },
  });

  assertThrows(
    () => config.services,
    ConfigurationError,
    "'services.web' must be an object",
  );
});
