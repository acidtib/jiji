import { assertEquals } from "@std/assert";
import { EnvLoader } from "../env_loader.ts";

Deno.test("EnvLoader.parseEnvFile - basic KEY=value", () => {
  const content = `
KEY1=value1
KEY2=value2
`;
  const result = EnvLoader.parseEnvFile(content);

  assertEquals(result.KEY1, "value1");
  assertEquals(result.KEY2, "value2");
  assertEquals(Object.keys(result).length, 2);
});

Deno.test("EnvLoader.parseEnvFile - double quoted values", () => {
  const content = `
DATABASE_URL="postgres://user:pass@localhost/db"
MESSAGE="Hello World"
`;
  const result = EnvLoader.parseEnvFile(content);

  assertEquals(result.DATABASE_URL, "postgres://user:pass@localhost/db");
  assertEquals(result.MESSAGE, "Hello World");
});

Deno.test("EnvLoader.parseEnvFile - single quoted values", () => {
  const content = `
API_KEY='my-secret-key'
PATH='/usr/local/bin'
`;
  const result = EnvLoader.parseEnvFile(content);

  assertEquals(result.API_KEY, "my-secret-key");
  assertEquals(result.PATH, "/usr/local/bin");
});

Deno.test("EnvLoader.parseEnvFile - handles comments", () => {
  const content = `
# This is a comment
KEY1=value1
# Another comment
KEY2=value2
`;
  const result = EnvLoader.parseEnvFile(content);

  assertEquals(result.KEY1, "value1");
  assertEquals(result.KEY2, "value2");
  assertEquals(Object.keys(result).length, 2);
});

Deno.test("EnvLoader.parseEnvFile - handles inline comments for unquoted", () => {
  const content = `
KEY1=value1 # inline comment
KEY2=value2
`;
  const result = EnvLoader.parseEnvFile(content);

  assertEquals(result.KEY1, "value1");
  assertEquals(result.KEY2, "value2");
});

Deno.test("EnvLoader.parseEnvFile - preserves # in quoted values", () => {
  const content = `
COLOR="#ff0000"
MESSAGE="Hello # World"
`;
  const result = EnvLoader.parseEnvFile(content);

  assertEquals(result.COLOR, "#ff0000");
  assertEquals(result.MESSAGE, "Hello # World");
});

Deno.test("EnvLoader.parseEnvFile - handles empty lines", () => {
  const content = `

KEY1=value1

KEY2=value2

`;
  const result = EnvLoader.parseEnvFile(content);

  assertEquals(result.KEY1, "value1");
  assertEquals(result.KEY2, "value2");
  assertEquals(Object.keys(result).length, 2);
});

Deno.test("EnvLoader.parseEnvFile - skips invalid lines", () => {
  const content = `
VALID_KEY=value
invalid line without equals
another-invalid
KEY2=value2
`;
  const result = EnvLoader.parseEnvFile(content);

  assertEquals(result.VALID_KEY, "value");
  assertEquals(result.KEY2, "value2");
  assertEquals(Object.keys(result).length, 2);
});

Deno.test("EnvLoader.parseEnvFile - skips invalid key names", () => {
  const content = `
VALID_KEY=value
123_INVALID=value
-INVALID=value
ANOTHER_VALID=value2
`;
  const result = EnvLoader.parseEnvFile(content);

  assertEquals(result.VALID_KEY, "value");
  assertEquals(result.ANOTHER_VALID, "value2");
  assertEquals(Object.keys(result).length, 2);
});

Deno.test("EnvLoader.parseEnvFile - handles empty values", () => {
  const content = `
EMPTY=
ANOTHER_EMPTY=""
HAS_VALUE=something
`;
  const result = EnvLoader.parseEnvFile(content);

  assertEquals(result.EMPTY, "");
  assertEquals(result.ANOTHER_EMPTY, "");
  assertEquals(result.HAS_VALUE, "something");
});

