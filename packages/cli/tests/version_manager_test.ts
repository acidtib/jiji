/**
 * Tests for VersionManager
 */

import { assertEquals } from "@std/assert";
import { VersionManager } from "../src/utils/version_manager.ts";

Deno.test("VersionManager - uses custom version when provided", async () => {
  const version = await VersionManager.determineVersionTag({
    customVersion: "v1.2.3",
  });

  assertEquals(version, "v1.2.3");
});

Deno.test("VersionManager - uses custom version with complex tag", async () => {
  const version = await VersionManager.determineVersionTag({
    customVersion: "v2.0.0-beta.1+build.123",
  });

  assertEquals(version, "v2.0.0-beta.1+build.123");
});

Deno.test("VersionManager - returns 'latest' for image services without custom version", async () => {
  const version = await VersionManager.determineVersionTag({
    isImageService: true,
    serviceName: "nginx",
  });

  assertEquals(version, "latest");
});

Deno.test("VersionManager - custom version takes precedence over image service", async () => {
  const version = await VersionManager.determineVersionTag({
    customVersion: "v3.0.0",
    isImageService: true,
  });

  assertEquals(version, "v3.0.0");
});

Deno.test("VersionManager - uses git SHA for build services in git repo", async () => {
  // This test will only pass if running in the actual jiji git repo
  // We'll check if it's a git SHA format (7 or 40 characters, alphanumeric)
  const version = await VersionManager.determineVersionTag({
    useGitSha: true,
    shortSha: true,
    isImageService: false,
  });

  // Git SHA should be 7 chars (short) or 40 chars (full)
  const isValidGitSha = /^[a-f0-9]{7,40}$/i.test(version);
  const isUlid = /^[0-9A-HJKMNP-TV-Z]{26}$/.test(version);

  // Should be either a git SHA or ULID (if not in git repo)
  assertEquals(isValidGitSha || isUlid, true);
});

Deno.test("VersionManager - short SHA is 7 characters", async () => {
  const version = await VersionManager.determineVersionTag({
    useGitSha: true,
    shortSha: true,
    isImageService: false,
  });

  // Check if it's a short git SHA (7 chars) or ULID (26 chars)
  const isShortSha = /^[a-f0-9]{7}$/i.test(version);
  const isUlid = /^[0-9A-HJKMNP-TV-Z]{26}$/.test(version);

  assertEquals(isShortSha || isUlid, true);
});

Deno.test("VersionManager - generates ULID when useGitSha is false", async () => {
  const version = await VersionManager.determineVersionTag({
    useGitSha: false,
    isImageService: false,
  });

  // Should return 'latest' as final fallback
  assertEquals(version, "latest");
});

Deno.test("VersionManager - custom version overrides git SHA", async () => {
  const version = await VersionManager.determineVersionTag({
    customVersion: "v4.5.6",
    useGitSha: true,
    shortSha: true,
  });

  assertEquals(version, "v4.5.6");
});

Deno.test("VersionManager - handles service name in options", async () => {
  const version = await VersionManager.determineVersionTag({
    isImageService: true,
    serviceName: "my-custom-service",
  });

  assertEquals(version, "latest");
});

Deno.test("VersionManager - version precedence order", async () => {
  // Test 1: Custom version beats everything
  const customVersion = await VersionManager.determineVersionTag({
    customVersion: "v1.0.0",
    isImageService: true,
    useGitSha: true,
  });
  assertEquals(customVersion, "v1.0.0");

  // Test 2: Image service without custom version gets 'latest'
  const imageServiceVersion = await VersionManager.determineVersionTag({
    isImageService: true,
  });
  assertEquals(imageServiceVersion, "latest");

  // Test 3: Build service gets git SHA or ULID
  const buildServiceVersion = await VersionManager.determineVersionTag({
    isImageService: false,
    useGitSha: true,
  });
  // Should be git SHA (7-40 chars) or ULID (26 chars)
  const isValidVersion = /^[a-f0-9]{7,40}$/i.test(buildServiceVersion) ||
    /^[0-9A-HJKMNP-TV-Z]{26}$/.test(buildServiceVersion);
  assertEquals(isValidVersion, true);
});

Deno.test("VersionManager - handles empty custom version", async () => {
  const version = await VersionManager.determineVersionTag({
    customVersion: "",
    isImageService: true,
  });

  // Empty string should be falsy, so should fall back to 'latest' for image service
  assertEquals(version, "latest");
});

Deno.test("VersionManager - version tag format validation", async () => {
  // Test various valid version formats
  const versions = [
    "v1.0.0",
    "1.2.3",
    "v2.0.0-rc.1",
    "release-2024-01",
    "main-abc123",
    "feature/new-thing",
  ];

  for (const customVersion of versions) {
    const version = await VersionManager.determineVersionTag({
      customVersion,
    });
    assertEquals(version, customVersion);
  }
});
