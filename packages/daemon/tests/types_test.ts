import { assertEquals, assertThrows } from "@std/assert";
import { parseConfig } from "../src/types.ts";

Deno.test("parseConfig - throws when JIJI_SERVER_ID missing", () => {
  // Clear env to ensure JIJI_SERVER_ID is not set
  const original = Deno.env.get("JIJI_SERVER_ID");
  Deno.env.delete("JIJI_SERVER_ID");

  try {
    assertThrows(
      () => parseConfig(),
      Error,
      "JIJI_SERVER_ID environment variable is required",
    );
  } finally {
    if (original) Deno.env.set("JIJI_SERVER_ID", original);
  }
});

Deno.test("parseConfig - uses defaults when only JIJI_SERVER_ID set", () => {
  const original = {
    JIJI_SERVER_ID: Deno.env.get("JIJI_SERVER_ID"),
    JIJI_ENGINE: Deno.env.get("JIJI_ENGINE"),
    JIJI_INTERFACE: Deno.env.get("JIJI_INTERFACE"),
    JIJI_CORROSION_API: Deno.env.get("JIJI_CORROSION_API"),
    JIJI_CORROSION_DIR: Deno.env.get("JIJI_CORROSION_DIR"),
    JIJI_LOOP_INTERVAL: Deno.env.get("JIJI_LOOP_INTERVAL"),
  };

  // Set only JIJI_SERVER_ID, clear others
  Deno.env.set("JIJI_SERVER_ID", "test-server-1");
  Deno.env.delete("JIJI_ENGINE");
  Deno.env.delete("JIJI_INTERFACE");
  Deno.env.delete("JIJI_CORROSION_API");
  Deno.env.delete("JIJI_CORROSION_DIR");
  Deno.env.delete("JIJI_LOOP_INTERVAL");

  try {
    const config = parseConfig();
    assertEquals(config.serverId, "test-server-1");
    assertEquals(config.engine, "docker");
    assertEquals(config.interfaceName, "jiji0");
    assertEquals(config.corrosionApi, "http://127.0.0.1:31220");
    assertEquals(config.corrosionDir, "/opt/jiji/corrosion");
    assertEquals(config.loopInterval, 30);
  } finally {
    // Restore
    for (const [key, val] of Object.entries(original)) {
      if (val !== undefined) Deno.env.set(key, val);
      else Deno.env.delete(key);
    }
  }
});

Deno.test("parseConfig - rejects invalid JIJI_ENGINE", () => {
  const original = {
    JIJI_SERVER_ID: Deno.env.get("JIJI_SERVER_ID"),
    JIJI_ENGINE: Deno.env.get("JIJI_ENGINE"),
  };

  Deno.env.set("JIJI_SERVER_ID", "test-server");
  Deno.env.set("JIJI_ENGINE", "invalid");

  try {
    assertThrows(
      () => parseConfig(),
      Error,
      "Invalid JIJI_ENGINE",
    );
  } finally {
    for (const [key, val] of Object.entries(original)) {
      if (val !== undefined) Deno.env.set(key, val);
      else Deno.env.delete(key);
    }
  }
});

Deno.test("parseConfig - rejects invalid JIJI_LOOP_INTERVAL", () => {
  const original = {
    JIJI_SERVER_ID: Deno.env.get("JIJI_SERVER_ID"),
    JIJI_LOOP_INTERVAL: Deno.env.get("JIJI_LOOP_INTERVAL"),
  };

  Deno.env.set("JIJI_SERVER_ID", "test-server");
  Deno.env.set("JIJI_LOOP_INTERVAL", "0");

  try {
    assertThrows(
      () => parseConfig(),
      Error,
      "JIJI_LOOP_INTERVAL must be a positive integer",
    );
  } finally {
    for (const [key, val] of Object.entries(original)) {
      if (val !== undefined) Deno.env.set(key, val);
      else Deno.env.delete(key);
    }
  }
});

Deno.test("parseConfig - accepts custom values", () => {
  const original = {
    JIJI_SERVER_ID: Deno.env.get("JIJI_SERVER_ID"),
    JIJI_ENGINE: Deno.env.get("JIJI_ENGINE"),
    JIJI_INTERFACE: Deno.env.get("JIJI_INTERFACE"),
    JIJI_CORROSION_API: Deno.env.get("JIJI_CORROSION_API"),
    JIJI_CORROSION_DIR: Deno.env.get("JIJI_CORROSION_DIR"),
    JIJI_LOOP_INTERVAL: Deno.env.get("JIJI_LOOP_INTERVAL"),
  };

  Deno.env.set("JIJI_SERVER_ID", "custom-server");
  Deno.env.set("JIJI_ENGINE", "podman");
  Deno.env.set("JIJI_INTERFACE", "wg0");
  Deno.env.set("JIJI_CORROSION_API", "http://10.0.0.1:31220");
  Deno.env.set("JIJI_CORROSION_DIR", "/custom/path");
  Deno.env.set("JIJI_LOOP_INTERVAL", "60");

  try {
    const config = parseConfig();
    assertEquals(config.serverId, "custom-server");
    assertEquals(config.engine, "podman");
    assertEquals(config.interfaceName, "wg0");
    assertEquals(config.corrosionApi, "http://10.0.0.1:31220");
    assertEquals(config.corrosionDir, "/custom/path");
    assertEquals(config.loopInterval, 60);
  } finally {
    for (const [key, val] of Object.entries(original)) {
      if (val !== undefined) Deno.env.set(key, val);
      else Deno.env.delete(key);
    }
  }
});
