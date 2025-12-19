import { assertEquals, assertExists } from "@std/assert";
import { Configuration } from "../src/lib/configuration.ts";

// Test configuration with multiple services including dependencies
const TEST_CONFIG_DATA = {
  project: "test-app",
  ssh: {
    user: "deploy",
    port: 22,
  },
  builder: {
    engine: "docker",
    local: true,
    registry: {
      type: "local",
      port: 6767,
    },
  },
  services: {
    api: {
      build: {
        context: "./api",
        dockerfile: "Dockerfile",
      },
      servers: [{ host: "192.168.1.87" }],
      ports: ["3000:3000"],
      environment: {
        NODE_ENV: "production",
        DATABASE_URL: "postgres://database:5432/testapp",
      },
    },
    database: {
      image: "postgres:15-alpine",
      servers: [{ host: "192.168.1.87" }],
      ports: ["5432:5432"],
      environment: {
        POSTGRES_DB: "testapp",
        POSTGRES_USER: "testuser",
        POSTGRES_PASSWORD: "testpass",
      },
    },
    worker: {
      build: {
        context: "./worker",
        dockerfile: "Dockerfile",
      },
      servers: [{ host: "192.168.1.88" }],
      environment: {
        NODE_ENV: "production",
        REDIS_URL: "redis://cache:6379",
      },
    },
    cache: {
      image: "redis:7-alpine",
      servers: [{ host: "192.168.1.88" }],
      ports: ["6379:6379"],
    },
  },
};

Deno.test("Deploy Service Filtering - getMatchingServiceNames works with single service", () => {
  const config = new Configuration(TEST_CONFIG_DATA);

  const matchingNames = config.getMatchingServiceNames(["api"]);
  assertEquals(matchingNames, ["api"]);
});

Deno.test("Deploy Service Filtering - getMatchingServiceNames works with multiple services", () => {
  const config = new Configuration(TEST_CONFIG_DATA);

  const matchingNames = config.getMatchingServiceNames(["api", "database"]);
  assertEquals(matchingNames.sort(), ["api", "database"]);
});

Deno.test("Deploy Service Filtering - getMatchingServiceNames works with patterns", () => {
  const config = new Configuration(TEST_CONFIG_DATA);

  const matchingNames = config.getMatchingServiceNames(["*api*"]);
  assertEquals(matchingNames, ["api"]);
});

Deno.test("Deploy Service Filtering - getMatchingServiceNames works with wildcards", () => {
  const config = new Configuration(TEST_CONFIG_DATA);

  const matchingNames = config.getMatchingServiceNames(["cache", "work*"]);
  assertEquals(matchingNames.sort(), ["cache", "worker"]);
});

Deno.test("Deploy Service Filtering - getBuildServices returns only buildable services", () => {
  const config = new Configuration(TEST_CONFIG_DATA);

  const buildServices = config.getBuildServices();
  const buildServiceNames = buildServices.map((s) => s.name).sort();
  assertEquals(buildServiceNames, ["api", "worker"]);
});

Deno.test("Deploy Service Filtering - getDeployableServices returns all services", () => {
  const config = new Configuration(TEST_CONFIG_DATA);

  const deployableServices = config.getDeployableServices();
  const deployableServiceNames = deployableServices.map((s) => s.name).sort();
  assertEquals(deployableServiceNames, ["api", "cache", "database", "worker"]);
});

Deno.test("Deploy Service Filtering - filtered deployable services work correctly", () => {
  const config = new Configuration(TEST_CONFIG_DATA);

  // Get all deployable services
  const allServices = config.getDeployableServices();
  assertEquals(allServices.length, 4);

  // Filter by pattern (simulating -S api)
  const servicePatterns = ["api"];
  const matchingNames = config.getMatchingServiceNames(servicePatterns);
  const filteredServices = allServices.filter((service) =>
    matchingNames.includes(service.name)
  );

  assertEquals(filteredServices.length, 1);
  assertEquals(filteredServices[0].name, "api");
});

Deno.test("Deploy Service Filtering - multiple service patterns work correctly", () => {
  const config = new Configuration(TEST_CONFIG_DATA);

  // Get all deployable services
  const allServices = config.getDeployableServices();

  // Filter by multiple patterns (simulating -S api,cache)
  const servicePatterns = ["api", "cache"];
  const matchingNames = config.getMatchingServiceNames(servicePatterns);
  const filteredServices = allServices.filter((service) =>
    matchingNames.includes(service.name)
  );

  assertEquals(filteredServices.length, 2);
  const filteredNames = filteredServices.map((s) => s.name).sort();
  assertEquals(filteredNames, ["api", "cache"]);
});

Deno.test("Deploy Service Filtering - wildcard patterns work correctly", () => {
  const config = new Configuration(TEST_CONFIG_DATA);

  // Get all deployable services
  const allServices = config.getDeployableServices();

  // Filter by wildcard pattern (simulating -S *a*)
  const servicePatterns = ["*a*"];
  const matchingNames = config.getMatchingServiceNames(servicePatterns);
  const filteredServices = allServices.filter((service) =>
    matchingNames.includes(service.name)
  );

  assertEquals(filteredServices.length, 3); // api, database, cache
  const filteredNames = filteredServices.map((s) => s.name).sort();
  assertEquals(filteredNames, ["api", "cache", "database"]);
});

Deno.test("Deploy Service Filtering - configuration has expected structure", () => {
  const config = new Configuration(TEST_CONFIG_DATA);

  assertEquals(config.project, "test-app");
  assertExists(config.services.get("api"));
  assertExists(config.services.get("database"));
  assertExists(config.services.get("worker"));
  assertExists(config.services.get("cache"));

  // Verify api service configuration
  const apiService = config.services.get("api")!;
  assertEquals(apiService.name, "api");
  assertEquals(apiService.servers.length, 1);
  assertEquals(apiService.servers[0].host, "192.168.1.87");
  assertEquals(apiService.requiresBuild(), true);

  // Verify database service configuration
  const dbService = config.services.get("database")!;
  assertEquals(dbService.name, "database");
  assertEquals(dbService.servers.length, 1);
  assertEquals(dbService.servers[0].host, "192.168.1.87");
  assertEquals(dbService.requiresBuild(), false);
});
