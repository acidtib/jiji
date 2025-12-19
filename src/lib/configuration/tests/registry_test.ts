import { assertEquals, assertThrows } from "@std/assert";
import { RegistryConfiguration } from "../registry.ts";
import { ConfigurationError } from "../base.ts";

// Test data
const LOCAL_REGISTRY_DATA = {
  type: "local",
  port: 6767,
};

const LOCAL_REGISTRY_CUSTOM_PORT_DATA = {
  type: "local",
  port: 6000,
};

const REMOTE_REGISTRY_DATA = {
  type: "remote",
  server: "registry.example.com:5000",
  username: "myuser",
  password: "secret123",
};

const REMOTE_REGISTRY_WITH_ENV_PASSWORD_DATA = {
  type: "remote",
  server: "gcr.io:443",
  username: "oauth2accesstoken",
  password: "${GCR_TOKEN}",
};

const INVALID_REGISTRY_TYPE_DATA = {
  type: "invalid",
};

const REMOTE_REGISTRY_MISSING_SERVER_DATA = {
  type: "remote",
  username: "myuser",
  password: "secret",
};

const REMOTE_REGISTRY_MISSING_USERNAME_DATA = {
  type: "remote",
  server: "registry.example.com:5000",
  password: "secret",
};

const REMOTE_REGISTRY_MISSING_PASSWORD_DATA = {
  type: "remote",
  server: "registry.example.com:5000",
  username: "myuser",
};

const INVALID_SERVER_FORMAT_DATA = {
  type: "remote",
  server: "invalid-server-format",
  username: "myuser",
  password: "secret",
};

const INVALID_PORT_DATA = {
  type: "local",
  port: 70000,
};

Deno.test("RegistryConfiguration - local registry with default port", () => {
  const registry = new RegistryConfiguration({});

  assertEquals(registry.type, "local");
  assertEquals(registry.port, 6767);
  assertEquals(registry.isLocal(), true);
  assertEquals(registry.getRegistryUrl(), "localhost:6767");
});

Deno.test("RegistryConfiguration - local registry with custom port", () => {
  const registry = new RegistryConfiguration(LOCAL_REGISTRY_CUSTOM_PORT_DATA);

  assertEquals(registry.type, "local");
  assertEquals(registry.port, 6000);
  assertEquals(registry.isLocal(), true);
  assertEquals(registry.getRegistryUrl(), "localhost:6000");
});

Deno.test("RegistryConfiguration - remote registry", () => {
  const registry = new RegistryConfiguration(REMOTE_REGISTRY_DATA);

  assertEquals(registry.type, "remote");
  assertEquals(registry.server, "registry.example.com:5000");
  assertEquals(registry.username, "myuser");
  assertEquals(registry.password, "secret123");
  assertEquals(registry.isLocal(), false);
  assertEquals(registry.getRegistryUrl(), "registry.example.com:5000");
});

Deno.test("RegistryConfiguration - getFullImageName", () => {
  const registry = new RegistryConfiguration(LOCAL_REGISTRY_DATA);

  const imageName = registry.getFullImageName("myproject", "web", "v1.0.0");
  assertEquals(imageName, "localhost:6767/myproject-web:v1.0.0");
});

Deno.test("RegistryConfiguration - getFullImageName with remote registry", () => {
  const registry = new RegistryConfiguration(REMOTE_REGISTRY_DATA);

  const imageName = registry.getFullImageName("myproject", "api", "abc123");
  assertEquals(imageName, "registry.example.com:5000/myproject-api:abc123");
});

Deno.test("RegistryConfiguration - environment variable substitution", () => {
  // Set environment variable for testing
  Deno.env.set("GCR_TOKEN", "test-token-value");

  try {
    const registry = new RegistryConfiguration(
      REMOTE_REGISTRY_WITH_ENV_PASSWORD_DATA,
    );

    assertEquals(registry.password, "test-token-value");
  } finally {
    // Clean up
    Deno.env.delete("GCR_TOKEN");
  }
});

