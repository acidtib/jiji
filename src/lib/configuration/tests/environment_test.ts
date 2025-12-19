import { assertEquals, assertThrows } from "@std/assert";
import { EnvironmentConfiguration } from "../environment.ts";
import { ConfigurationError } from "../base.ts";

// Test data
const MINIMAL_ENV_DATA = {};

const COMPLETE_ENV_DATA = {
  clear: {
    NODE_ENV: "production",
    DATABASE_URL: "postgres://localhost:5432/mydb",
    API_KEY: "secret123",
    PORT: "3000",
    DEBUG: "false",
    REDIS_URL: "redis://localhost:6379",
    JWT_SECRET: "super-secret-key",
    LOG_LEVEL: "info",
  },
};

const SECRETS_ENV_DATA = {
  secrets: ["DATABASE_PASSWORD", "API_SECRET", "JWT_SECRET"],
};

Deno.test("EnvironmentConfiguration - minimal configuration", () => {
  const env = new EnvironmentConfiguration(MINIMAL_ENV_DATA);

  assertEquals(Object.keys(env.clear).length, 0);
  assertEquals(env.secrets.length, 0);
});

Deno.test("EnvironmentConfiguration - complete configuration", () => {
  const env = new EnvironmentConfiguration(COMPLETE_ENV_DATA);

  assertEquals(env.clear.NODE_ENV, "production");
  assertEquals(env.clear.DATABASE_URL, "postgres://localhost:5432/mydb");
  assertEquals(env.clear.API_KEY, "secret123");
  assertEquals(env.clear.PORT, "3000");
  assertEquals(env.clear.DEBUG, "false");
  assertEquals(Object.keys(env.clear).length, 8);
});

Deno.test("EnvironmentConfiguration - secrets configuration", () => {
  const env = new EnvironmentConfiguration(SECRETS_ENV_DATA);

  assertEquals(env.secrets.length, 3);
  assertEquals(env.secrets.includes("DATABASE_PASSWORD"), true);
  assertEquals(env.secrets.includes("API_SECRET"), true);
  assertEquals(env.secrets.includes("JWT_SECRET"), true);
});

Deno.test("EnvironmentConfiguration - combined configuration", () => {
  const combinedData = {
    clear: {
      NODE_ENV: "production",
      PORT: "3000",
    },
    secrets: ["DATABASE_PASSWORD"],
  };

  const env = new EnvironmentConfiguration(combinedData);

  assertEquals(env.clear.NODE_ENV, "production");
  assertEquals(env.secrets.includes("DATABASE_PASSWORD"), true);
});

Deno.test("EnvironmentConfiguration - toEnvArray method", () => {
  const env = new EnvironmentConfiguration(COMPLETE_ENV_DATA);
  const envArray = env.toEnvArray();

  assertEquals(Array.isArray(envArray), true);
  assertEquals(envArray.includes("NODE_ENV=production"), true);
  assertEquals(envArray.includes("PORT=3000"), true);
  assertEquals(envArray.length, 8);
});

Deno.test("EnvironmentConfiguration - validation passes for valid environment", () => {
  const env = new EnvironmentConfiguration(COMPLETE_ENV_DATA);

  // Should not throw
  env.validate();
});

Deno.test("EnvironmentConfiguration - validation handles empty environment", () => {
  const env = new EnvironmentConfiguration({});

  // Should not throw
  env.validate();
});

Deno.test("EnvironmentConfiguration - validation fails with invalid variable name", () => {
  const invalidData = {
    clear: {
      "INVALID-NAME": "value", // Hyphens not allowed
    },
  };
  const env = new EnvironmentConfiguration(invalidData);

  assertThrows(
    () => env.validate(),
    ConfigurationError,
    "Invalid environment variable name 'INVALID-NAME'",
  );
});

Deno.test("EnvironmentConfiguration - toObject method", () => {
  const env = new EnvironmentConfiguration(COMPLETE_ENV_DATA);
  const obj = env.toObject();

  assertEquals(obj.clear, {
    NODE_ENV: "production",
    DATABASE_URL: "postgres://localhost:5432/mydb",
    API_KEY: "secret123",
    PORT: "3000",
    DEBUG: "false",
    REDIS_URL: "redis://localhost:6379",
    JWT_SECRET: "super-secret-key",
    LOG_LEVEL: "info",
  });
});