Deno.test("EnvLoader.parseEnvFile - handles values with equals sign", () => {
  const content = `
CONNECTION_STRING=host=localhost;port=5432
URL="https://example.com?a=1&b=2"
`;
  const result = EnvLoader.parseEnvFile(content);

  assertEquals(result.CONNECTION_STRING, "host=localhost;port=5432");
  assertEquals(result.URL, "https://example.com?a=1&b=2");
});

Deno.test("EnvLoader.buildEnvFilePaths - with environment", () => {
  const paths = EnvLoader.buildEnvFilePaths("/project", "staging");

  assertEquals(paths.includes("/project/.env.staging"), true);
  assertEquals(paths.includes("/project/.env"), true);
  assertEquals(paths.indexOf("/project/.env.staging"), 0); // First priority
});

Deno.test("EnvLoader.buildEnvFilePaths - without environment", () => {
  const paths = EnvLoader.buildEnvFilePaths("/project");

  assertEquals(paths.length, 1);
  assertEquals(paths[0], "/project/.env");
});

Deno.test("EnvLoader.buildEnvFilePaths - with custom path", () => {
  const paths = EnvLoader.buildEnvFilePaths(
    "/project",
    "production",
    ".secrets",
  );

  assertEquals(paths.includes("/project/.secrets.production"), true);
  assertEquals(paths.includes("/project/.secrets"), true);
});

Deno.test("EnvLoader.isEnvVarReference - ALL_CAPS patterns", () => {
  assertEquals(EnvLoader.isEnvVarReference("DATABASE_URL"), true);
  assertEquals(EnvLoader.isEnvVarReference("API_KEY"), true);
  assertEquals(EnvLoader.isEnvVarReference("SECRET123"), true);
  assertEquals(EnvLoader.isEnvVarReference("A"), true);
});

Deno.test("EnvLoader.isEnvVarReference - non-matching patterns", () => {
  assertEquals(EnvLoader.isEnvVarReference("database_url"), false);
  assertEquals(EnvLoader.isEnvVarReference("Database_Url"), false);
  assertEquals(EnvLoader.isEnvVarReference("myValue"), false);
  assertEquals(EnvLoader.isEnvVarReference("123ABC"), false);
  assertEquals(EnvLoader.isEnvVarReference("https://example.com"), false);
  assertEquals(EnvLoader.isEnvVarReference("user@host"), false);
  assertEquals(EnvLoader.isEnvVarReference(""), false);
});

Deno.test("EnvLoader.resolveVariable - from envVars", () => {
  const envVars = { MY_SECRET: "secret-value" };

  assertEquals(
    EnvLoader.resolveVariable("MY_SECRET", envVars, false),
    "secret-value",
  );
});

Deno.test("EnvLoader.resolveVariable - from host env with fallback", () => {
  Deno.env.set("TEST_ENV_VAR", "host-value");

  try {
    assertEquals(
      EnvLoader.resolveVariable("TEST_ENV_VAR", {}, true),
      "host-value",
    );
  } finally {
    Deno.env.delete("TEST_ENV_VAR");
  }
});

Deno.test("EnvLoader.resolveVariable - returns undefined when not found", () => {
  assertEquals(
    EnvLoader.resolveVariable("NONEXISTENT", {}, false),
    undefined,
  );
});

Deno.test("EnvLoader.resolveVariable - envVars takes priority over host", () => {
  Deno.env.set("PRIORITY_TEST", "host-value");

  try {
    const envVars = { PRIORITY_TEST: "env-file-value" };
    assertEquals(
      EnvLoader.resolveVariable("PRIORITY_TEST", envVars, true),
      "env-file-value",
    );
  } finally {
    Deno.env.delete("PRIORITY_TEST");
  }
});

Deno.test("EnvLoader.getProjectRootFromConfigPath", () => {
  const configPath = "/home/user/myproject/.jiji/deploy.yml";
  const projectRoot = EnvLoader.getProjectRootFromConfigPath(configPath);

  assertEquals(projectRoot, "/home/user/myproject");
});

Deno.test("EnvLoader.loadEnvFile - returns empty when file not found", async () => {
  const result = await EnvLoader.loadEnvFile({
    projectRoot: "/nonexistent/path",
  });

  assertEquals(result.variables, {});
  assertEquals(result.loadedFrom, null);
});
