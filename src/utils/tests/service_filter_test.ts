import { assert, assertEquals } from "@std/assert";
import { ServiceFilter, type ServiceFilterOptions } from "../service_filter.ts";
import { ServiceConfiguration } from "../../lib/configuration/service.ts";

// Mock service configuration for testing
function createMockService(
  name: string,
  config: Record<string, unknown>,
  project = "test",
): ServiceConfiguration {
  return new ServiceConfiguration(name, config, project);
}

Deno.test("ServiceFilter - basic service filtering", () => {
  const services = new Map([
    [
      "web",
      createMockService("web", {
        image: "nginx:latest",
        hosts: ["host1", "host2"],
      }),
    ],
    [
      "api",
      createMockService("api", {
        image: "node:18",
        hosts: ["host2", "host3"],
      }),
    ],
    [
      "worker",
      createMockService("worker", {
        build: { context: "./worker" },
        hosts: ["host3"],
      }),
    ],
  ]);

  // Filter by service names
  const filtered1 = ServiceFilter.filter(services, {
    services: ["web", "api"],
  });
  assertEquals(filtered1.size, 2);
  assert(filtered1.has("web"));
  assert(filtered1.has("api"));
  assert(!filtered1.has("worker"));

  // Filter by exclusion
  const filtered2 = ServiceFilter.filter(services, {
    exclude: ["worker"],
  });
  assertEquals(filtered2.size, 2);
  assert(filtered2.has("web"));
  assert(filtered2.has("api"));
  assert(!filtered2.has("worker"));

  // No filters - return all
  const filtered3 = ServiceFilter.filter(services, {});
  assertEquals(filtered3.size, 3);
});

Deno.test("ServiceFilter - pattern matching", () => {
  const services = new Map([
    [
      "web-frontend",
      createMockService("web-frontend", {
        image: "nginx:latest",
        hosts: ["host1"],
      }),
    ],
    [
      "web-backend",
      createMockService("web-backend", {
        image: "node:18",
        hosts: ["host2"],
      }),
    ],
    [
      "database",
      createMockService("database", {
        image: "postgres:15",
        hosts: ["host3"],
      }),
    ],
    [
      "cache",
      createMockService("cache", {
        image: "redis:7",
        hosts: ["host4"],
      }),
    ],
  ]);

  // Pattern matching with wildcard
  const filtered1 = ServiceFilter.filter(services, {
    patterns: ["web-*"],
  });
  assertEquals(filtered1.size, 2);
  assert(filtered1.has("web-frontend"));
  assert(filtered1.has("web-backend"));
  assert(!filtered1.has("database"));
  assert(!filtered1.has("cache"));

  // Pattern matching with single character wildcard
  const filtered2 = ServiceFilter.filter(services, {
    patterns: ["cach?"],
  });
  assertEquals(filtered2.size, 1);
  assert(filtered2.has("cache"));

  // Multiple patterns
  const filtered3 = ServiceFilter.filter(services, {
    patterns: ["web-*", "database"],
  });
  assertEquals(filtered3.size, 3);
  assert(filtered3.has("web-frontend"));
  assert(filtered3.has("web-backend"));
  assert(filtered3.has("database"));
  assert(!filtered3.has("cache"));
});

Deno.test("ServiceFilter - build vs image filtering", () => {
  const services = new Map([
    [
      "pre-built",
      createMockService("pre-built", {
        image: "nginx:latest",
        hosts: ["host1"],
      }),
    ],
    [
      "custom-build",
      createMockService("custom-build", {
        build: { context: "./app" },
        hosts: ["host2"],
      }),
    ],
    [
      "another-image",
      createMockService("another-image", {
        image: "redis:7",
        hosts: ["host3"],
      }),
    ],
  ]);

  // Build-only filter
  const buildOnly = ServiceFilter.filter(services, {
    buildOnly: true,
  });
  assertEquals(buildOnly.size, 1);
  assert(buildOnly.has("custom-build"));
  assert(!buildOnly.has("pre-built"));
  assert(!buildOnly.has("another-image"));

  // Image-only filter
  const imageOnly = ServiceFilter.filter(services, {
    imageOnly: true,
  });
  assertEquals(imageOnly.size, 2);
  assert(!imageOnly.has("custom-build"));
  assert(imageOnly.has("pre-built"));
  assert(imageOnly.has("another-image"));
});

