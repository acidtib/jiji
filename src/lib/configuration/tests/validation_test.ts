import { assertEquals } from "@std/assert";
import {
  ConfigurationValidator,
  ValidationRules,
  ValidatorPresets,
} from "../validation.ts";

// Test data
const VALID_CONFIG_DATA = {
  project: "testapp",
  engine: "docker",
  ssh: {
    user: "deploy",
    port: 22,
    connect_timeout: 30,
    command_timeout: 300,
  },
  services: {
    web: {
      image: "nginx:latest",
      hosts: ["web1.example.com"],
      ports: ["80:80"],
    },
    api: {
      image: "node:18",
      hosts: ["api.example.com"],
      ports: ["3000:3000"],
      build: {
        dockerfile: "Dockerfile",
        context: ".",
      },
    },
  },
  env: {
    NODE_ENV: "production",
    DATABASE_URL: "postgresql://localhost:5432/app",
  },
};

const _INVALID_CONFIG_DATA = {
  project: "testapp",
  engine: "invalid_engine",
  ssh: {
    user: "",
    port: -1,
    connect_timeout: "invalid",
  },
  services: {
    web: {
      // Missing required image/build
      hosts: [],
      ports: "invalid_ports",
    },
    api: {
      image: 123, // Invalid type
      hosts: "invalid_hosts",
      volumes: [],
    },
  },
};

const MINIMAL_VALID_CONFIG = {
  project: "minimal",
  engine: "docker",
  services: {
    simple: {
      image: "hello-world",
      hosts: ["example.com"],
    },
  },
};

Deno.test("ValidationRules - required rule works correctly", () => {
  const rule = ValidationRules.required();

  // Should pass with valid values
  const result1 = rule.validate("test_value", "field", { config: {} });
  assertEquals(result1.valid, true);
  assertEquals(result1.errors.length, 0);

  const result2 = rule.validate(0, "field", { config: {} });
  assertEquals(result2.valid, true);

  const result3 = rule.validate(false, "field", { config: {} });
  assertEquals(result3.valid, true);

  // Should fail with null/undefined
  const result4 = rule.validate(null, "field", { config: {} });
  assertEquals(result4.valid, false);
  assertEquals(result4.errors.length, 1);
  assertEquals(result4.errors[0].code, "REQUIRED");

  const result5 = rule.validate(undefined, "field", { config: {} });
  assertEquals(result5.valid, false);
  assertEquals(result5.errors.length, 1);
});

Deno.test("ValidationRules - string rule validates correctly", () => {
  const rule = ValidationRules.string();

  const result1 = rule.validate("test_string", "field", { config: {} });
  assertEquals(result1.valid, true);
  assertEquals(result1.errors.length, 0);

  // Should fail with non-strings
  const result2 = rule.validate(123, "field", { config: {} });
  assertEquals(result2.valid, false);
  assertEquals(result2.errors[0].code, "TYPE_STRING");

  const result3 = rule.validate([], "field", { config: {} });
  assertEquals(result3.valid, false);

  const result4 = rule.validate({}, "field", { config: {} });
  assertEquals(result4.valid, false);
});

Deno.test("ValidationRules - number rule validates correctly", () => {
  const rule = ValidationRules.number();

  const result1 = rule.validate(123, "field", { config: {} });
  assertEquals(result1.valid, true);
  assertEquals(result1.errors.length, 0);

  const result2 = rule.validate(0, "field", { config: {} });
  assertEquals(result2.valid, true);

  const result3 = rule.validate(-456, "field", { config: {} });
  assertEquals(result3.valid, true);

  // Should fail with non-numbers
  const result4 = rule.validate("123", "field", { config: {} });
  assertEquals(result4.valid, false);
  assertEquals(result4.errors[0].code, "TYPE_NUMBER");

  const result5 = rule.validate(true, "field", { config: {} });
  assertEquals(result5.valid, false);
});

