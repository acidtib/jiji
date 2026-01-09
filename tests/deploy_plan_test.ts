import { assertEquals } from "@std/assert";
import { Configuration } from "../src/lib/configuration.ts";

// Test configuration with multiple services for deployment plan testing
const TEST_CONFIG_DATA = {
  project: "test-deployment",
  ssh: {
    user: "deploy",
    port: 22,
  },
  builder: {
    engine: "docker",
    local: true,
    registry: {
      type: "local",
      port: 9270,
    },
  },
  servers: {
    web1: {
      host: "192.168.1.10",
      arch: "amd64",
    },
    web2: {
      host: "192.168.1.11",
      arch: "amd64",
    },
    api1: {
      host: "192.168.1.12",
      arch: "amd64",
    },
    db1: {
      host: "192.168.1.13",
      arch: "amd64",
    },
  },
  services: {
    web: {
      image: "nginx:latest",
      hosts: ["web1", "web2"],
      ports: ["80:80", "443:443"],
      proxy: {
        app_port: 80,
        host: "example.com",
        ssl: true,
      },
    },
    api: {
      build: {
        context: "./api",
        dockerfile: "Dockerfile.prod",
        target: "production",
      },
      hosts: ["api1"],
      ports: ["3000:3000"],
      environment: {
        NODE_ENV: "production",
      },
    },
    database: {
      image: "postgres:15-alpine",
      hosts: ["db1"],
      ports: ["5432:5432"],
      volumes: ["db_data:/var/lib/postgresql/data"],
    },
  },
};

Deno.test("Configuration - deployment plan data collection", () => {
  const config = new Configuration(TEST_CONFIG_DATA);

  // Test that we can get deployable services
  const deployableServices = config.getDeployableServices();
  assertEquals(deployableServices.length, 3);

  // Test that we can get build services
  const buildServices = config.getBuildServices();
  assertEquals(buildServices.length, 1);
  assertEquals(buildServices[0].name, "api");

  // Test service details for deployment plan
  const webService = deployableServices.find((s) => s.name === "web");
  const apiService = deployableServices.find((s) => s.name === "api");
  const dbService = deployableServices.find((s) => s.name === "database");

  // Verify web service details
  assertEquals(webService?.image, "nginx:latest");
  assertEquals(webService?.hosts.length, 2);
  assertEquals(webService?.hosts, ["web1", "web2"]);
  assertEquals(webService?.ports, ["80:80", "443:443"]);
  assertEquals(webService?.proxy?.enabled, true);

  // Verify api service details
  assertEquals(typeof apiService?.build, "object");
  const apiBuild = apiService?.build as {
    context: string;
    dockerfile: string;
    target: string;
  };
  assertEquals(apiBuild?.context, "./api");
  assertEquals(apiBuild?.dockerfile, "Dockerfile.prod");
  assertEquals(apiBuild?.target, "production");
  assertEquals(apiService?.hosts.length, 1);
  assertEquals(apiService?.hosts, ["api1"]);

  // Verify database service details
  assertEquals(dbService?.image, "postgres:15-alpine");
  assertEquals(dbService?.volumes, ["db_data:/var/lib/postgresql/data"]);
});

Deno.test("Configuration - deployment plan display format validation", () => {
  const config = new Configuration(TEST_CONFIG_DATA);

  // Test basic configuration properties that would be displayed
  assertEquals(config.project, "test-deployment");
  assertEquals(config.builder.engine, "docker");
  assertEquals(config.builder.registry.getRegistryUrl(), "localhost:9270");

  const services = config.getDeployableServices();

  // Verify each service has the required properties for plan display
  services.forEach((service) => {
    // Every service should have a name
    assertEquals(typeof service.name, "string");

    // Every service should have either image or build
    const hasImage = service.image !== undefined;
    const hasBuild = service.build !== undefined;
    assertEquals(hasImage || hasBuild, true);

    // Hosts array should exist (can be empty)
    assertEquals(Array.isArray(service.hosts), true);

    // Ports array should exist (can be empty)
    assertEquals(Array.isArray(service.ports), true);
  });
});

Deno.test("Configuration - service filtering for deployment plan", async () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const { filterServicesByPatterns } = await import("../src/utils/config.ts");

  const allServices = config.getDeployableServices();
  assertEquals(allServices.length, 3);

  // Test filtering by specific service name
  const webOnly = filterServicesByPatterns(allServices, "web", config);
  assertEquals(webOnly.length, 1);
  assertEquals(webOnly[0].name, "web");

  // Test filtering by pattern
  const dbOnly = filterServicesByPatterns(allServices, "data*", config);
  assertEquals(dbOnly.length, 1);
  assertEquals(dbOnly[0].name, "database");

  // Test filtering multiple services
  const webAndApi = filterServicesByPatterns(
    allServices,
    "web,api",
    config,
  );
  assertEquals(webAndApi.length, 2);
  assertEquals(webAndApi.map((s) => s.name).sort(), ["api", "web"]);
});

Deno.test("Configuration - proxy configuration for deployment plan", () => {
  const config = new Configuration(TEST_CONFIG_DATA);
  const services = config.getDeployableServices();

  const webService = services.find((s) => s.name === "web");
  assertEquals(webService?.proxy?.enabled, true);
  assertEquals(webService?.proxy?.targets[0].host, "example.com");
  assertEquals(webService?.proxy?.targets[0].ssl, true);

  const apiService = services.find((s) => s.name === "api");
  assertEquals(apiService?.proxy?.enabled || false, false);
});
