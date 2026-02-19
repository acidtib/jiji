import { assertEquals, assertThrows } from "@std/assert";
import { type BuildConfig, ServiceConfiguration } from "../service.ts";
import { ConfigurationError } from "../base.ts";

// Test data
const MINIMAL_SERVICE_DATA = {
  image: "nginx:latest",
  hosts: ["web1"],
};

const COMPLETE_SERVICE_DATA = {
  image: "myapp:v1.0.0",
  hosts: ["web1", "web2"],
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
  const service = new ServiceConfiguration(
    "web",
    MINIMAL_SERVICE_DATA,
    "myproject",
  );

  assertEquals(service.name, "web");
  assertEquals(service.project, "myproject");
  assertEquals(service.image, "nginx:latest");
  assertEquals(service.hosts, ["web1"]);
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
  assertEquals(service.hosts, ["web1", "web2"]);
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
      hosts: ["server.example.com"],
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
      hosts: ["server.example.com"],
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
      hosts: ["server.example.com"],
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
    hosts: ["localhost"],
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Service 'invalid' cannot specify both 'image' and 'build'",
  );
});

Deno.test("ServiceConfiguration - validation fails without hosts", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    // Missing hosts array
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Service 'web' must specify at least one host in the 'hosts' array",
  );
});

Deno.test("ServiceConfiguration - validation fails with empty hosts", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: [],
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Service 'web' must specify at least one host in the 'hosts' array",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid image type", () => {
  const service = new ServiceConfiguration("web", {
    image: 123,
    hosts: ["localhost"],
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "'image' in web must be a string",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid hosts type", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: "not-an-array",
  }, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "'hosts' for service 'web' must be an array",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid ports type", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["localhost"],
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
    hosts: ["web1.example.com"],
    ports: ["8000"], // Container port only format
  }, "myproject");

  // Should not throw - validation should pass
  service.validate();
});

Deno.test("ServiceConfiguration - validation succeeds with host:container port format", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["web1.example.com"],
    ports: ["8080:8000"], // host_port:container_port format
  }, "myproject");

  // Should not throw - validation should pass
  service.validate();
});

Deno.test("ServiceConfiguration - validation succeeds with full port format", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["web1.example.com"],
    ports: ["192.168.1.1:8080:8000/tcp"], // Full format with IP and protocol
  }, "myproject");

  // Should not throw - validation should pass
  service.validate();
});

Deno.test("ServiceConfiguration - validation succeeds with UDP protocol", () => {
  const service = new ServiceConfiguration("dns", {
    image: "dns-server:latest",
    hosts: ["dns1.example.com"],
    ports: [
      "1900/udp", // Container port only with UDP
      "53:53/udp", // Host:container with UDP
      "127.0.0.1:5353:53/udp", // Full format with IP and UDP
    ],
  }, "myproject");

  // Should not throw - validation should pass
  service.validate();
});

Deno.test("ServiceConfiguration - validation succeeds with mixed TCP/UDP protocols", () => {
  const service = new ServiceConfiguration("multiport", {
    image: "app:latest",
    hosts: ["app1.example.com"],
    ports: [
      "80:80/tcp", // HTTP over TCP
      "443:443/tcp", // HTTPS over TCP
      "53:53/udp", // DNS over UDP
      "123:123/udp", // NTP over UDP
      "8080:8080", // No protocol (defaults to TCP)
    ],
  }, "myproject");

  // Should not throw - validation should pass
  service.validate();
});

Deno.test("ServiceConfiguration - validation fails with invalid port format", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["web1.example.com"],
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
    hosts: ["web1.example.com"],
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
    hosts: ["web1.example.com"],
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
    hosts: ["localhost"],
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
    hosts: ["localhost"],
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
    hosts: ["localhost"],
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
    hosts: ["localhost"],
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
    hosts: ["localhost"],
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
    hosts: ["localhost"],
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
  assertEquals(obj.hosts, ["web1", "web2"]);
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
  assertEquals(obj.hosts, ["web1"]);

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
  assertEquals(obj.hosts, ["localhost"]);
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

  const service = new ServiceConfiguration("web", serviceData, "myproject");
  assertEquals(service.command, "nginx -g 'daemon off;'");
});

Deno.test("ServiceConfiguration - command as array", () => {
  const serviceData = {
    image: "nginx:latest",
    hosts: ["localhost"],
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
    hosts: ["localhost"],
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

// Resource constraints tests
Deno.test("ServiceConfiguration - cpus as number", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["web1.example.com"],
    cpus: 2,
  }, "myproject");

  assertEquals(service.cpus, 2);
});

Deno.test("ServiceConfiguration - cpus as numeric string", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["web1.example.com"],
    cpus: "1.5",
  }, "myproject");

  assertEquals(service.cpus, "1.5");
});

Deno.test("ServiceConfiguration - cpus as fractional number", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["web1.example.com"],
    cpus: 0.5,
  }, "myproject");

  assertEquals(service.cpus, 0.5);
});

Deno.test("ServiceConfiguration - cpus validation fails with negative number", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["web1.example.com"],
    cpus: -1,
  }, "myproject");

  assertThrows(
    () => service.cpus,
    ConfigurationError,
    "'cpus' for service 'web' must be a positive number or numeric string",
  );
});

Deno.test("ServiceConfiguration - cpus validation fails with invalid string", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["web1.example.com"],
    cpus: "invalid",
  }, "myproject");

  assertThrows(
    () => service.cpus,
    ConfigurationError,
    "'cpus' for service 'web' must be a positive number or numeric string",
  );
});

Deno.test("ServiceConfiguration - memory with valid formats", () => {
  const formats = ["512m", "1g", "2gb", "1024mb", "2G", "512M"];

  for (const mem of formats) {
    const service = new ServiceConfiguration("web", {
      image: "nginx:latest",
      hosts: ["web1.example.com"],
      memory: mem,
    }, "myproject");

    assertEquals(service.memory, mem);
  }
});

