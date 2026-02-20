import { assertEquals, assertThrows } from "@std/assert";
import { BuilderConfiguration } from "../builder.ts";
import { ConfigurationError } from "../base.ts";

// Test data
const MINIMAL_BUILDER_DATA = {
  local: true,
};

const COMPLETE_LOCAL_BUILDER_DATA = {
  engine: "docker",
  local: true,
  cache: true,
  registry: {
    type: "local",
    port: 31270,
  },
};

const REMOTE_BUILDER_DATA = {
  engine: "podman",
  local: false,
  remote: "ssh://builder@192.168.1.50:22",
  registry: {
    type: "local",
    port: 31270,
  },
};

const REMOTE_REGISTRY_BUILDER_DATA = {
  engine: "docker",
  local: true,
  registry: {
    type: "remote",
    server: "registry.example.com:5000",
    username: "myuser",
    password: "secret",
  },
};

const INVALID_REMOTE_URI_DATA = {
  local: false,
  remote: "invalid-uri",
};

const BOTH_LOCAL_AND_REMOTE_DATA = {
  local: true,
  remote: "ssh://builder@host:22",
};

Deno.test("BuilderConfiguration - minimal configuration", () => {
  const builder = new BuilderConfiguration(MINIMAL_BUILDER_DATA);

  assertEquals(builder.local, true);
  assertEquals(builder.remote, undefined);
  assertEquals(builder.cache, true);
  assertEquals(builder.registry.type, "local");
  assertEquals(builder.registry.port, 31270);
});

Deno.test("BuilderConfiguration - complete local builder", () => {
  const builder = new BuilderConfiguration(COMPLETE_LOCAL_BUILDER_DATA);

  assertEquals(builder.engine, "docker");
  assertEquals(builder.local, true);
  assertEquals(builder.remote, undefined);
  assertEquals(builder.cache, true);
  assertEquals(builder.isLocalBuild(), true);
});

Deno.test("BuilderConfiguration - remote builder", () => {
  const builder = new BuilderConfiguration(REMOTE_BUILDER_DATA);

  assertEquals(builder.engine, "podman");
  assertEquals(builder.local, false);
  assertEquals(builder.remote, "ssh://builder@192.168.1.50:22");
  assertEquals(builder.isLocalBuild(), false);

  const remoteHost = builder.getRemoteHost();
  assertEquals(remoteHost?.user, "builder");
  assertEquals(remoteHost?.host, "192.168.1.50");
  assertEquals(remoteHost?.port, 22);
});

Deno.test("BuilderConfiguration - remote registry", () => {
  const builder = new BuilderConfiguration(REMOTE_REGISTRY_BUILDER_DATA);

  assertEquals(builder.registry.type, "remote");
  assertEquals(builder.registry.server, "registry.example.com:5000");
  assertEquals(builder.registry.username, "myuser");
  assertEquals(builder.registry.password, "secret");
  assertEquals(builder.registry.isLocal(), false);
});

Deno.test("BuilderConfiguration - remote host parsing with defaults", () => {
  const builder = new BuilderConfiguration({
    local: false,
    remote: "ssh://192.168.1.50",
  });

  const remoteHost = builder.getRemoteHost();
  assertEquals(remoteHost?.user, "root");
  assertEquals(remoteHost?.host, "192.168.1.50");
  assertEquals(remoteHost?.port, 22);
});

Deno.test("BuilderConfiguration - engine validation", () => {
  assertThrows(
    () => {
      const builder = new BuilderConfiguration({ engine: "invalid" });
      // Access the engine property to trigger validation
      builder.engine;
    },
    ConfigurationError,
    "Invalid value for 'engine'",
  );
});

Deno.test("BuilderConfiguration - invalid remote URI", () => {
  const builder = new BuilderConfiguration(INVALID_REMOTE_URI_DATA);

  assertThrows(
    () => builder.validate(),
    ConfigurationError,
    "Invalid remote builder URI",
  );
});

Deno.test("BuilderConfiguration - cannot be both local and remote", () => {
  const builder = new BuilderConfiguration(BOTH_LOCAL_AND_REMOTE_DATA);

  assertThrows(
    () => builder.validate(),
    ConfigurationError,
    "Builder cannot be both 'local: true' and have a 'remote' configuration",
  );
});

Deno.test("BuilderConfiguration - no-cache option", () => {
  const builder = new BuilderConfiguration({
    local: true,
    cache: false,
  });

  assertEquals(builder.cache, false);
});

Deno.test("BuilderConfiguration - registry defaults to local", () => {
  const builder = new BuilderConfiguration({
    local: true,
  });

  assertEquals(builder.registry.type, "local");
  assertEquals(builder.registry.isLocal(), true);
});

Deno.test("BuilderConfiguration - custom registry port", () => {
  const builder = new BuilderConfiguration({
    local: true,
    registry: {
      type: "local",
      port: 6000,
    },
  });

  assertEquals(builder.registry.port, 6000);
  assertEquals(builder.registry.getRegistryUrl(), "localhost:6000");
});