Deno.test("ServiceFilter - host-based filtering", () => {
  const services = new Map([
    [
      "service1",
      createMockService("service1", {
        image: "nginx:latest",
        hosts: ["host1", "host2"],
      }),
    ],
    [
      "service2",
      createMockService("service2", {
        image: "node:18",
        hosts: ["host2", "host3"],
      }),
    ],
    [
      "service3",
      createMockService("service3", {
        image: "redis:7",
        hosts: ["host4"],
      }),
    ],
  ]);

  // Filter by specific hosts
  const filtered1 = ServiceFilter.filter(services, {
    hosts: ["host1"],
  });
  assertEquals(filtered1.size, 1);
  assert(filtered1.has("service1"));

  // Filter by multiple hosts (services that target any of these hosts)
  const filtered2 = ServiceFilter.filter(services, {
    hosts: ["host2", "host4"],
  });
  assertEquals(filtered2.size, 3); // All services have at least one matching host
  assert(filtered2.has("service1")); // has host2
  assert(filtered2.has("service2")); // has host2
  assert(filtered2.has("service3")); // has host4

  // Filter by non-existent host
  const filtered3 = ServiceFilter.filter(services, {
    hosts: ["non-existent"],
  });
  assertEquals(filtered3.size, 0);
});

Deno.test("ServiceFilter - exclusion takes precedence", () => {
  const services = new Map([
    [
      "web",
      createMockService("web", {
        image: "nginx:latest",
        hosts: ["host1"],
      }),
    ],
    [
      "api",
      createMockService("api", {
        image: "node:18",
        hosts: ["host2"],
      }),
    ],
  ]);

  // Include web but exclude it - exclusion wins
  const filtered = ServiceFilter.filter(services, {
    services: ["web", "api"],
    exclude: ["web"],
  });
  assertEquals(filtered.size, 1);
  assert(!filtered.has("web"));
  assert(filtered.has("api"));
});

Deno.test("ServiceFilter - complex filtering", () => {
  const services = new Map([
    [
      "web-frontend",
      createMockService("web-frontend", {
        image: "nginx:latest",
        hosts: ["host1", "host2"],
      }),
    ],
    [
      "web-api",
      createMockService("web-api", {
        build: { context: "./api" },
        hosts: ["host2", "host3"],
      }),
    ],
    [
      "worker-queue",
      createMockService("worker-queue", {
        build: { context: "./worker" },
        hosts: ["host3"],
      }),
    ],
    [
      "database",
      createMockService("database", {
        image: "postgres:15",
        hosts: ["host4"],
      }),
    ],
  ]);

  // Complex filter: web services that require building and target host2 or host3
  const filtered = ServiceFilter.filter(services, {
    patterns: ["web-*"],
    buildOnly: true,
    hosts: ["host2", "host3"],
  });

  assertEquals(filtered.size, 1);
  assert(filtered.has("web-api")); // matches pattern, is build-only, targets host2/host3
  assert(!filtered.has("web-frontend")); // matches pattern and hosts, but not build-only
  assert(!filtered.has("worker-queue")); // is build-only and targets host3, but doesn't match pattern
  assert(!filtered.has("database")); // doesn't match any criteria
});