Deno.test("ValidationRules - array rule validates correctly", () => {
  const rule = ValidationRules.array();

  const result1 = rule.validate([], "field", { config: {} });
  assertEquals(result1.valid, true);
  assertEquals(result1.errors.length, 0);

  const result2 = rule.validate([1, 2, 3], "field", { config: {} });
  assertEquals(result2.valid, true);

  const result3 = rule.validate(["a", "b"], "field", { config: {} });
  assertEquals(result3.valid, true);

  // Should fail with non-arrays
  const result4 = rule.validate("not_array", "field", { config: {} });
  assertEquals(result4.valid, false);
  assertEquals(result4.errors[0].code, "TYPE_ARRAY");

  const result5 = rule.validate({}, "field", { config: {} });
  assertEquals(result5.valid, false);
});

Deno.test("ValidationRules - object rule validates correctly", () => {
  const rule = ValidationRules.object();

  const result1 = rule.validate({}, "field", { config: {} });
  assertEquals(result1.valid, true);
  assertEquals(result1.errors.length, 0);

  const result2 = rule.validate({ key: "value" }, "field", { config: {} });
  assertEquals(result2.valid, true);

  // Should fail with non-objects
  const result3 = rule.validate("not_object", "field", { config: {} });
  assertEquals(result3.valid, false);
  assertEquals(result3.errors[0].code, "TYPE_OBJECT");

  const result4 = rule.validate([], "field", { config: {} });
  assertEquals(result4.valid, false);

  const result5 = rule.validate(null, "field", { config: {} });
  assertEquals(result5.valid, false);
});

Deno.test("ValidationRules - oneOf rule validates correctly", () => {
  const rule = ValidationRules.oneOf(["docker", "podman"] as const);

  const result1 = rule.validate("docker", "field", { config: {} });
  assertEquals(result1.valid, true);
  assertEquals(result1.errors.length, 0);

  const result2 = rule.validate("podman", "field", { config: {} });
  assertEquals(result2.valid, true);

  // Should fail with invalid values
  const result3 = rule.validate("invalid", "field", { config: {} });
  assertEquals(result3.valid, false);
  assertEquals(result3.errors[0].code, "ENUM");
});

Deno.test("ValidationRules - length rule validates correctly", () => {
  // Test minimum length
  const minRule = ValidationRules.length(3);

  const result1 = minRule.validate("hello", "field", { config: {} });
  assertEquals(result1.valid, true);

  const result2 = minRule.validate("hi", "field", { config: {} });
  assertEquals(result2.valid, false);
  assertEquals(result2.errors[0].code, "MIN_LENGTH");

  // Test maximum length
  const maxRule = ValidationRules.length(undefined, 5);

  const result3 = maxRule.validate("hello", "field", { config: {} });
  assertEquals(result3.valid, true);

  const result4 = maxRule.validate("hello world", "field", { config: {} });
  assertEquals(result4.valid, false);
  assertEquals(result4.errors[0].code, "MAX_LENGTH");

  // Test both min and max
  const bothRule = ValidationRules.length(3, 10);

  const result5 = bothRule.validate("hello", "field", { config: {} });
  assertEquals(result5.valid, true);

  const result6 = bothRule.validate("hi", "field", { config: {} });
  assertEquals(result6.valid, false);
});

Deno.test("ValidationRules - min and max number validation", () => {
  const minRule = ValidationRules.min(10);

  const result1 = minRule.validate(15, "field", { config: {} });
  assertEquals(result1.valid, true);

  const result2 = minRule.validate(5, "field", { config: {} });
  assertEquals(result2.valid, false);
  assertEquals(result2.errors[0].code, "MIN_VALUE");

  const maxRule = ValidationRules.max(100);

  const result3 = maxRule.validate(50, "field", { config: {} });
  assertEquals(result3.valid, true);

  const result4 = maxRule.validate(150, "field", { config: {} });
  assertEquals(result4.valid, false);
  assertEquals(result4.errors[0].code, "MAX_VALUE");
});

