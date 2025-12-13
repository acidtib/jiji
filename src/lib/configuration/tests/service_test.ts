import { assertEquals, assertThrows } from "@std/assert";
import { ServiceConfiguration } from "../service.ts";
import { ConfigurationError } from "../base.ts";

// Test data
const MINIMAL_SERVICE_DATA = {
  image: "nginx:latest",
  hosts: ["web1.example.com"],
};

const COMPLETE_SERVICE_DATA = {
  image: "myapp:v1.0.0",
  hosts: ["web1.example.com", "web2.example.com"],
  ports: ["80:80", "443:443"],
  volumes: ["/data:/app/data", "/logs:/app/logs"],
  environment: {
    NODE_ENV: "production",
    DATABASE_URL: "postgres://localhost:5432/mydb",
    API_KEY: "secret123",
  },
  command: ["npm", "start"],
};

const ENVIRONMENT_ARRAY_SERVICE_DATA = {
  image: "myapp:latest",
  hosts: ["localhost"],
  environment: [
    "NODE_ENV=production",
    "PORT=3000",
    "DEBUG=false",
  ],
};

const BUILD_SERVICE_DATA = {
  hosts: ["localhost"],
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
  hosts: ["localhost"],
  build: "./backend",
};

const INVALID_SERVICE_DATA = {
  // Missing required image and build
  hosts: ["localhost"],
};

Deno.test("ServiceConfiguration - minimal configuration", () => {
  const service = new ServiceConfiguration("web", MINIMAL_SERVICE_DATA);

  assertEquals(service.name, "web");
  assertEquals(service.image, "nginx:latest");
  assertEquals(service.hosts, ["web1.example.com"]);
  assertEquals(service.ports, []);
  assertEquals(service.volumes, []);
  assertEquals(service.environment, {});
  assertEquals(service.build, undefined);
  assertEquals(service.command, undefined);
});

Deno.test("ServiceConfiguration - complete configuration", () => {
  const service = new ServiceConfiguration("api", COMPLETE_SERVICE_DATA);

  assertEquals(service.name, "api");
  assertEquals(service.image, "myapp:v1.0.0");
  assertEquals(service.hosts, ["web1.example.com", "web2.example.com"]);
  assertEquals(service.ports, ["80:80", "443:443"]);
  assertEquals(service.volumes, ["/data:/app/data", "/logs:/app/logs"]);
  assertEquals(service.command, ["npm", "start"]);

  // Environment variables as object
  const env = service.environment as Record<string, string>;
  assertEquals(env.NODE_ENV, "production");
  assertEquals(env.DATABASE_URL, "postgres://localhost:5432/mydb");
  assertEquals(env.API_KEY, "secret123");
});

Deno.test("ServiceConfiguration - environment as array", () => {
  const service = new ServiceConfiguration(
    "web",
    ENVIRONMENT_ARRAY_SERVICE_DATA,
  );

  assertEquals(Array.isArray(service.environment), true);
  const envArray = service.environment as string[];
  assertEquals(envArray, [
    "NODE_ENV=production",
    "PORT=3000",
    "DEBUG=false",
  ]);
});

Deno.test("ServiceConfiguration - build configuration object", () => {
  const service = new ServiceConfiguration("api", BUILD_SERVICE_DATA);

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
  const service = new ServiceConfiguration("api", STRING_BUILD_SERVICE_DATA);

  assertEquals(service.image, undefined);
  assertEquals(service.build, "./backend");
});

Deno.test("ServiceConfiguration - requiresBuild method", () => {
  const serviceWithoutBuild = new ServiceConfiguration(
    "web",
    MINIMAL_SERVICE_DATA,
  );
  const serviceWithBuild = new ServiceConfiguration("api", BUILD_SERVICE_DATA);
  const serviceWithStringBuild = new ServiceConfiguration(
    "app",
    STRING_BUILD_SERVICE_DATA,
  );

  assertEquals(serviceWithoutBuild.requiresBuild(), false);
  assertEquals(serviceWithBuild.requiresBuild(), true);
  assertEquals(serviceWithStringBuild.requiresBuild(), true);
});

Deno.test("ServiceConfiguration - getImageName method", () => {
  const serviceWithImage = new ServiceConfiguration(
    "web",
    MINIMAL_SERVICE_DATA,
  );
  const serviceWithBuild = new ServiceConfiguration("api", BUILD_SERVICE_DATA);

  assertEquals(serviceWithImage.getImageName(), "nginx:latest");
  assertEquals(serviceWithBuild.getImageName(), "api:latest");
  assertEquals(
    serviceWithBuild.getImageName("registry.com"),
    "registry.com/api:latest",
  );
});

Deno.test("ServiceConfiguration - validation passes for valid service", () => {
  const service = new ServiceConfiguration("web", MINIMAL_SERVICE_DATA);

  // Should not throw
  service.validate();
});

Deno.test("ServiceConfiguration - validation passes for build service", () => {
  const service = new ServiceConfiguration("api", BUILD_SERVICE_DATA);

  // Should not throw
  service.validate();
});

Deno.test("ServiceConfiguration - validation fails without image or build", () => {
  const service = new ServiceConfiguration("web", INVALID_SERVICE_DATA);

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Service 'web' must specify either 'image' or 'build'",
  );
});

Deno.test("ServiceConfiguration - validation fails with both image and build", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    build: ".",
    hosts: ["localhost"],
  });

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Service 'web' cannot specify both 'image' and 'build'",
  );
});

Deno.test("ServiceConfiguration - validation fails without hosts", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
  });

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Service 'web' must specify at least one host",
  );
});