Deno.test("ServiceFilter - service grouping by concurrency", () => {
  const services = new Map([
    [
      "service1",
      createMockService("service1", { image: "nginx", hosts: ["host1"] }),
    ],
    [
      "service2",
      createMockService("service2", { image: "nginx", hosts: ["host2"] }),
    ],
    [
      "service3",
      createMockService("service3", { image: "nginx", hosts: ["host3"] }),
    ],
    [
      "service4",
      createMockService("service4", { image: "nginx", hosts: ["host4"] }),
    ],
    [
      "service5",
      createMockService("service5", { image: "nginx", hosts: ["host5"] }),
    ],
  ]);

  const groups = ServiceFilter.group(services, {
    maxConcurrent: 2,
  });

  assertEquals(groups.length, 3);
  assertEquals(groups[0].length, 2);
  assertEquals(groups[1].length, 2);
  assertEquals(groups[2].length, 1);

  // Verify all services are included
  const allServicesFromGroups = groups.flat();
  assertEquals(allServicesFromGroups.length, 5);
});

Deno.test("ServiceFilter - service grouping by hosts", () => {
  const services = new Map([
    ["web1", createMockService("web1", { image: "nginx", hosts: ["host1"] })],
    ["web2", createMockService("web2", { image: "nginx", hosts: ["host1"] })],
    ["api1", createMockService("api1", { image: "node", hosts: ["host2"] })],
    ["api2", createMockService("api2", { image: "node", hosts: ["host2"] })],
    ["db", createMockService("db", { image: "postgres", hosts: ["host3"] })],
  ]);

  const groups = ServiceFilter.group(services, {
    groupByHosts: true,
    maxConcurrent: 2,
  });

  // Should group by primary host, then batch by maxConcurrent
  assertEquals(groups.length, 2); // ceil(3 host groups / 2)

  // All services should be included
  const allServicesFromGroups = groups.flat();
  assertEquals(allServicesFromGroups.length, 5);
});

Deno.test("ServiceFilter - dependency-based grouping", () => {
  const services = new Map([
    [
      "database",
      createMockService("database", { image: "postgres", hosts: ["host1"] }),
    ],
    ["api", createMockService("api", { image: "node", hosts: ["host2"] })],
    ["web", createMockService("web", { image: "nginx", hosts: ["host3"] })],
    [
      "worker",
      createMockService("worker", { image: "worker", hosts: ["host4"] }),
    ],
  ]);

  const dependencies = {
    "api": ["database"],
    "web": ["api"],
    "worker": ["database"],
  };

  const groups = ServiceFilter.group(services, {
    dependencies,
  });

  // Database should be in first group (no dependencies)
  assertEquals(groups[0].length, 1);
  assertEquals(groups[0][0].name, "database");

  // API and worker should be in second group (depend on database)
  assertEquals(groups[1].length, 2);
  const secondGroupNames = groups[1].map((s) => s.name).sort();
  assertEquals(secondGroupNames, ["api", "worker"]);

  // Web should be in third group (depends on api)
  assertEquals(groups[2].length, 1);
  assertEquals(groups[2][0].name, "web");
});

Deno.test("ServiceFilter - utility functions", () => {
  const services = new Map([
    [
      "web",
      createMockService("web", {
        image: "nginx:latest",
        hosts: ["host1", "host2"],
      }),
    ],
    [
      "api",
      createMockService("api", {
        image: "node:18",
        hosts: ["host2", "host3"],
      }),
    ],
  ]);

  // Get unique hosts
  const uniqueHosts = ServiceFilter.getUniqueHosts(services);
  assertEquals(uniqueHosts.sort(), ["host1", "host2", "host3"]);

  // Get services for specific host
  const servicesForHost2 = ServiceFilter.getServicesForHost(services, "host2");
  assertEquals(servicesForHost2.length, 2);
  assertEquals(servicesForHost2.map((s) => s.name).sort(), ["api", "web"]);

  const servicesForHost1 = ServiceFilter.getServicesForHost(services, "host1");
  assertEquals(servicesForHost1.length, 1);
  assertEquals(servicesForHost1[0].name, "web");
});

