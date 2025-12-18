import { assertEquals, assertThrows } from "@std/assert";
import { Configuration } from "../src/lib/configuration.ts";

// Test data for project integration
const PROJECT_CONFIG_DATA = {
  project: "myapp",
  engine: "docker" as const,
  ssh: {
    user: "deploy",
    port: 22,
  },
  services: {
    web: {
      image: "nginx:latest",
      hosts: ["web1.example.com", "web2.example.com"],
      ports: ["80:80", "443:443"],
    },
    api: {
      build: {
        context: "./api",
        dockerfile: "Dockerfile.prod",
      },
      hosts: ["api1.example.com"],
      ports: ["3000:3000"],
      environment: {
        NODE_ENV: "production",
        DATABASE_URL: "postgres://db:5432/myapp",
      },
    },
    database: {
      image: "postgres:15",
      hosts: ["db1.example.com"],
      ports: ["5432:5432"],
      environment: {
        POSTGRES_DB: "myapp",
        POSTGRES_USER: "appuser",
        POSTGRES_PASSWORD: "secret123",
      },
    },
  },
};

Deno.test("Project Integration - Configuration loads project field", () => {
  const config = new Configuration(PROJECT_CONFIG_DATA);

  assertEquals(config.project, "myapp");
  assertEquals(config.engine, "docker");
  assertEquals(config.services.size, 3);
});

Deno.test("Project Integration - Services receive project context", () => {
  const config = new Configuration(PROJECT_CONFIG_DATA);

  const webService = config.getService("web");
  const apiService = config.getService("api");
  const dbService = config.getService("database");

  // All services should have the project name
  assertEquals(webService.project, "myapp");
  assertEquals(apiService.project, "myapp");
  assertEquals(dbService.project, "myapp");
});

Deno.test("Project Integration - Image naming uses project prefix", () => {
  const config = new Configuration(PROJECT_CONFIG_DATA);

  const webService = config.getService("web");
  const apiService = config.getService("api");

  // Service with existing image should keep original image name
  assertEquals(webService.getImageName(), "nginx:latest");

  // Service with build should generate project-prefixed image name
  assertEquals(apiService.getImageName(), "myapp-api:latest");
  assertEquals(
    apiService.getImageName("registry.io"),
    "registry.io/myapp-api:latest",
  );
});

Deno.test("Project Integration - Container naming uses project prefix", () => {
  const config = new Configuration(PROJECT_CONFIG_DATA);

  const webService = config.getService("web");
  const apiService = config.getService("api");
  const dbService = config.getService("database");

  // Basic container names should include project prefix
  assertEquals(webService.getContainerName(), "myapp-web");
  assertEquals(apiService.getContainerName(), "myapp-api");
  assertEquals(dbService.getContainerName(), "myapp-database");

  // Container names with suffixes
  assertEquals(webService.getContainerName("1"), "myapp-web-1");
  assertEquals(apiService.getContainerName("prod"), "myapp-api-prod");
  assertEquals(dbService.getContainerName("primary"), "myapp-database-primary");
});

Deno.test("Project Integration - Project validation enforces naming rules", () => {
  // Valid project names should work
  const validConfigs = [
    {
      project: "myapp",
      engine: "docker",
      ssh: { user: "deploy" },
      services: { web: { image: "nginx", hosts: ["localhost"] } },
    },
    {
      project: "my-app",
      engine: "docker",
      ssh: { user: "deploy" },
      services: { web: { image: "nginx", hosts: ["localhost"] } },
    },
    {
      project: "my_app",
      engine: "docker",
      ssh: { user: "deploy" },
      services: { web: { image: "nginx", hosts: ["localhost"] } },
    },
    {
      project: "app123",
      engine: "docker",
      ssh: { user: "deploy" },
      services: { web: { image: "nginx", hosts: ["localhost"] } },
    },
  ];

  validConfigs.forEach((configData, index) => {
    const config = new Configuration(configData);
    const result = config.validate();

    assertEquals(result.valid, true, `Config ${index} should be valid`);
  });
});

