import { assertEquals, assertThrows } from "@std/assert";
import { type BuildConfig, ServiceConfiguration } from "../service.ts";
import { ConfigurationError } from "../base.ts";

// Test data
const MINIMAL_SERVICE_DATA = {
  image: "nginx:latest",
  servers: [{ host: "web1.example.com", arch: "amd64" }],
};

const COMPLETE_SERVICE_DATA = {
  image: "myapp:v1.0.0",
  servers: [
    { host: "web1.example.com", arch: "amd64" },
    { host: "web2.example.com", arch: "amd64" },
  ],
  ports: ["80:80", "443:443"],
  volumes: ["/data:/app/data", "/logs:/app/logs"],
  environment: {
    clear: {
      NODE_ENV: "production",
      DATABASE_URL: "postgres://localhost:5432/mydb",
      API_KEY: "secret123",
    },
  },
  command: ["npm", "start"],
};

const BUILD_SERVICE_DATA = {
  servers: [{ host: "localhost", arch: "amd64" }],
  build: {
    dockerfile: "Dockerfile",
    context: ".",
    args: {
      BUILD_ENV: "production",
      VERSION: "1.0.0",
    },
    target: "runtime",
  },
};

const STRING_BUILD_SERVICE_DATA = {
  servers: [{ host: "localhost", arch: "amd64" }],
  build: "./backend",
};

const SERVER_ARCH_SERVICE_DATA = {
  image: "nginx:latest",
  servers: [
    { host: "192.168.1.100", arch: "amd64" },
    { host: "192.168.1.101", arch: "arm64" },
    { host: "192.168.1.102", arch: "amd64" },
  ],
};

const MIXED_ARCH_BUILD_SERVICE_DATA = {
  build: "./app",
  servers: [
    { host: "192.168.1.100", arch: "amd64" },
    { host: "192.168.1.101", arch: "arm64" },
  ],
};

const INVALID_SERVICE_DATA = {
  // Missing required image and build
  servers: [{ host: "localhost", arch: "amd64" }],
};

Deno.test("ServiceConfiguration - minimal configuration", () => {
  const service = new ServiceConfiguration(
    "web",
    MINIMAL_SERVICE_DATA,
    "myproject",
  );

  assertEquals(service.name, "web");
  assertEquals(service.project, "myproject");
  assertEquals(service.image, "nginx:latest");
  assertEquals(service.servers[0].host, "web1.example.com");
  assertEquals(service.ports, []);
  assertEquals(service.volumes, []);
  assertEquals(Object.keys(service.environment.clear).length, 0);
  assertEquals(service.environment.secrets.length, 0);
  assertEquals(service.build, undefined);
  assertEquals(service.command, undefined);
});

Deno.test("ServiceConfiguration - complete configuration", () => {
  const service = new ServiceConfiguration(
    "api",
    COMPLETE_SERVICE_DATA,
    "myproject",
  );

  assertEquals(service.name, "api");
  assertEquals(service.project, "myproject");
  assertEquals(service.image, "myapp:v1.0.0");
  assertEquals(service.servers.map((s) => s.host), [
    "web1.example.com",
    "web2.example.com",
  ]);
  assertEquals(service.ports, ["80:80", "443:443"]);
  assertEquals(service.volumes, ["/data:/app/data", "/logs:/app/logs"]);
  assertEquals(service.command, ["npm", "start"]);

  // Environment configuration
  assertEquals(service.environment.clear.NODE_ENV, "production");
  assertEquals(
    service.environment.clear.DATABASE_URL,
    "postgres://localhost:5432/mydb",
  );
  assertEquals(service.environment.clear.API_KEY, "secret123");
});

Deno.test("ServiceConfiguration - build configuration object", () => {
  const service = new ServiceConfiguration(
    "api",
    BUILD_SERVICE_DATA,
    "myproject",
  );

  assertEquals(service.image, undefined);
  assertEquals(typeof service.build, "object");

  const buildConfig = service.build as unknown as Record<string, unknown>;
  assertEquals(buildConfig.dockerfile, "Dockerfile");
  assertEquals(buildConfig.context, ".");
  assertEquals(
    (buildConfig.args as Record<string, unknown>).BUILD_ENV,
    "production",
  );
  assertEquals((buildConfig.args as Record<string, unknown>).VERSION, "1.0.0");
  assertEquals(buildConfig.target, "runtime");
});