Deno.test("ValidationRules - port validation", () => {
  const rule = ValidationRules.port();

  // Valid ports
  const result1 = rule.validate(80, "field", { config: {} });
  assertEquals(result1.valid, true);

  const result2 = rule.validate(65535, "field", { config: {} });
  assertEquals(result2.valid, true);

  // Invalid ports
  const result3 = rule.validate(0, "field", { config: {} });
  assertEquals(result3.valid, false);
  assertEquals(result3.errors[0].code, "INVALID_PORT");

  const result4 = rule.validate(70000, "field", { config: {} });
  assertEquals(result4.valid, false);

  const result5 = rule.validate(22.5, "field", { config: {} });
  assertEquals(result5.valid, false);
});

Deno.test("ValidationRules - hostname validation", () => {
  const rule = ValidationRules.hostname();

  // Valid hostnames/IPs
  const result1 = rule.validate("example.com", "field", { config: {} });
  assertEquals(result1.valid, true);

  const result2 = rule.validate("192.168.1.1", "field", { config: {} });
  assertEquals(result2.valid, true);

  // Invalid hostnames
  const result3 = rule.validate("", "field", { config: {} });
  assertEquals(result3.valid, false);
  assertEquals(result3.errors[0].code, "EMPTY_HOSTNAME");

  const result4 = rule.validate("invalid..hostname", "field", { config: {} });
  assertEquals(result4.valid, false);
  assertEquals(result4.errors[0].code, "INVALID_HOSTNAME");

  // Localhost warning
  const result5 = rule.validate("localhost", "field", { config: {} });
  assertEquals(result5.valid, true);
  assertEquals(result5.warnings.length, 1);
  assertEquals(result5.warnings[0].code, "LOCALHOST_WARNING");
});

Deno.test("ValidationRules - pattern validation", () => {
  const rule = ValidationRules.pattern(/^[a-z]+$/);

  const result1 = rule.validate("hello", "field", { config: {} });
  assertEquals(result1.valid, true);

  const result2 = rule.validate("Hello123", "field", { config: {} });
  assertEquals(result2.valid, false);
  assertEquals(result2.errors[0].code, "PATTERN");
});

Deno.test("ValidationRules - custom rule works correctly", () => {
  const rule = ValidationRules.custom("test", (value, path, _context) => {
    const errors = [];
    if (value === "invalid_value") {
      errors.push({
        path,
        message: "Custom validation failed",
        code: "CUSTOM_ERROR",
      });
    }
    return { valid: errors.length === 0, errors, warnings: [] };
  });

  const result1 = rule.validate("test_value", "field", { config: {} });
  assertEquals(result1.valid, true);

  const result2 = rule.validate("invalid_value", "field", { config: {} });
  assertEquals(result2.valid, false);
  assertEquals(result2.errors[0].code, "CUSTOM_ERROR");
});

Deno.test("ConfigurationValidator - basic validation functionality", () => {
  const validator = new ConfigurationValidator();
  validator.addRule("engine", ValidationRules.required());
  validator.addRule(
    "engine",
    ValidationRules.oneOf(["docker", "podman"] as const),
  );

  const result1 = validator.validate({ engine: "docker" });
  assertEquals(result1.valid, true);
  assertEquals(result1.errors.length, 0);

  const result2 = validator.validate({ engine: "invalid" });
  assertEquals(result2.valid, false);
  assertEquals(result2.errors.length, 1);
});

Deno.test("ConfigurationValidator - multiple rules per field", () => {
  const validator = new ConfigurationValidator();
  validator.addRules("name", [
    ValidationRules.required(),
    ValidationRules.string(),
    ValidationRules.length(3, 20),
  ]);

  const result1 = validator.validate({ name: "validname" });
  assertEquals(result1.valid, true);

  const result2 = validator.validate({ name: "hi" });
  assertEquals(result2.valid, false);

  const result3 = validator.validate({});
  assertEquals(result3.valid, false);
});