Deno.test("Project Integration - Project validation rejects invalid names", () => {
  // Invalid project names should fail validation
  const invalidConfigs = [
    {
      project: "",
      engine: "docker",
      ssh: { user: "deploy" },
      services: { web: { image: "nginx", hosts: ["localhost"] } },
    }, // Empty
    {
      project: "My-App",
      engine: "docker",
      ssh: { user: "deploy" },
      services: { web: { image: "nginx", hosts: ["localhost"] } },
    }, // Uppercase
    {
      project: "my app",
      engine: "docker",
      ssh: { user: "deploy" },
      services: { web: { image: "nginx", hosts: ["localhost"] } },
    }, // Space
    {
      project: "my.app",
      engine: "docker",
      ssh: { user: "deploy" },
      services: { web: { image: "nginx", hosts: ["localhost"] } },
    }, // Dot
    {
      project: "my@app",
      engine: "docker",
      ssh: { user: "deploy" },
      services: { web: { image: "nginx", hosts: ["localhost"] } },
    }, // Special char
    {
      project: "-myapp",
      engine: "docker",
      ssh: { user: "deploy" },
      services: { web: { image: "nginx", hosts: ["localhost"] } },
    }, // Start with hyphen
    {
      project: "myapp-",
      engine: "docker",
      ssh: { user: "deploy" },
      services: { web: { image: "nginx", hosts: ["localhost"] } },
    }, // End with hyphen
  ];

  invalidConfigs.forEach((configData, index) => {
    const config = new Configuration(configData);
    const result = config.validate();
    assertEquals(result.valid, false, `Config ${index} should be invalid`);

    // Should have project-related validation errors
    const projectErrors = result.errors.filter((err: { path: string }) => err.path === "project");
    assertEquals(
      projectErrors.length > 0,
      true,
      `Config ${index} should have project validation errors`,
    );
  });
});

Deno.test("Project Integration - Missing project field fails validation", () => {
  const configWithoutProject = {
    engine: "docker" as const,
    services: {
      web: {
        image: "nginx:latest",
        hosts: ["localhost"],
      },
    },
  };

  const config = new Configuration(configWithoutProject);

  assertThrows(
    () => config.project,
    Error,
    "Missing required configuration: 'project'",
  );
});

Deno.test("Project Integration - toObject includes project field", () => {
  const config = new Configuration(PROJECT_CONFIG_DATA);
  const obj = config.toObject();

  assertEquals(obj.project, "myapp");
  assertEquals(typeof obj.services, "object");

  // Verify services are properly serialized
  const services = obj.services as Record<string, unknown>;
  assertEquals("web" in services, true);
  assertEquals("api" in services, true);
  assertEquals("database" in services, true);
});

Deno.test("Project Integration - withDefaults creates valid project config", () => {
  const defaultConfig = Configuration.withDefaults();

  assertEquals(defaultConfig.project, "default");
  assertEquals(defaultConfig.engine, "podman");

  // Should be able to get services with project context
  const webService = defaultConfig.getService("web");
  assertEquals(webService.project, "default");
  assertEquals(webService.getContainerName(), "default-web");
});

Deno.test("Project Integration - withDefaults accepts project override", () => {
  const customConfig = Configuration.withDefaults({
    project: "custom-project",
    engine: "docker",
  });

  assertEquals(customConfig.project, "custom-project");
  assertEquals(customConfig.engine, "docker");

  const webService = customConfig.getService("web");
  assertEquals(webService.project, "custom-project");
  assertEquals(webService.getContainerName(), "custom-project-web");
  assertEquals(webService.getImageName(), "nginx:latest"); // Should keep original for existing images
});

Deno.test("Project Integration - Service names are scoped by project", () => {
  const config = new Configuration(PROJECT_CONFIG_DATA);

  // Multiple services in the same project should have unique container names
  const webService = config.getService("web");
  const apiService = config.getService("api");
  const dbService = config.getService("database");

  const containerNames = [
    webService.getContainerName(),
    apiService.getContainerName(),
    dbService.getContainerName(),
  ];

  // All container names should be unique
  const uniqueNames = new Set(containerNames);
  assertEquals(uniqueNames.size, containerNames.length);

  // All should start with project prefix
  containerNames.forEach((name) => {
    assertEquals(
      name.startsWith("myapp-"),
      true,
      `${name} should start with project prefix`,
    );
  });
});

Deno.test("Project Integration - Complex project name validation", () => {
  // Test edge cases for project name validation
  const edgeCases = [
    { name: "a", valid: true }, // Single character
    { name: "a".repeat(50), valid: true }, // Max length (50)
    { name: "a".repeat(51), valid: false }, // Over max length
    { name: "my-app-v2", valid: true }, // Multiple hyphens
    { name: "my_app_v2", valid: true }, // Multiple underscores
    { name: "app-2023", valid: true }, // Numbers with hyphen
    { name: "2023-app", valid: true }, // Starting with number
    { name: "my--app", valid: false }, // Double hyphen
    { name: "my__app", valid: false }, // Double underscore
    { name: "my-_app", valid: false }, // Mixed separators
  ];

  edgeCases.forEach(({ name, valid }, index) => {
    const configData = {
      project: name,
      engine: "docker" as const,
      ssh: { user: "deploy" },
      services: {
        web: {
          image: "nginx:latest",
          hosts: ["localhost"],
        },
      },
    };

    const config = new Configuration(configData);
    const result = config.validate();

    assertEquals(
      result.valid,
      valid,
      `Project name "${name}" (case ${index}) should ${
        valid ? "be valid" : "be invalid"
      }`,
    );
  });
});