Deno.test("ServiceConfiguration - build configuration string", () => {
  const service = new ServiceConfiguration(
    "api",
    STRING_BUILD_SERVICE_DATA,
    "myproject",
  );

  assertEquals(service.image, undefined);
  assertEquals(service.build, "./backend");
});

Deno.test("ServiceConfiguration - requiresBuild method", () => {
  const serviceWithoutBuild = new ServiceConfiguration(
    "web",
    MINIMAL_SERVICE_DATA,
    "myproject",
  );
  const serviceWithBuild = new ServiceConfiguration(
    "api",
    BUILD_SERVICE_DATA,
    "myproject",
  );
  const serviceWithStringBuild = new ServiceConfiguration(
    "app",
    STRING_BUILD_SERVICE_DATA,
    "myproject",
  );

  assertEquals(serviceWithoutBuild.requiresBuild(), false);
  assertEquals(serviceWithBuild.requiresBuild(), true);
  assertEquals(serviceWithStringBuild.requiresBuild(), true);
});

Deno.test("ServiceConfiguration - getImageName method", () => {
  const serviceWithImage = new ServiceConfiguration(
    "web",
    MINIMAL_SERVICE_DATA,
    "myproject",
  );
  const serviceWithBuild = new ServiceConfiguration(
    "api",
    BUILD_SERVICE_DATA,
    "myproject",
  );

  assertEquals(serviceWithImage.getImageName(), "nginx:latest");
  assertEquals(serviceWithBuild.getImageName(), "myproject-api:latest");
  assertEquals(
    serviceWithBuild.getImageName("registry.com"),
    "registry.com/myproject-api:latest",
  );
});

Deno.test("ServiceConfiguration - getImageName preserves version tag from image", () => {
  const service = new ServiceConfiguration(
    "garage",
    {
      image: "dxflrs/garage:v2.1.0",
      servers: [{ host: "server.example.com", arch: "amd64" }],
    },
    "s3",
  );

  // When no version is passed, should preserve the version from the image
  assertEquals(service.getImageName(), "dxflrs/garage:v2.1.0");
  assertEquals(service.getImageName(undefined), "dxflrs/garage:v2.1.0");
});

Deno.test("ServiceConfiguration - getImageName can override version tag", () => {
  const service = new ServiceConfiguration(
    "garage",
    {
      image: "dxflrs/garage:v2.1.0",
      servers: [{ host: "server.example.com", arch: "amd64" }],
    },
    "s3",
  );

  // When version is explicitly passed, should override the version from the image
  assertEquals(
    service.getImageName(undefined, "v2.2.0"),
    "dxflrs/garage:v2.2.0",
  );
  assertEquals(
    service.getImageName(undefined, "latest"),
    "dxflrs/garage:latest",
  );
});

Deno.test("ServiceConfiguration - getImageName adds version to untagged image", () => {
  const service = new ServiceConfiguration(
    "nginx",
    {
      image: "nginx",
      servers: [{ host: "server.example.com", arch: "amd64" }],
    },
    "web",
  );

  // When version is passed and image has no tag, should add the version
  assertEquals(service.getImageName(undefined, "1.24"), "nginx:1.24");
  assertEquals(service.getImageName(undefined, "alpine"), "nginx:alpine");
});

Deno.test("ServiceConfiguration - getContainerName method", () => {
  const service = new ServiceConfiguration(
    "web",
    MINIMAL_SERVICE_DATA,
    "myproject",
  );

  assertEquals(service.getContainerName(), "myproject-web");
  assertEquals(service.getContainerName("1"), "myproject-web-1");
  assertEquals(
    service.getContainerName("production"),
    "myproject-web-production",
  );
});

Deno.test("ServiceConfiguration - validation passes for valid service", () => {
  const service = new ServiceConfiguration(
    "web",
    MINIMAL_SERVICE_DATA,
    "myproject",
  );

  // Should not throw
  service.validate();
});

Deno.test("ServiceConfiguration - validation passes for build service", () => {
  const service = new ServiceConfiguration(
    "api",
    BUILD_SERVICE_DATA,
    "myproject",
  );

  // Should not throw
  service.validate();
});