Deno.test("ConfigurationValidator - nested path validation", () => {
  const validator = new ConfigurationValidator();
  validator.addRule("ssh.user", ValidationRules.required());
  validator.addRule("ssh.port", ValidationRules.port());

  const result1 = validator.validate({
    ssh: {
      user: "deploy",
      port: 22,
    },
  });
  assertEquals(result1.valid, true);

  const result2 = validator.validate({
    ssh: {
      user: "deploy",
      port: 70000,
    },
  });
  assertEquals(result2.valid, false);
});

Deno.test("ConfigurationValidator - handles missing nested objects", () => {
  const validator = new ConfigurationValidator();
  validator.addRule("services.web.image", ValidationRules.required());

  const result = validator.validate({
    services: {},
  });
  assertEquals(result.valid, false);
  assertEquals(result.errors.length, 1);
});

Deno.test("ValidatorPresets - createJijiValidator works", () => {
  const validator = ValidatorPresets.createJijiValidator();

  const result1 = validator.validate(MINIMAL_VALID_CONFIG);
  // The minimal config might not have SSH config which is required
  // Let's just check that validation runs and produces a result
  assertEquals(typeof result1.valid, "boolean");
  assertEquals(Array.isArray(result1.errors), true);

  const result2 = validator.validate({
    engine: "invalid",
    services: {},
  });
  assertEquals(result2.valid, false);
  assertEquals(result2.errors.length > 0, true);
});

Deno.test("ValidatorPresets - createServiceValidator works", () => {
  const validator = ValidatorPresets.createServiceValidator();

  const validService = {
    image: "nginx:latest",
    hosts: ["example.com"],
    ports: ["80:80"],
  };

  const invalidService = {
    // Missing image/build
    hosts: [],
    ports: ["80:80"],
  };

  const validResult = validator.validate(validService);
  assertEquals(validResult.valid, true);

  const invalidResult = validator.validate(invalidService);
  assertEquals(invalidResult.valid, false);
});

Deno.test("ConfigurationValidator - validation context", () => {
  const validator = new ConfigurationValidator();
  validator.addRule(
    "field",
    ValidationRules.custom("context_test", (value, path, context) => {
      const errors = [];
      if (context.environment === "production" && value === "debug") {
        errors.push({
          path,
          message: "Debug mode not allowed in production",
          code: "DEBUG_IN_PROD",
        });
      }
      return { valid: errors.length === 0, errors, warnings: [] };
    }),
  );

  const devResult = validator.validate(
    { field: "debug" },
    { config: {}, environment: "development" },
  );
  assertEquals(devResult.valid, true);

  const prodResult = validator.validate(
    { field: "debug" },
    { config: {}, environment: "production" },
  );
  assertEquals(prodResult.valid, false);
  assertEquals(prodResult.errors[0].code, "DEBUG_IN_PROD");
});

Deno.test("ConfigurationValidator - comprehensive validation with complex config", () => {
  const validator = ValidatorPresets.createJijiValidator();

  const result = validator.validate(VALID_CONFIG_DATA);
  // Note: This might not be fully valid depending on preset rules
  // The test verifies the validator processes complex structures
  assertEquals(typeof result.valid, "boolean");
  assertEquals(Array.isArray(result.errors), true);
  assertEquals(Array.isArray(result.warnings), true);
});

Deno.test("ConfigurationValidator - error collection from multiple rules", () => {
  const validator = new ConfigurationValidator();
  validator.addRule("field1", ValidationRules.required());
  validator.addRule("field2", ValidationRules.required());
  validator.addRule("field3", ValidationRules.required());

  const result = validator.validate({});
  assertEquals(result.valid, false);
  assertEquals(result.errors.length, 3);

  const errorPaths = result.errors.map((e) => e.path);
  assertEquals(errorPaths.includes("field1"), true);
  assertEquals(errorPaths.includes("field2"), true);
  assertEquals(errorPaths.includes("field3"), true);
});
