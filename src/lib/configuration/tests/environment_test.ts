import { assertEquals, assertThrows } from "@std/assert";
import { EnvironmentConfiguration } from "../environment.ts";
import { ConfigurationError } from "../base.ts";

// Test data
const MINIMAL_ENV_DATA = {};

const COMPLETE_ENV_DATA = {
  variables: {
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

const FILES_ENV_DATA = {
  files: {
    CA_CERT: "/etc/ssl/certs/ca-cert.pem",
    TLS_KEY: "/etc/ssl/private/tls.key",
  },
};

Deno.test("EnvironmentConfiguration - minimal configuration", () => {
  const env = new EnvironmentConfiguration("test", MINIMAL_ENV_DATA);

  assertEquals(env.name, "test");
  assertEquals(Object.keys(env.variables).length, 0);
  assertEquals(env.secrets.length, 0);
  assertEquals(Object.keys(env.files).length, 0);
});

Deno.test("EnvironmentConfiguration - complete configuration", () => {
  const env = new EnvironmentConfiguration("production", COMPLETE_ENV_DATA);

  assertEquals(env.name, "production");
  assertEquals(env.variables.NODE_ENV, "production");
  assertEquals(env.variables.DATABASE_URL, "postgres://localhost:5432/mydb");
  assertEquals(env.variables.API_KEY, "secret123");
  assertEquals(env.variables.PORT, "3000");
  assertEquals(env.variables.DEBUG, "false");
  assertEquals(Object.keys(env.variables).length, 8);
});

Deno.test("EnvironmentConfiguration - secrets configuration", () => {
  const env = new EnvironmentConfiguration("test", SECRETS_ENV_DATA);

  assertEquals(env.secrets.length, 3);
  assertEquals(env.secrets.includes("DATABASE_PASSWORD"), true);
  assertEquals(env.secrets.includes("API_SECRET"), true);
  assertEquals(env.secrets.includes("JWT_SECRET"), true);
});

Deno.test("EnvironmentConfiguration - files configuration", () => {
  const env = new EnvironmentConfiguration("test", FILES_ENV_DATA);

  assertEquals(Object.keys(env.files).length, 2);
  assertEquals(env.files.CA_CERT, "/etc/ssl/certs/ca-cert.pem");
  assertEquals(env.files.TLS_KEY, "/etc/ssl/private/tls.key");
});

Deno.test("EnvironmentConfiguration - combined configuration", () => {
  const combinedData = {
    variables: {
      NODE_ENV: "production",
      PORT: "3000",
    },
    secrets: ["DATABASE_PASSWORD"],
    files: {
      TLS_CERT: "/etc/ssl/cert.pem",
    },
  };

  const env = new EnvironmentConfiguration("combined", combinedData);

  assertEquals(env.variables.NODE_ENV, "production");
  assertEquals(env.secrets.includes("DATABASE_PASSWORD"), true);
  assertEquals(env.files.TLS_CERT, "/etc/ssl/cert.pem");
});

Deno.test("EnvironmentConfiguration - toEnvArray method", () => {
  const env = new EnvironmentConfiguration("test", COMPLETE_ENV_DATA);
  const envArray = env.toEnvArray();

  assertEquals(Array.isArray(envArray), true);
  assertEquals(envArray.includes("NODE_ENV=production"), true);
  assertEquals(envArray.includes("PORT=3000"), true);
  assertEquals(envArray.length, 8);
});

Deno.test("EnvironmentConfiguration - validation passes for valid environment", () => {
  const env = new EnvironmentConfiguration("test", COMPLETE_ENV_DATA);

  // Should not throw
  env.validate();
});

Deno.test("EnvironmentConfiguration - validation handles empty environment", () => {
  const env = new EnvironmentConfiguration("test", {});

  // Should not throw
  env.validate();
});

Deno.test("EnvironmentConfiguration - validation fails with invalid variable name", () => {
  const invalidData = {
    variables: {
      "INVALID-NAME": "value", // Hyphens not allowed
    },
  };
  const env = new EnvironmentConfiguration("test", invalidData);

  assertThrows(
    () => env.validate(),
    ConfigurationError,
    "Invalid environment variable name 'INVALID-NAME'",
  );
});

Deno.test("EnvironmentConfiguration - toObject method", () => {
  const env = new EnvironmentConfiguration("test", COMPLETE_ENV_DATA);
  const obj = env.toObject();

  assertEquals(obj.variables, {
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
  const env = new EnvironmentConfiguration("test", {});
  const obj = env.toObject();

  assertEquals(obj, {});
});

Deno.test("EnvironmentConfiguration - toObject with secrets and files", () => {
  const combinedData = {
    variables: { NODE_ENV: "test" },
    secrets: ["SECRET_KEY"],
    files: { CERT: "/path/to/cert" },
  };
  const env = new EnvironmentConfiguration("test", combinedData);
  const obj = env.toObject();

  assertEquals(obj.variables, { NODE_ENV: "test" });
  assertEquals(obj.secrets, ["SECRET_KEY"]);
  assertEquals(obj.files, { CERT: "/path/to/cert" });
});

Deno.test("EnvironmentConfiguration - withDefaults static method", () => {
  const env = EnvironmentConfiguration.withDefaults("development");

  assertEquals(env.name, "development");
  assertEquals(Object.keys(env.variables).length, 0);
  assertEquals(env.secrets.length, 0);
  assertEquals(Object.keys(env.files).length, 0);
});

Deno.test("EnvironmentConfiguration - withDefaults accepts overrides", () => {
  const env = EnvironmentConfiguration.withDefaults("production", {
    variables: {
      NODE_ENV: "production",
      DEBUG: "true",
    },
    secrets: ["API_KEY"],
  });

  assertEquals(env.name, "production");
  assertEquals(env.variables.NODE_ENV, "production");
  assertEquals(env.variables.DEBUG, "true");
  assertEquals(env.secrets.includes("API_KEY"), true);
});

Deno.test("EnvironmentConfiguration - merge functionality", () => {
  const env1 = new EnvironmentConfiguration("test", {
    variables: { VAR1: "value1", VAR2: "value2" },
    secrets: ["SECRET1"],
  });

  const env2 = new EnvironmentConfiguration("test", {
    variables: { VAR2: "updated_value2", VAR3: "value3" },
    secrets: ["SECRET2"],
  });

  const merged = env1.merge(env2);

  assertEquals(merged.variables.VAR1, "value1");
  assertEquals(merged.variables.VAR2, "updated_value2"); // Overridden
  assertEquals(merged.variables.VAR3, "value3");
  assertEquals(merged.secrets.length, 2);
  assertEquals(merged.secrets.includes("SECRET1"), true);
  assertEquals(merged.secrets.includes("SECRET2"), true);
});

Deno.test("EnvironmentConfiguration - validation fails with invalid secret name", () => {
  const invalidData = {
    secrets: ["INVALID-SECRET"], // Hyphens not allowed
  };
  const env = new EnvironmentConfiguration("test", invalidData);

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
  const env = new EnvironmentConfiguration("test", invalidData);

  assertThrows(
    () => env.validate(),
    ConfigurationError,
    "Secret name '' in environment 'test' must be a non-empty string",
  );
});

Deno.test("EnvironmentConfiguration - validation fails with empty file path", () => {
  const invalidData = {
    files: {
      CERT_FILE: "", // Empty file path
    },
  };
  const env = new EnvironmentConfiguration("test", invalidData);

  assertThrows(
    () => env.validate(),
    ConfigurationError,
    "File path for 'CERT_FILE' in environment 'test' must be a non-empty string",
  );
});