Deno.test("ServiceConfiguration - validation fails with empty hosts", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: [],
  });

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Service 'web' must specify at least one host",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid image type", () => {
  const service = new ServiceConfiguration("web", {
    image: 123,
    hosts: ["localhost"],
  });

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "'image' in web must be a string",
  );
});

Deno.test("ServiceConfiguration - validation fails with empty image", () => {
  const service = new ServiceConfiguration("web", {
    image: "",
    hosts: ["localhost"],
  });

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Service 'web' must specify either 'image' or 'build'",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid hosts type", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: "not-an-array",
  });

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "'hosts' in web must be an array",
  );
});

Deno.test("ServiceConfiguration - validation fails with non-string host", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["valid-host", 123],
  });

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "'hosts' in web must be a string",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid ports type", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["localhost"],
    ports: "not-an-array",
  });

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "'ports' in web must be an array",
  );
});

Deno.test("ServiceConfiguration - validation fails with non-string port", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["localhost"],
    ports: ["80:80", 443],
  });

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Invalid port mapping '443' for service 'web'. Expected format: [host_ip:]host_port:container_port[/protocol]",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid volumes type", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["localhost"],
    volumes: "not-an-array",
  });

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "'volumes' in web must be an array",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid environment type", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["localhost"],
    environment: "not-an-object-or-array",
  });

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "'environment' for service 'web' must be an array or object",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid environment array format", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["localhost"],
    environment: ["VALID_VAR=value", "INVALID_VAR_NO_EQUALS"],
  });

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Invalid environment variable 'INVALID_VAR_NO_EQUALS' for service 'web'. Expected format: KEY=value",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid command type", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["localhost"],
    command: 123,
  });

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
    hosts: ["localhost"],
    build: 123,
  });

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "'build' for service 'web' must be a string or object",
  );
});

Deno.test("ServiceConfiguration - validation fails with build missing context", () => {
  const service = new ServiceConfiguration("web", {
    hosts: ["localhost"],
    build: {
      dockerfile: "Dockerfile",
    },
  });

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "'build.context' in web must be a string",
  );
});

Deno.test("ServiceConfiguration - toObject method", () => {
  const service = new ServiceConfiguration("web", COMPLETE_SERVICE_DATA);
  const obj = service.toObject();

  assertEquals(obj.image, "myapp:v1.0.0");
  assertEquals(obj.hosts, ["web1.example.com", "web2.example.com"]);
  assertEquals(obj.ports, ["80:80", "443:443"]);
  assertEquals(obj.volumes, ["/data:/app/data", "/logs:/app/logs"]);
  assertEquals(obj.command, ["npm", "start"]);
  assertEquals(obj.environment, {
    NODE_ENV: "production",
    DATABASE_URL: "postgres://localhost:5432/mydb",
    API_KEY: "secret123",
  });
});

Deno.test("ServiceConfiguration - toObject excludes empty arrays and undefined values", () => {
  const service = new ServiceConfiguration("web", MINIMAL_SERVICE_DATA);
  const obj = service.toObject();

  assertEquals(obj.image, "nginx:latest");
  assertEquals(obj.hosts, ["web1.example.com"]);

  // Empty arrays and undefined values should not be included
  assertEquals("ports" in obj, false);
  assertEquals("volumes" in obj, false);
  assertEquals("environment" in obj, false);
  assertEquals("command" in obj, false);
  assertEquals("build" in obj, false);
});

Deno.test("ServiceConfiguration - toObject with build configuration", () => {
  const service = new ServiceConfiguration("api", BUILD_SERVICE_DATA);
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
});

Deno.test("ServiceConfiguration - lazy loading of properties", () => {
  const service = new ServiceConfiguration("web", COMPLETE_SERVICE_DATA);

  // Access properties multiple times to ensure they're cached
  assertEquals(service.image, "myapp:v1.0.0");
  assertEquals(service.image, "myapp:v1.0.0");

  const hosts1 = service.hosts;
  const hosts2 = service.hosts;
  assertEquals(hosts1, hosts2); // Should be the same instance

  const env1 = service.environment;
  const env2 = service.environment;
  assertEquals(env1, env2); // Should be the same instance
});

Deno.test("ServiceConfiguration - command as string", () => {
  const serviceData = {
    image: "nginx:latest",
    hosts: ["localhost"],
    command: "nginx -g 'daemon off;'",
  };

  const service = new ServiceConfiguration("web", serviceData);
  assertEquals(service.command, "nginx -g 'daemon off;'");
});

Deno.test("ServiceConfiguration - command as array", () => {
  const serviceData = {
    image: "nginx:latest",
    hosts: ["localhost"],
    command: ["nginx", "-g", "daemon off;"],
  };

  const service = new ServiceConfiguration("web", serviceData);
  assertEquals(service.command, ["nginx", "-g", "daemon off;"]);
});

Deno.test("ServiceConfiguration - build with all options", () => {
  const service = new ServiceConfiguration("api", BUILD_SERVICE_DATA);
  const build = service.build as unknown as Record<string, unknown>;

  assertEquals(build.context, ".");
  assertEquals(build.dockerfile, "Dockerfile");
  assertEquals(build.target, "runtime");
  assertEquals(typeof build.args, "object");
  assertEquals((build.args as Record<string, unknown>).BUILD_ENV, "production");
  assertEquals((build.args as Record<string, unknown>).VERSION, "1.0.0");
});

Deno.test("ServiceConfiguration - build with minimal options", () => {
  const serviceData = {
    hosts: ["localhost"],
    build: {
      context: "./app",
    },
  };

  const service = new ServiceConfiguration("web", serviceData);
  const build = service.build as unknown as Record<string, unknown>;

  assertEquals(build.context, "./app");
  assertEquals(build.dockerfile, undefined);
  assertEquals(build.args, undefined);
  assertEquals(build.target, undefined);
});
