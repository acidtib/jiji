import { assertEquals, assertThrows } from "@std/assert";

import {
  Configuration,
  ConfigurationError,
} from "../../../lib/configuration.ts";

// Test data
const VALID_CONFIG_DATA = {
  engine: "docker" as const,
  ssh: {
    user: "testuser",
    port: 22,
    connect_timeout: 30,
    command_timeout: 300,
  },
  services: {
    web: {
      image: "nginx:latest",
      hosts: ["web1.example.com", "web2.example.com"],
      ports: ["80:80", "443:443"],
      env: {
        NODE_ENV: "production",
      },
    },
    api: {
      hosts: ["api1.example.com"],
      ports: ["3000:3000"],
      build: {
        dockerfile: "Dockerfile",
        context: ".",
      },
    },
  },
  env: {
    variables: {
      GLOBAL_VAR: "global_value",
      DATABASE_URL: "postgres://localhost:5432/mydb",
    },
  },
};

const INVALID_CONFIG_DATA = {
  engine: "invalid_engine",
  ssh: {
    user: "testuser",
    port: "not_a_number",
  },
  services: {
    web: {
      // Missing required fields
      hosts: [],
    },
  },
};

const MINIMAL_CONFIG_DATA = {
  engine: "podman" as const,
  ssh: {
    user: "root",
  },
  services: {
    simple: {
      image: "alpine:latest",
      hosts: ["localhost"],
    },
  },
};

Deno.test("Configuration - basic construction", () => {
  const config = new Configuration(VALID_CONFIG_DATA);

  assertEquals(config.engine, "docker");
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
  const config = new Configuration({ engine: "podman", services: {} });
  assertEquals(config.engine, "podman");
});

Deno.test("Configuration - engine validation fails for invalid engine", () => {
  const config = new Configuration({ engine: "invalid", services: {} });

  assertThrows(
    () => config.engine,
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
    engine: "docker",
    ssh: {
      user: "root",
    },
    services: {},
  });
  const ssh = config.ssh;

  // Should use defaults from SSHConfiguration
  assertEquals(ssh.user, "root");
  assertEquals(ssh.port, 22);
});

Deno.test("Configuration - services property", () => {
  const config = new Configuration(VALID_CONFIG_DATA);
  const services = config.services;

  assertEquals(services.size, 2);

  const webService = services.get("web");
  assertEquals(webService?.name, "web");
  assertEquals(webService?.image, "nginx:latest");
  assertEquals(webService?.hosts, ["web1.example.com", "web2.example.com"]);

  const apiService = services.get("api");
  assertEquals(apiService?.name, "api");
  assertEquals(
    (apiService?.build as unknown as Record<string, unknown>)?.dockerfile,
    "Dockerfile",
  );
  assertEquals(apiService?.hosts, ["api1.example.com"]);
});

Deno.test("Configuration - environment configuration", () => {
  const config = new Configuration(VALID_CONFIG_DATA, undefined, "production");
  const env = config.environment;

  assertEquals(env.name, "production");
  assertEquals(env.variables.GLOBAL_VAR, "global_value");
  assertEquals(env.variables.DATABASE_URL, "postgres://localhost:5432/mydb");
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

Deno.test("Configuration - getAllHosts method", () => {
  const config = new Configuration(VALID_CONFIG_DATA);
  const hosts = config.getAllHosts();

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
    engine: "docker",
    services: {
      web: {
        image: "nginx:latest",
        hosts: [], // Empty hosts array
      },
    },
  };

  const config = new Configuration(noHostsConfig);
  const result = config.validate();

  assertEquals(result.valid, false);
  assertEquals(result.errors.some((e) => e.code === "NO_HOSTS"), true);
});

Deno.test("Configuration - validation warns about many hosts", () => {
  const manyHostsServices: Record<string, unknown> = {};

  // Create services with many hosts
  for (let i = 1; i <= 15; i++) {
    manyHostsServices[`service${i}`] = {
      image: "nginx:latest",
      hosts: [`host${i}.example.com`],
    };
  }

  const manyHostsConfig = {
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

  assertEquals(obj.engine, "podman");
  assertEquals(typeof obj.services, "object");
  assertEquals(typeof obj.ssh, "object");
  assertEquals((obj.ssh as Record<string, unknown>).user, "root");
});

Deno.test("Configuration - withDefaults static method", () => {
  const config = Configuration.withDefaults();

  assertEquals(config.engine, "podman");
  assertEquals(config.ssh.user, "root");
  assertEquals(config.ssh.port, 22);
  assertEquals(config.services.size, 1);
  assertEquals(config.services.has("web"), true);
});

Deno.test("Configuration - withDefaults accepts overrides", () => {
  const config = Configuration.withDefaults({
    engine: "docker",
    ssh: { user: "deploy" },
  });

  assertEquals(config.engine, "docker");
  assertEquals(config.ssh.user, "deploy");
});

Deno.test("Configuration - lazy loading of properties", () => {
  const config = new Configuration(VALID_CONFIG_DATA);

  // Access properties multiple times to ensure they're cached properly
  assertEquals(config.engine, "docker");
  assertEquals(config.engine, "docker");

  const ssh1 = config.ssh;
  const ssh2 = config.ssh;
  assertEquals(ssh1, ssh2); // Should be the same instance

  const services1 = config.services;
  const services2 = config.services;
  assertEquals(services1, services2); // Should be the same instance
});

Deno.test("Configuration - handles missing optional sections", () => {
  const minimalConfig = {
    engine: "docker",
    ssh: {
      user: "root",
    },
    services: {
      web: {
        image: "nginx:latest",
        hosts: ["localhost"],
      },
    },
  };

  const config = new Configuration(minimalConfig);

  // SSH should use defaults
  assertEquals(config.ssh.user, "root");
  assertEquals(config.ssh.port, 22);

  // Environment should be empty but functional
  assertEquals(config.environment.name, "default");
  assertEquals(config.environment.variables.NONEXISTENT, undefined);
});

Deno.test("Configuration - environment name defaults", () => {
  const config = new Configuration(MINIMAL_CONFIG_DATA);
  assertEquals(config.environmentName, undefined);
  assertEquals(config.environment.name, "default");
});

Deno.test("Configuration - environment name from constructor", () => {
  const config = new Configuration(MINIMAL_CONFIG_DATA, undefined, "test");
  assertEquals(config.environmentName, "test");
  assertEquals(config.environment.name, "test");
});

Deno.test("Configuration - missing required engine throws", () => {
  const config = new Configuration({ services: {} });

  assertThrows(
    () => config.engine,
    ConfigurationError,
    "Missing required configuration: 'engine'",
  );
});

Deno.test("Configuration - missing required services throws", () => {
  const config = new Configuration({ engine: "docker" });

  assertThrows(
    () => config.services,
    ConfigurationError,
    "Missing required configuration: 'services'",
  );
});

Deno.test("Configuration - invalid services structure throws", () => {
  const config = new Configuration({
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