Deno.test("ServiceFilter - filter summary", () => {
  const allServices = new Map([
    ["web", createMockService("web", { image: "nginx", hosts: ["host1"] })],
    ["api", createMockService("api", { image: "node", hosts: ["host2"] })],
    [
      "worker",
      createMockService("worker", {
        build: { context: "./worker" },
        hosts: ["host3"],
      }),
    ],
  ]);

  const filteredServices = new Map([
    ["web", allServices.get("web")!],
    ["api", allServices.get("api")!],
  ]);

  const filterOptions: ServiceFilterOptions = {
    services: ["web", "api"],
    exclude: ["worker"],
    imageOnly: true,
  };

  const summary = ServiceFilter.createFilterSummary(
    allServices,
    filteredServices,
    filterOptions,
  );

  assert(summary.includes("Total services: 3"));
  assert(summary.includes("Selected services: 2"));
  assert(summary.includes("Services: web, api"));
  assert(summary.includes("Filter by names: web, api"));
  assert(summary.includes("Excluded: worker"));
  assert(summary.includes("Filter: image-only services"));
});

Deno.test("ServiceFilter - validation", () => {
  // Valid options should return no errors
  const validOptions: ServiceFilterOptions = {
    services: ["web", "api"],
    patterns: ["web-*"],
    exclude: ["old-service"],
    buildOnly: false,
    imageOnly: false,
    hosts: ["host1", "host2"],
  };

  const validErrors = ServiceFilter.validateFilterOptions(validOptions);
  assertEquals(validErrors.length, 0);

  // Invalid: both buildOnly and imageOnly
  const invalidOptions1: ServiceFilterOptions = {
    buildOnly: true,
    imageOnly: true,
  };
  const errors1 = ServiceFilter.validateFilterOptions(invalidOptions1);
  assertEquals(errors1.length, 1);
  assert(errors1[0].includes("Cannot specify both buildOnly and imageOnly"));

  // Invalid: empty service names
  const invalidOptions2: ServiceFilterOptions = {
    services: ["web", "", "api"],
  };
  const errors2 = ServiceFilter.validateFilterOptions(invalidOptions2);
  assertEquals(errors2.length, 1);
  assert(errors2[0].includes("Service names cannot be empty"));

  // Invalid: empty patterns
  const invalidOptions3: ServiceFilterOptions = {
    patterns: ["web-*", "  ", "api-*"],
  };
  const errors3 = ServiceFilter.validateFilterOptions(invalidOptions3);
  assertEquals(errors3.length, 1);
  assert(errors3[0].includes("Service patterns cannot be empty"));

  // Invalid: empty exclusions
  const invalidOptions4: ServiceFilterOptions = {
    exclude: ["old-service", ""],
  };
  const errors4 = ServiceFilter.validateFilterOptions(invalidOptions4);
  assertEquals(errors4.length, 1);
  assert(errors4[0].includes("Excluded service names cannot be empty"));

  // Invalid: empty hosts
  const invalidOptions5: ServiceFilterOptions = {
    hosts: ["host1", "", "host2"],
  };
  const errors5 = ServiceFilter.validateFilterOptions(invalidOptions5);
  assertEquals(errors5.length, 1);
  assert(errors5[0].includes("Host names cannot be empty"));
});

Deno.test("ServiceFilter - edge cases", () => {
  // Empty services map
  const emptyServices = new Map<string, ServiceConfiguration>();
  const filtered1 = ServiceFilter.filter(emptyServices, {
    services: ["web"],
  });
  assertEquals(filtered1.size, 0);

  // Filter with no matching services
  const services = new Map([
    ["web", createMockService("web", { image: "nginx", hosts: ["host1"] })],
  ]);

  const filtered2 = ServiceFilter.filter(services, {
    services: ["nonexistent"],
  });
  assertEquals(filtered2.size, 0);

  // Grouping with empty services
  const groups1 = ServiceFilter.group(emptyServices, {
    maxConcurrent: 5,
  });
  assertEquals(groups1.length, 0);

  // Grouping with single service
  const singleService = new Map([
    ["web", createMockService("web", { image: "nginx", hosts: ["host1"] })],
  ]);
  const groups2 = ServiceFilter.group(singleService, {
    maxConcurrent: 5,
  });
  assertEquals(groups2.length, 1);
  assertEquals(groups2[0].length, 1);
});