Deno.test("EnvironmentConfiguration - toObject returns empty object for empty env", () => {
  const env = new EnvironmentConfiguration({});
  const obj = env.toObject();

  assertEquals(obj, {});
});

Deno.test("EnvironmentConfiguration - toObject with secrets", () => {
  const combinedData = {
    clear: { NODE_ENV: "test" },
    secrets: ["SECRET_KEY"],
  };
  const env = new EnvironmentConfiguration(combinedData);
  const obj = env.toObject();

  assertEquals(obj.clear, { NODE_ENV: "test" });
  assertEquals(obj.secrets, ["SECRET_KEY"]);
});

Deno.test("EnvironmentConfiguration - empty static method", () => {
  const env = EnvironmentConfiguration.empty();

  assertEquals(Object.keys(env.clear).length, 0);
  assertEquals(env.secrets.length, 0);
});

Deno.test("EnvironmentConfiguration - merge functionality", () => {
  const env1 = new EnvironmentConfiguration({
    clear: { VAR1: "value1", VAR2: "value2" },
    secrets: ["SECRET1"],
  });

  const env2 = new EnvironmentConfiguration({
    clear: { VAR2: "updated_value2", VAR3: "value3" },
    secrets: ["SECRET2"],
  });

  const merged = env1.merge(env2);

  assertEquals(merged.clear.VAR1, "value1");
  assertEquals(merged.clear.VAR2, "updated_value2"); // Overridden
  assertEquals(merged.clear.VAR3, "value3");
  assertEquals(merged.secrets.length, 2);
  assertEquals(merged.secrets.includes("SECRET1"), true);
  assertEquals(merged.secrets.includes("SECRET2"), true);
});

Deno.test("EnvironmentConfiguration - validation fails with invalid secret name", () => {
  const invalidData = {
    secrets: ["INVALID-SECRET"], // Hyphens not allowed
  };
  const env = new EnvironmentConfiguration(invalidData);

  assertThrows(
    () => env.validate(),
    ConfigurationError,
    "Invalid secret name 'INVALID-SECRET'",
  );
});

Deno.test("EnvironmentConfiguration - validation fails with empty secret", () => {
  const invalidData = {
    secrets: [""], // Empty secret name
  };
  const env = new EnvironmentConfiguration(invalidData);

  assertThrows(
    () => env.validate(),
    ConfigurationError,
    "must be a non-empty string",
  );
});

Deno.test("EnvironmentConfiguration - resolveVariables with secrets", () => {
  // Set environment variables for testing
  Deno.env.set("TEST_SECRET_1", "secret_value_1");
  Deno.env.set("TEST_SECRET_2", "secret_value_2");

  try {
    const env = new EnvironmentConfiguration({
      clear: {
        VAR1: "value1",
        VAR2: "value2",
      },
      secrets: ["TEST_SECRET_1", "TEST_SECRET_2"],
    });

    const resolved = env.resolveVariables();

    assertEquals(resolved.VAR1, "value1");
    assertEquals(resolved.VAR2, "value2");
    assertEquals(resolved.TEST_SECRET_1, "secret_value_1");
    assertEquals(resolved.TEST_SECRET_2, "secret_value_2");
    assertEquals(Object.keys(resolved).length, 4);
  } finally {
    Deno.env.delete("TEST_SECRET_1");
    Deno.env.delete("TEST_SECRET_2");
  }
});

Deno.test("EnvironmentConfiguration - resolveVariables skips undefined secrets", () => {
  const env = new EnvironmentConfiguration({
    clear: {
      VAR1: "value1",
    },
    secrets: ["NONEXISTENT_SECRET"],
  });

  const resolved = env.resolveVariables();

  assertEquals(resolved.VAR1, "value1");
  assertEquals(resolved.NONEXISTENT_SECRET, undefined);
  assertEquals(Object.keys(resolved).length, 1);
});
