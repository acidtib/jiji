import { assertEquals, assertRejects, assertThrows } from "@std/assert";
import { SecretsConfiguration } from "../src/lib/configuration/secrets.ts";
import { ConfigurationError } from "../src/lib/configuration/base.ts";
import {
  createSecretAdapter,
  SUPPORTED_ADAPTERS,
} from "../src/lib/secrets/factory.ts";
import { MockSecretAdapter } from "../src/lib/secrets/testing.ts";
import { resolveSecrets } from "../src/utils/secret_resolver.ts";

// --- SecretsConfiguration tests ---

Deno.test("SecretsConfiguration - parses valid doppler config", () => {
  const config = new SecretsConfiguration({
    adapter: "doppler",
    project: "my-app",
    config: "prd",
  });

  assertEquals(config.adapter, "doppler");
  assertEquals(config.project, "my-app");
  assertEquals(config.configName, "prd");
  assertEquals(config.isConfigured, true);
  config.validate(); // should not throw
});

Deno.test("SecretsConfiguration - minimal config with only adapter", () => {
  const config = new SecretsConfiguration({
    adapter: "doppler",
  });

  assertEquals(config.adapter, "doppler");
  assertEquals(config.project, undefined);
  assertEquals(config.configName, undefined);
  assertEquals(config.isConfigured, true);
  config.validate();
});

Deno.test("SecretsConfiguration - empty config is not configured", () => {
  const config = new SecretsConfiguration({});
  assertEquals(config.isConfigured, false);
  config.validate(); // should not throw when not configured
});

Deno.test("SecretsConfiguration - rejects unsupported adapter", () => {
  const config = new SecretsConfiguration({
    adapter: "vault",
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Unsupported secret adapter",
  );
});

Deno.test("SecretsConfiguration - toAdapterConfig returns correct shape", () => {
  const config = new SecretsConfiguration({
    adapter: "doppler",
    project: "proj",
    config: "stg",
  });

  const adapterConfig = config.toAdapterConfig();
  assertEquals(adapterConfig.adapter, "doppler");
  assertEquals(adapterConfig.project, "proj");
  assertEquals(adapterConfig.config, "stg");
});

// --- Factory tests ---

Deno.test("createSecretAdapter - creates DopplerAdapter for 'doppler'", () => {
  const adapter = createSecretAdapter({
    adapter: "doppler",
    project: "test",
    config: "dev",
  });

  assertEquals(adapter.name, "doppler");
});

Deno.test("createSecretAdapter - throws for unknown adapter", () => {
  assertThrows(
    () => createSecretAdapter({ adapter: "unknown" }),
    Error,
    "Unsupported secret adapter: 'unknown'",
  );
});

Deno.test("SUPPORTED_ADAPTERS contains doppler", () => {
  assertEquals(SUPPORTED_ADAPTERS.includes("doppler"), true);
});

// --- MockSecretAdapter tests ---

Deno.test("MockSecretAdapter - returns configured variables", async () => {
  const mock = new MockSecretAdapter({
    variables: { API_KEY: "secret123", DB_PASS: "pass456" },
  });

  const result = await mock.fetch();
  assertEquals(result.variables.API_KEY, "secret123");
  assertEquals(result.variables.DB_PASS, "pass456");
});

Deno.test("MockSecretAdapter - dependency check passes by default", async () => {
  const mock = new MockSecretAdapter();
  const result = await mock.checkDependencies();
  assertEquals(result.satisfied, true);
});

Deno.test("MockSecretAdapter - dependency check can fail", async () => {
  const mock = new MockSecretAdapter({ dependenciesSatisfied: false });
  const result = await mock.checkDependencies();
  assertEquals(result.satisfied, false);
});

Deno.test("MockSecretAdapter - can simulate fetch error", async () => {
  const mock = new MockSecretAdapter({
    fetchError: new Error("network timeout"),
  });

  await assertRejects(
    () => mock.fetch(),
    Error,
    "network timeout",
  );
});

// --- resolveSecrets tests ---

Deno.test("resolveSecrets - no adapter returns .env-only behavior", async () => {
  // No secretsConfig and no adapterOverride → short-circuit
  const result = await resolveSecrets({
    projectRoot: "/nonexistent/path",
  });

  assertEquals(result.adapterSource, null);
  assertEquals(typeof result.variables, "object");
});

Deno.test("resolveSecrets - adapter variables are included", async () => {
  const mock = new MockSecretAdapter({
    variables: { ADAPTER_SECRET: "from_adapter" },
  });

  const result = await resolveSecrets(
    { projectRoot: "/nonexistent/path" },
    undefined,
    mock,
  );

  assertEquals(result.variables.ADAPTER_SECRET, "from_adapter");
  assertEquals(result.adapterSource, "Mock (mock)");
});

Deno.test("resolveSecrets - .env overrides adapter variables", async () => {
  // Create a temp .env file
  const tmpDir = await Deno.makeTempDir();
  const envPath = `${tmpDir}/.env`;
  await Deno.writeTextFile(
    envPath,
    "SHARED_KEY=from_env\nENV_ONLY=env_value\n",
  );

  const mock = new MockSecretAdapter({
    variables: {
      SHARED_KEY: "from_adapter",
      ADAPTER_ONLY: "adapter_value",
    },
  });

  const result = await resolveSecrets(
    { projectRoot: tmpDir },
    undefined,
    mock,
  );

  // .env wins for shared keys
  assertEquals(result.variables.SHARED_KEY, "from_env");
  // .env-only key is present
  assertEquals(result.variables.ENV_ONLY, "env_value");
  // adapter-only key is present
  assertEquals(result.variables.ADAPTER_ONLY, "adapter_value");

  await Deno.remove(tmpDir, { recursive: true });
});

Deno.test("resolveSecrets - adapter dependency failure throws", async () => {
  const mock = new MockSecretAdapter({ dependenciesSatisfied: false });

  await assertRejects(
    () =>
      resolveSecrets(
        { projectRoot: "/nonexistent/path" },
        undefined,
        mock,
      ),
    Error,
    "dependency check failed",
  );
});

Deno.test("resolveSecrets - adapter fetch error propagates", async () => {
  const mock = new MockSecretAdapter({
    fetchError: new Error("download failed"),
  });

  await assertRejects(
    () =>
      resolveSecrets(
        { projectRoot: "/nonexistent/path" },
        undefined,
        mock,
      ),
    Error,
    "download failed",
  );
});

// --- Backward compatibility ---

Deno.test("Configuration without secrets key works unchanged", () => {
  const config = new SecretsConfiguration({});
  assertEquals(config.isConfigured, false);
  config.validate(); // no-op, no throw
});