Deno.test("ServiceConfiguration - validation fails without image or build", () => {
  const service = new ServiceConfiguration(
    "web",
    INVALID_SERVICE_DATA,
    "myproject",
  );

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Service 'web' must specify either 'image' or 'build'",
  );
});

Deno.test("ServiceConfiguration - validation fails with both image and build", () => {
  const service = new ServiceConfiguration("invalid", {
    image: "nginx:latest",
    build: "./app",
    servers: [{ host: "localhost" }],
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Service 'invalid' cannot specify both 'image' and 'build'",
  );
});

Deno.test("ServiceConfiguration - validation fails without servers", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    // Missing servers array
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Service 'web' must specify at least one server",
  );
});

Deno.test("ServiceConfiguration - validation fails with empty servers", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    servers: [],
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Service 'web' must specify at least one server",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid image type", () => {
  const service = new ServiceConfiguration("web", {
    image: 123,
    servers: [{ host: "localhost" }],
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "'image' in web must be a string",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid servers type", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    servers: "not-an-array",
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "'servers' for service 'web' must be an array",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid server", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    servers: [{ host: "web1.example.com", arch: "amd64" }, 123],
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Server at index 1 for service 'web' must be an object with 'host' property",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid ports type", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    servers: [{ host: "localhost" }],
    ports: "not-an-array",
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "'ports' in web must be an array",
  );
});

Deno.test("ServiceConfiguration - validation succeeds with container port only", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    servers: [{ host: "web1.example.com", arch: "amd64" }],
    ports: ["8000"], // Container port only format
  }, "myproject");

  // Should not throw - validation should pass
  service.validate();
});

Deno.test("ServiceConfiguration - validation succeeds with host:container port format", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    servers: [{ host: "web1.example.com", arch: "amd64" }],
    ports: ["8080:8000"], // host_port:container_port format
  }, "myproject");

  // Should not throw - validation should pass
  service.validate();
});

Deno.test("ServiceConfiguration - validation succeeds with full port format", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    servers: [{ host: "web1.example.com", arch: "amd64" }],
    ports: ["192.168.1.1:8080:8000/tcp"], // Full format with IP and protocol
  }, "myproject");

  // Should not throw - validation should pass
  service.validate();
});

Deno.test("ServiceConfiguration - validation fails with invalid port format", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    servers: [{ host: "web1.example.com", arch: "amd64" }],
    ports: ["invalid:port"], // Invalid format
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Invalid port mapping 'invalid:port' for service 'web'. Expected format: container_port, host_port:container_port, or [host_ip:]host_port:container_port[/protocol]",
  );
});

Deno.test("ServiceConfiguration - validation fails with out of range port", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    servers: [{ host: "web1.example.com", arch: "amd64" }],
    ports: ["99999"], // Port out of range
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Invalid port mapping '99999' for service 'web'. Expected format: container_port, host_port:container_port, or [host_ip:]host_port:container_port[/protocol]",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid IP in port mapping", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    servers: [{ host: "web1.example.com", arch: "amd64" }],
    ports: ["999.999.999.999:8080:8000"], // Invalid IP
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Invalid port mapping '999.999.999.999:8080:8000' for service 'web'. Expected format: container_port, host_port:container_port, or [host_ip:]host_port:container_port[/protocol]",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid volumes type", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    servers: [{ host: "localhost" }],
    volumes: "not-an-array",
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "'volumes' in web must be an array",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid environment type", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    servers: [{ host: "localhost" }],
    environment: "not-an-object",
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "'environment' for service 'web' must be an object",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid environment variable name", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    servers: [{ host: "localhost" }],
    environment: {
      clear: {
        "INVALID-VAR": "value", // Hyphens not allowed
      },
    },
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Invalid environment variable name",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid command type", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    servers: [{ host: "localhost" }],
    command: 123,
  }, "myproject");

  // Command validation doesn't throw - it's handled during property access
  // This test verifies that invalid command types are handled gracefully
  assertThrows(
    () => service.command,
    ConfigurationError,
    "'command' for service 'web' must be a string or array",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid build type", () => {
  const service = new ServiceConfiguration("web", {
    servers: [{ host: "localhost" }],
    build: 123,
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "'build' for service 'web' must be a string or object",
  );
});