Deno.test("ServiceConfiguration - memory validation fails with invalid format", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["web1.example.com"],
    memory: "512",
  }, "myproject");

  assertThrows(
    () => service.memory,
    ConfigurationError,
    "'memory' for service 'web' must be a string with format: number + unit",
  );
});

Deno.test("ServiceConfiguration - memory validation fails with invalid unit", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["web1.example.com"],
    memory: "512x",
  }, "myproject");

  assertThrows(
    () => service.memory,
    ConfigurationError,
    "'memory' for service 'web' must be a string with format: number + unit",
  );
});

Deno.test("ServiceConfiguration - gpus with valid formats", () => {
  const formats = ["all", "0", "0,1", "device=0", "device=0,1"];

  for (const gpu of formats) {
    const service = new ServiceConfiguration("ml", {
      image: "tensorflow:latest",
      hosts: ["gpu1.example.com"],
      gpus: gpu,
    }, "myproject");

    assertEquals(service.gpus, gpu);
  }
});

Deno.test("ServiceConfiguration - gpus validation fails with non-string", () => {
  const service = new ServiceConfiguration("ml", {
    image: "tensorflow:latest",
    hosts: ["gpu1.example.com"],
    gpus: 123,
  }, "myproject");

  assertThrows(
    () => service.gpus,
    ConfigurationError,
    "'gpus' for service 'ml' must be a string",
  );
});

Deno.test("ServiceConfiguration - devices as string array", () => {
  const service = new ServiceConfiguration("media", {
    image: "ffmpeg:latest",
    hosts: ["media1.example.com"],
    devices: ["/dev/video0", "/dev/snd"],
  }, "myproject");

  assertEquals(service.devices, ["/dev/video0", "/dev/snd"]);
});

Deno.test("ServiceConfiguration - devices with mount paths", () => {
  const service = new ServiceConfiguration("media", {
    image: "ffmpeg:latest",
    hosts: ["media1.example.com"],
    devices: ["/dev/video0:/dev/video0:rwm", "/dev/snd:/dev/snd"],
  }, "myproject");

  assertEquals(service.devices, [
    "/dev/video0:/dev/video0:rwm",
    "/dev/snd:/dev/snd",
  ]);
});

Deno.test("ServiceConfiguration - devices defaults to empty array", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["web1.example.com"],
  }, "myproject");

  assertEquals(service.devices, []);
});

Deno.test("ServiceConfiguration - complete resource constraints configuration", () => {
  const service = new ServiceConfiguration("ml", {
    image: "tensorflow:latest",
    hosts: ["gpu1.example.com"],
    cpus: 4,
    memory: "8g",
    gpus: "all",
    devices: ["/dev/nvidia0", "/dev/nvidiactl"],
  }, "myproject");

  assertEquals(service.cpus, 4);
  assertEquals(service.memory, "8g");
  assertEquals(service.gpus, "all");
  assertEquals(service.devices, ["/dev/nvidia0", "/dev/nvidiactl"]);
});

// Privileged and capabilities tests
Deno.test("ServiceConfiguration - privileged defaults to false", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["web1.example.com"],
  }, "myproject");

  assertEquals(service.privileged, false);
});

Deno.test("ServiceConfiguration - privileged set to true", () => {
  const service = new ServiceConfiguration("fuse", {
    image: "fuse-app:latest",
    hosts: ["fuse1.example.com"],
    privileged: true,
  }, "myproject");

  assertEquals(service.privileged, true);
});

Deno.test("ServiceConfiguration - privileged set to false explicitly", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["web1.example.com"],
    privileged: false,
  }, "myproject");

  assertEquals(service.privileged, false);
});

Deno.test("ServiceConfiguration - privileged validation fails with non-boolean", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["web1.example.com"],
    privileged: "yes",
  }, "myproject");

  assertThrows(
    () => service.privileged,
    ConfigurationError,
    "'privileged' for service 'web' must be a boolean",
  );
});

Deno.test("ServiceConfiguration - cap_add with single capability", () => {
  const service = new ServiceConfiguration("fuse", {
    image: "fuse-app:latest",
    hosts: ["fuse1.example.com"],
    cap_add: ["SYS_ADMIN"],
  }, "myproject");

  assertEquals(service.cap_add, ["SYS_ADMIN"]);
});

Deno.test("ServiceConfiguration - cap_add with multiple capabilities", () => {
  const service = new ServiceConfiguration("network", {
    image: "network-app:latest",
    hosts: ["net1.example.com"],
    cap_add: ["SYS_ADMIN", "NET_ADMIN", "NET_RAW"],
  }, "myproject");

  assertEquals(service.cap_add, ["SYS_ADMIN", "NET_ADMIN", "NET_RAW"]);
});

Deno.test("ServiceConfiguration - cap_add defaults to empty array", () => {
  const service = new ServiceConfiguration("web", {
    image: "nginx:latest",
    hosts: ["web1.example.com"],
  }, "myproject");

  assertEquals(service.cap_add, []);
});

Deno.test("ServiceConfiguration - complete configuration with privileged and cap_add", () => {
  const service = new ServiceConfiguration("fuse-storage", {
    image: "rclone:latest",
    hosts: ["storage1.example.com"],
    privileged: true,
    cap_add: ["SYS_ADMIN"],
    devices: ["/dev/fuse"],
    memory: "2g",
  }, "myproject");

  assertEquals(service.privileged, true);
  assertEquals(service.cap_add, ["SYS_ADMIN"]);
  assertEquals(service.devices, ["/dev/fuse"]);
  assertEquals(service.memory, "2g");
});
