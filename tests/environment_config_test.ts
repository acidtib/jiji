import { assertEquals, assertThrows } from "@std/assert";
import { EnvironmentConfiguration } from "../src/lib/configuration/environment.ts";
import { ConfigurationError } from "../src/lib/configuration/base.ts";

Deno.test("EnvironmentConfiguration - accepts string values", () => {
  const config = new EnvironmentConfiguration({
    clear: {
      STRING_VAR: "hello",
      ANOTHER_STRING: "world",
    },
  });

  config.validate();
  const vars = config.clear;

  assertEquals(vars.STRING_VAR, "hello");
  assertEquals(vars.ANOTHER_STRING, "world");
});

Deno.test("EnvironmentConfiguration - accepts and converts integer values", () => {
  const config = new EnvironmentConfiguration({
    clear: {
      PORT: 8080,
      MAX_CONNECTIONS: 100,
      TIMEOUT: 0,
    },
  });

  config.validate();
  const vars = config.clear;

  assertEquals(vars.PORT, "8080");
  assertEquals(vars.MAX_CONNECTIONS, "100");
  assertEquals(vars.TIMEOUT, "0");
});

Deno.test("EnvironmentConfiguration - accepts and converts boolean values", () => {
  const config = new EnvironmentConfiguration({
    clear: {
      DEBUG: true,
      ENABLED: false,
      IS_PRODUCTION: true,
    },
  });

  config.validate();
  const vars = config.clear;

  assertEquals(vars.DEBUG, "true");
  assertEquals(vars.ENABLED, "false");
  assertEquals(vars.IS_PRODUCTION, "true");
});

Deno.test("EnvironmentConfiguration - accepts mixed types", () => {
  const config = new EnvironmentConfiguration({
    clear: {
      APP_NAME: "myapp",
      PORT: 3000,
      DEBUG: true,
      MAX_RETRIES: 5,
      VERBOSE: false,
    },
  });

  config.validate();
  const vars = config.clear;

  assertEquals(vars.APP_NAME, "myapp");
  assertEquals(vars.PORT, "3000");
  assertEquals(vars.DEBUG, "true");
  assertEquals(vars.MAX_RETRIES, "5");
  assertEquals(vars.VERBOSE, "false");
});

Deno.test("EnvironmentConfiguration - rejects invalid types", () => {
  const config = new EnvironmentConfiguration({
    clear: {
      INVALID: { nested: "object" },
    },
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Environment variable 'INVALID' must be a string, number, or boolean",
  );
});

Deno.test("EnvironmentConfiguration - rejects array values", () => {
  const config = new EnvironmentConfiguration({
    clear: {
      INVALID: ["array", "value"],
    },
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Environment variable 'INVALID' must be a string, number, or boolean",
  );
});

Deno.test("EnvironmentConfiguration - rejects null values", () => {
  const config = new EnvironmentConfiguration({
    clear: {
      INVALID: null,
    },
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Environment variable 'INVALID' must be a string, number, or boolean",
  );
});

Deno.test("EnvironmentConfiguration - validates variable names", () => {
  const config = new EnvironmentConfiguration({
    clear: {
      "invalid-name": "value",
    },
  });

  assertThrows(
    () => config.validate(),
    ConfigurationError,
    "Invalid environment variable name 'invalid-name'",
  );
});

Deno.test("EnvironmentConfiguration - toEnvArray converts all values to strings", () => {
  const config = new EnvironmentConfiguration({
    clear: {
      STRING: "hello",
      NUMBER: 42,
      BOOLEAN: true,
    },
  });

  const envArray = config.toEnvArray();

  assertEquals(envArray.includes("STRING=hello"), true);
  assertEquals(envArray.includes("NUMBER=42"), true);
  assertEquals(envArray.includes("BOOLEAN=true"), true);
});

Deno.test("EnvironmentConfiguration - merge preserves type conversion", () => {
  const config1 = new EnvironmentConfiguration({
    clear: {
      PORT: 8080,
      DEBUG: true,
    },
  });

  const config2 = new EnvironmentConfiguration({
    clear: {
      HOST: "localhost",
      VERBOSE: false,
    },
  });

  const merged = config1.merge(config2);
  const vars = merged.clear;

  assertEquals(vars.PORT, "8080");
  assertEquals(vars.DEBUG, "true");
  assertEquals(vars.HOST, "localhost");
  assertEquals(vars.VERBOSE, "false");
});

Deno.test("EnvironmentConfiguration - empty configuration", () => {
  const config = EnvironmentConfiguration.empty();

  config.validate();
  const vars = config.clear;

  assertEquals(Object.keys(vars).length, 0);
});

Deno.test("EnvironmentConfiguration - resolveVariables includes clear vars", () => {
  const config = new EnvironmentConfiguration({
    clear: {
      PORT: 3000,
      DEBUG: true,
      NAME: "app",
    },
  });

  const resolved = config.resolveVariables();

  assertEquals(resolved.PORT, "3000");
  assertEquals(resolved.DEBUG, "true");
  assertEquals(resolved.NAME, "app");
});

Deno.test("EnvironmentConfiguration - handles negative numbers", () => {
  const config = new EnvironmentConfiguration({
    clear: {
      OFFSET: -10,
      TEMPERATURE: -273.15,
    },
  });

  config.validate();
  const vars = config.clear;

  assertEquals(vars.OFFSET, "-10");
  assertEquals(vars.TEMPERATURE, "-273.15");
});

Deno.test("EnvironmentConfiguration - handles zero values", () => {
  const config = new EnvironmentConfiguration({
    clear: {
      COUNT: 0,
      ENABLED: false,
      EMPTY: "",
    },
  });

  config.validate();
  const vars = config.clear;

  assertEquals(vars.COUNT, "0");
  assertEquals(vars.ENABLED, "false");
  assertEquals(vars.EMPTY, "");
});
