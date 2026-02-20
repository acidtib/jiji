import { assertEquals, assertThrows } from "@std/assert";
import { RegistryConfiguration } from "../registry.ts";
import { ConfigurationError } from "../base.ts";

// Test data
const LOCAL_REGISTRY_DATA = {
  type: "local",
  port: 31270,
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

const _REMOTE_REGISTRY_WITH_ENV_PASSWORD_DATA = {
  type: "remote",
  server: "gcr.io:443",
  username: "oauth2accesstoken",
  password: "GCR_TOKEN",
};

const REMOTE_REGISTRY_NO_PORT_DATA = {
  type: "remote",
  server: "ghcr.io",
  username: "myuser",
  password: "secret123",
};

const GHCR_AUTO_NAMESPACE_DATA = {
  type: "remote",
  server: "ghcr.io",
  username: "acidtib",
  password: "secret123",
};

const GHCR_MISSING_USERNAME_DATA = {
  type: "remote",
  server: "ghcr.io",
  password: "secret123",
};

const DOCKER_HUB_AUTO_NAMESPACE_DATA = {
  type: "remote",
  server: "docker.io",
  username: "myuser",
  password: "secret123",
};

const DOCKER_HUB_MISSING_USERNAME_DATA = {
  type: "remote",
  server: "docker.io",
  password: "secret123",
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
  server: "@invalid-server",
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
  assertEquals(registry.port, 31270);
  assertEquals(registry.isLocal(), true);
  assertEquals(registry.getRegistryUrl(), "localhost:31270");
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

Deno.test("RegistryConfiguration - remote registry without port", () => {
  const registry = new RegistryConfiguration(REMOTE_REGISTRY_NO_PORT_DATA);

  assertEquals(registry.type, "remote");
  assertEquals(registry.server, "ghcr.io");
  assertEquals(registry.username, "myuser");
  assertEquals(registry.password, "secret123");
  assertEquals(registry.isLocal(), false);
  assertEquals(registry.getRegistryUrl(), "ghcr.io");
});

Deno.test("RegistryConfiguration - getFullImageName", () => {
  const registry = new RegistryConfiguration(LOCAL_REGISTRY_DATA);

  const imageName = registry.getFullImageName("myproject", "web", "v1.0.0");
  assertEquals(imageName, "localhost:31270/myproject-web:v1.0.0");
});

Deno.test("RegistryConfiguration - getFullImageName with remote registry", () => {
  const registry = new RegistryConfiguration(REMOTE_REGISTRY_DATA);

  const imageName = registry.getFullImageName("myproject", "api", "abc123");
  assertEquals(imageName, "registry.example.com:5000/myproject-api:abc123");
});

Deno.test("RegistryConfiguration - GHCR auto namespace detection", () => {
  const registry = new RegistryConfiguration(GHCR_AUTO_NAMESPACE_DATA);

  const imageName = registry.getFullImageName("myproject", "api", "v1.0.0");
  assertEquals(imageName, "ghcr.io/acidtib/myproject-api:v1.0.0");
});

Deno.test("RegistryConfiguration - GHCR missing username throws error", () => {
  const registry = new RegistryConfiguration(GHCR_MISSING_USERNAME_DATA);

  assertThrows(
    () => registry.getFullImageName("myproject", "api", "v1.0.0"),
    Error,
    "GHCR requires username to be configured",
  );
});

Deno.test("RegistryConfiguration - Docker Hub auto namespace detection", () => {
  const registry = new RegistryConfiguration(DOCKER_HUB_AUTO_NAMESPACE_DATA);

  const imageName = registry.getFullImageName("myproject", "api", "v1.0.0");
  assertEquals(imageName, "docker.io/myuser/myproject-api:v1.0.0");
});

Deno.test("RegistryConfiguration - Docker Hub missing username throws error", () => {
  const registry = new RegistryConfiguration(DOCKER_HUB_MISSING_USERNAME_DATA);

  assertThrows(
    () => registry.getFullImageName("myproject", "api", "v1.0.0"),
    ConfigurationError,
    "Docker Hub requires username to be configured",
  );
});

Deno.test("RegistryConfiguration - password getter returns raw value", () => {
  // Password getter should return raw value without resolving
  const registry = new RegistryConfiguration({
    type: "remote",
    server: "registry.example.com:5000",
    username: "user",
    password: "GCR_TOKEN",
  });

  // Raw value should be returned
  assertEquals(registry.password, "GCR_TOKEN");
});

Deno.test("RegistryConfiguration - resolvePassword with bare VAR_NAME syntax", () => {
  const registry = new RegistryConfiguration({
    type: "remote",
    server: "registry.example.com:5000",
    username: "user",
    password: "REGISTRY_PASSWORD",
  });

  // Resolve using envVars
  const envVars = { REGISTRY_PASSWORD: "my-secret-password" };
  assertEquals(registry.resolvePassword(envVars), "my-secret-password");
});

Deno.test("RegistryConfiguration - resolvePassword with host env fallback", () => {
  Deno.env.set("HOST_REGISTRY_TOKEN", "host-token-value");

  try {
    const registry = new RegistryConfiguration({
      type: "remote",
      server: "registry.example.com:5000",
      username: "user",
      password: "HOST_REGISTRY_TOKEN",
    });

    // Resolve with allowHostEnv=true
    assertEquals(registry.resolvePassword({}, true), "host-token-value");
  } finally {
    Deno.env.delete("HOST_REGISTRY_TOKEN");
  }
});

Deno.test("RegistryConfiguration - resolvePassword with literal value", () => {
  const registry = new RegistryConfiguration({
    type: "remote",
    server: "registry.example.com:5000",
    username: "user",
    password: "my-literal-password",
  });

  // Literal passwords (not ALL_CAPS) should be returned as-is
  assertEquals(registry.resolvePassword({}), "my-literal-password");
});

Deno.test("RegistryConfiguration - resolvePassword throws on missing secret", () => {
  Deno.env.delete("MISSING_TOKEN");

  const registry = new RegistryConfiguration({
    type: "remote",
    server: "registry.example.com:5000",
    username: "user",
    password: "MISSING_TOKEN",
  });

  assertThrows(
    () => registry.resolvePassword({}),
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

Deno.test("RegistryConfiguration - remote registry without port validation passes", () => {
  const registry = new RegistryConfiguration(REMOTE_REGISTRY_NO_PORT_DATA);

  // Should not throw
  registry.validate();
  assertEquals(registry.isLocal(), false);
});

Deno.test("RegistryConfiguration - GHCR auto namespace validation passes", () => {
  const registry = new RegistryConfiguration(GHCR_AUTO_NAMESPACE_DATA);

  // Should not throw
  registry.validate();
  assertEquals(registry.isLocal(), false);
});

Deno.test("RegistryConfiguration - GHCR validation fails without username", () => {
  const registry = new RegistryConfiguration(GHCR_MISSING_USERNAME_DATA);

  assertThrows(
    () => registry.validate(),
    ConfigurationError,
    "GHCR requires username to be configured",
  );
});

Deno.test("RegistryConfiguration - Docker Hub auto namespace validation passes", () => {
  const registry = new RegistryConfiguration(DOCKER_HUB_AUTO_NAMESPACE_DATA);

  // Should not throw
  registry.validate();
  assertEquals(registry.isLocal(), false);
});

Deno.test("RegistryConfiguration - Docker Hub validation fails without username", () => {
  const registry = new RegistryConfiguration(DOCKER_HUB_MISSING_USERNAME_DATA);

  assertThrows(
    () => registry.validate(),
    ConfigurationError,
    "Docker Hub requires username to be configured",
  );
});

Deno.test("RegistryConfiguration - registry URL for different ports", () => {
  const registry1 = new RegistryConfiguration({ type: "local", port: 31270 });
  const registry2 = new RegistryConfiguration({ type: "local", port: 8080 });

  assertEquals(registry1.getRegistryUrl(), "localhost:31270");
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