Deno.test("ServiceConfiguration - validation fails with build missing context", () => {
  const service = new ServiceConfiguration("web", {
    servers: [{ host: "localhost" }],
    build: {
      dockerfile: "Dockerfile",
    },
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "'build.context' in web must be a string",
  );
});

Deno.test("ServiceConfiguration - toObject method", () => {
  const service = new ServiceConfiguration(
    "api",
    COMPLETE_SERVICE_DATA,
    "myproject",
  );
  const obj = service.toObject();

  assertEquals(obj.image, "myapp:v1.0.0");
  assertEquals(obj.servers, [
    { host: "web1.example.com", arch: "amd64" },
    { host: "web2.example.com", arch: "amd64" },
  ]);
  assertEquals(obj.ports, ["80:80", "443:443"]);
  assertEquals(obj.volumes, ["/data:/app/data", "/logs:/app/logs"]);
  assertEquals(obj.command, ["npm", "start"]);
  assertEquals(obj.environment, {
    clear: {
      NODE_ENV: "production",
      DATABASE_URL: "postgres://localhost:5432/mydb",
      API_KEY: "secret123",
    },
  });
});

Deno.test("ServiceConfiguration - toObject excludes empty arrays and undefined values", () => {
  const service = new ServiceConfiguration(
    "web",
    MINIMAL_SERVICE_DATA,
    "myproject",
  );
  const obj = service.toObject();

  assertEquals(obj.image, "nginx:latest");
  assertEquals(obj.servers, [{ host: "web1.example.com", arch: "amd64" }]);

  // Empty arrays and undefined values should not be included
  assertEquals("ports" in obj, false);
  assertEquals("volumes" in obj, false);
  assertEquals("environment" in obj, false);
  assertEquals("command" in obj, false);
  assertEquals("build" in obj, false);
});

Deno.test("ServiceConfiguration - toObject with build configuration", () => {
  const service = new ServiceConfiguration(
    "web",
    BUILD_SERVICE_DATA,
    "myproject",
  );
  const obj = service.toObject();

  assertEquals("image" in obj, false);
  assertEquals(obj.build, {
    dockerfile: "Dockerfile",
    context: ".",
    args: {
      BUILD_ENV: "production",
      VERSION: "1.0.0",
    },
    target: "runtime",
  });
  assertEquals(obj.servers, [{ host: "localhost", arch: "amd64" }]);
});

Deno.test("ServiceConfiguration - lazy loading of properties", () => {
  const service = new ServiceConfiguration(
    "web",
    COMPLETE_SERVICE_DATA,
    "myproject",
  );

  // Access properties multiple times to ensure they're cached
  assertEquals(service.image, "myapp:v1.0.0");
  assertEquals(service.image, "myapp:v1.0.0");

  const servers1 = service.servers;
  const servers2 = service.servers;
  assertEquals(servers1, servers2); // Should be the same instance

  const env1 = service.environment;
  const env2 = service.environment;
  assertEquals(env1, env2); // Should be the same instance
});

Deno.test("ServiceConfiguration - command as string", () => {
  const serviceData = {
    image: "nginx:latest",
    servers: [{ host: "localhost" }],
    command: "nginx -g 'daemon off;'",
  };

  const service = new ServiceConfiguration("web", serviceData, "myproject");
  assertEquals(service.command, "nginx -g 'daemon off;'");
});

Deno.test("ServiceConfiguration - command as array", () => {
  const serviceData = {
    image: "nginx:latest",
    servers: [{ host: "localhost" }],
    command: ["nginx", "-g", "daemon off;"],
  };

  const service = new ServiceConfiguration("web", serviceData, "myproject");
  assertEquals(service.command, ["nginx", "-g", "daemon off;"]);
});

Deno.test("ServiceConfiguration - build with all options", () => {
  const service = new ServiceConfiguration(
    "api",
    BUILD_SERVICE_DATA,
    "myproject",
  );
  const build = service.build as BuildConfig;

  assertEquals(build.context, ".");
  assertEquals(build.dockerfile, "Dockerfile");
  assertEquals(build.target, "runtime");
  assertEquals(typeof build.args, "object");
  assertEquals(build.args?.BUILD_ENV, "production");
  assertEquals(build.args?.VERSION, "1.0.0");
});

Deno.test("ServiceConfiguration - build with minimal options", () => {
  const serviceData = {
    servers: [{ host: "localhost", arch: "amd64" }],
    build: {
      context: "./frontend",
    },
  };

  const service = new ServiceConfiguration("web", serviceData, "myproject");
  const build = service.build as BuildConfig;

  assertEquals(build.context, "./frontend");
  assertEquals(build.dockerfile, undefined);
  assertEquals(build.args, undefined);
  assertEquals(build.target, undefined);
});

Deno.test("ServiceConfiguration - server with architecture configuration", () => {
  const service = new ServiceConfiguration(
    "web",
    SERVER_ARCH_SERVICE_DATA,
    "myproject",
  );

  const servers = service.servers;
  assertEquals(servers.length, 3);

  // Check server objects
  assertEquals(servers[0], { host: "192.168.1.100", arch: "amd64" });
  assertEquals(servers[1], { host: "192.168.1.101", arch: "arm64" });
  assertEquals(servers[2], { host: "192.168.1.102", arch: "amd64" });
});

Deno.test("ServiceConfiguration - getRequiredArchitectures returns unique architectures", () => {
  const service = new ServiceConfiguration(
    "web",
    SERVER_ARCH_SERVICE_DATA,
    "myproject",
  );

  const architectures = service.getRequiredArchitectures();
  assertEquals(architectures.sort(), ["amd64", "arm64"]);
});

Deno.test("ServiceConfiguration - getServersByArchitecture groups servers correctly", () => {
  const service = new ServiceConfiguration(
    "web",
    SERVER_ARCH_SERVICE_DATA,
    "myproject",
  );

  const serversByArch = service.getServersByArchitecture();

  assertEquals(serversByArch.get("amd64"), ["192.168.1.100", "192.168.1.102"]);
  assertEquals(serversByArch.get("arm64"), ["192.168.1.101"]);
});

Deno.test("ServiceConfiguration - build service with mixed server architectures", () => {
  const service = new ServiceConfiguration(
    "web",
    MIXED_ARCH_BUILD_SERVICE_DATA,
    "myproject",
  );

  const architectures = service.getRequiredArchitectures();
  assertEquals(architectures.sort(), ["amd64", "arm64"]);

  const serversByArch = service.getServersByArchitecture();
  assertEquals(serversByArch.get("amd64"), ["192.168.1.100"]);
  assertEquals(serversByArch.get("arm64"), ["192.168.1.101"]);
});

Deno.test("ServiceConfiguration - default architecture for servers without arch", () => {
  const serviceData = {
    image: "nginx:latest",
    servers: [
      { host: "192.168.1.100" },
      { host: "192.168.1.101" },
    ],
  };

  const service = new ServiceConfiguration("web", serviceData, "myproject");
  const architectures = service.getRequiredArchitectures();

  assertEquals(architectures, ["amd64"]); // all default to amd64

  const serversByArch = service.getServersByArchitecture();
  assertEquals(serversByArch.get("amd64"), ["192.168.1.100", "192.168.1.101"]);
});

Deno.test("ServiceConfiguration - invalid server architecture throws error", () => {
  const serviceData = {
    image: "nginx:latest",
    servers: [
      { host: "192.168.1.100", arch: "x86" }, // invalid arch
    ],
  };

  assertThrows(
    () => {
      const service = new ServiceConfiguration("web", serviceData, "myproject");
      service.validate(); // This should trigger validation
    },
    ConfigurationError,
    "Invalid architecture 'x86'",
  );
});

Deno.test("ServiceConfiguration - invalid server object format throws error", () => {
  const serviceData = {
    image: "nginx:latest",
    servers: [
      { arch: "amd64" }, // missing host property
    ],
  };

  assertThrows(
    () => {
      const service = new ServiceConfiguration("web", serviceData, "myproject");
      service.servers; // Access servers to trigger validation
    },
    ConfigurationError,
    "Server at index 0 for service 'web' must have a 'host' property",
  );
});

Deno.test("ServiceConfiguration - server without arch defaults to amd64", () => {
  const serviceData = {
    image: "nginx:latest",
    servers: [
      { host: "192.168.1.100" }, // no arch specified
    ],
  };

  const service = new ServiceConfiguration("web", serviceData, "myproject");
  const architectures = service.getRequiredArchitectures();

  assertEquals(architectures, ["amd64"]);
});