Deno.test("RegistryConfiguration - environment variable not found", () => {
  // Ensure the env var doesn't exist
  Deno.env.delete("MISSING_TOKEN");

  assertThrows(
    () => {
      const registry = new RegistryConfiguration({
        type: "remote",
        server: "registry.example.com:5000",
        username: "user",
        password: "${MISSING_TOKEN}",
      });
      // Access password to trigger validation
      registry.password;
    },
    ConfigurationError,
    "Environment variable 'MISSING_TOKEN' not found",
  );
});

Deno.test("RegistryConfiguration - invalid registry type", () => {
  assertThrows(
    () => {
      const registry = new RegistryConfiguration(INVALID_REGISTRY_TYPE_DATA);
      registry.validate();
    },
    ConfigurationError,
    "Invalid value for 'type'",
  );
});

Deno.test("RegistryConfiguration - remote registry missing server", () => {
  const registry = new RegistryConfiguration(
    REMOTE_REGISTRY_MISSING_SERVER_DATA,
  );

  assertThrows(
    () => registry.validate(),
    ConfigurationError,
    "Remote registry requires 'server' to be configured",
  );
});

Deno.test("RegistryConfiguration - remote registry missing username", () => {
  const registry = new RegistryConfiguration(
    REMOTE_REGISTRY_MISSING_USERNAME_DATA,
  );

  assertThrows(
    () => registry.validate(),
    ConfigurationError,
    "Remote registry requires 'username' to be configured",
  );
});

Deno.test("RegistryConfiguration - remote registry missing password", () => {
  const registry = new RegistryConfiguration(
    REMOTE_REGISTRY_MISSING_PASSWORD_DATA,
  );

  assertThrows(
    () => registry.validate(),
    ConfigurationError,
    "Remote registry requires 'password' to be configured",
  );
});

Deno.test("RegistryConfiguration - invalid server format", () => {
  const registry = new RegistryConfiguration(INVALID_SERVER_FORMAT_DATA);

  assertThrows(
    () => registry.validate(),
    ConfigurationError,
    "Invalid registry server format",
  );
});

Deno.test("RegistryConfiguration - invalid port number", () => {
  assertThrows(
    () => {
      const registry = new RegistryConfiguration(INVALID_PORT_DATA);
      // Access port property to trigger validation
      registry.port;
    },
    ConfigurationError,
    "must be a valid port number",
  );
});

Deno.test("RegistryConfiguration - port below valid range", () => {
  assertThrows(
    () => {
      const registry = new RegistryConfiguration({
        type: "local",
        port: 0,
      });
      // Access port property to trigger validation
      registry.port;
    },
    ConfigurationError,
    "must be a valid port number",
  );
});

Deno.test("RegistryConfiguration - local registry validation passes", () => {
  const registry = new RegistryConfiguration(LOCAL_REGISTRY_DATA);

  // Should not throw
  registry.validate();
  assertEquals(registry.isLocal(), true);
});

Deno.test("RegistryConfiguration - remote registry validation passes", () => {
  const registry = new RegistryConfiguration(REMOTE_REGISTRY_DATA);

  // Should not throw
  registry.validate();
  assertEquals(registry.isLocal(), false);
});

Deno.test("RegistryConfiguration - registry URL for different ports", () => {
  const registry1 = new RegistryConfiguration({ type: "local", port: 6767 });
  const registry2 = new RegistryConfiguration({ type: "local", port: 8080 });

  assertEquals(registry1.getRegistryUrl(), "localhost:6767");
  assertEquals(registry2.getRegistryUrl(), "localhost:8080");
});

Deno.test("RegistryConfiguration - plain password (not env var)", () => {
  const registry = new RegistryConfiguration({
    type: "remote",
    server: "registry.example.com:5000",
    username: "user",
    password: "plain-password",
  });

  assertEquals(registry.password, "plain-password");
});
