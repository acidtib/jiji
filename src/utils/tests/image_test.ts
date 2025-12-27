import { assertEquals } from "@std/assert";
import { normalizeImageName } from "../image.ts";

// Unit tests for normalizeImageName

Deno.test("normalizeImageName - official library images without tag", () => {
  assertEquals(normalizeImageName("nginx"), "docker.io/library/nginx");
  assertEquals(normalizeImageName("postgres"), "docker.io/library/postgres");
  assertEquals(normalizeImageName("redis"), "docker.io/library/redis");
});

Deno.test("normalizeImageName - official library images with tag", () => {
  assertEquals(
    normalizeImageName("nginx:latest"),
    "docker.io/library/nginx:latest",
  );
  assertEquals(
    normalizeImageName("postgres:14"),
    "docker.io/library/postgres:14",
  );
  assertEquals(
    normalizeImageName("redis:alpine"),
    "docker.io/library/redis:alpine",
  );
});

Deno.test("normalizeImageName - user/org images without tag", () => {
  assertEquals(
    normalizeImageName("dxflrs/garage"),
    "docker.io/dxflrs/garage",
  );
  assertEquals(
    normalizeImageName("username/myimage"),
    "docker.io/username/myimage",
  );
});

Deno.test("normalizeImageName - user/org images with tag", () => {
  assertEquals(
    normalizeImageName("dxflrs/garage:v2.1.0"),
    "docker.io/dxflrs/garage:v2.1.0",
  );
  assertEquals(
    normalizeImageName("username/myimage:latest"),
    "docker.io/username/myimage:latest",
  );
});

Deno.test("normalizeImageName - images with docker.io registry already", () => {
  assertEquals(
    normalizeImageName("docker.io/library/nginx"),
    "docker.io/library/nginx",
  );
  assertEquals(
    normalizeImageName("docker.io/dxflrs/garage:v2.1.0"),
    "docker.io/dxflrs/garage:v2.1.0",
  );
});

Deno.test("normalizeImageName - images with other registries", () => {
  assertEquals(
    normalizeImageName("ghcr.io/owner/repo:v1"),
    "ghcr.io/owner/repo:v1",
  );
  assertEquals(
    normalizeImageName("gcr.io/project/image:latest"),
    "gcr.io/project/image:latest",
  );
  assertEquals(
    normalizeImageName("quay.io/org/image"),
    "quay.io/org/image",
  );
});

Deno.test("normalizeImageName - localhost registry", () => {
  assertEquals(
    normalizeImageName("localhost/myimage"),
    "localhost/myimage",
  );
  assertEquals(
    normalizeImageName("localhost:5000/myimage"),
    "localhost:5000/myimage",
  );
  assertEquals(
    normalizeImageName("localhost:5000/myimage:v1.0.0"),
    "localhost:5000/myimage:v1.0.0",
  );
});

Deno.test("normalizeImageName - custom registry with port", () => {
  assertEquals(
    normalizeImageName("registry.example.com:5000/myimage"),
    "registry.example.com:5000/myimage",
  );
  assertEquals(
    normalizeImageName("10.0.0.1:5000/myimage:latest"),
    "10.0.0.1:5000/myimage:latest",
  );
});

Deno.test("normalizeImageName - custom registry without port", () => {
  assertEquals(
    normalizeImageName("registry.example.com/myimage"),
    "registry.example.com/myimage",
  );
  assertEquals(
    normalizeImageName("my-registry.internal/org/image:v2"),
    "my-registry.internal/org/image:v2",
  );
});
