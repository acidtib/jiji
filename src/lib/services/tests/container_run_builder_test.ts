import { assertEquals, assertStringIncludes } from "@std/assert";
import { ContainerRunBuilder } from "../container_run_builder.ts";

Deno.test("ContainerRunBuilder - basic command construction", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest");
  const command = builder.build();

  assertStringIncludes(command, "podman run");
  assertStringIncludes(command, "--name test-app");
  assertStringIncludes(command, "nginx:latest");
});

Deno.test("ContainerRunBuilder - with network", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .network("jiji");
  const command = builder.build();

  assertStringIncludes(command, "--network jiji");
});

Deno.test("ContainerRunBuilder - with DNS configuration", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .dns("10.210.0.1", "jiji");
  const command = builder.build();

  assertStringIncludes(command, "--dns 10.210.0.1");
  assertStringIncludes(command, "--dns 8.8.8.8");
  assertStringIncludes(command, "--dns-search jiji");
  assertStringIncludes(command, "--dns-option ndots:1");
});

Deno.test("ContainerRunBuilder - with ports", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .ports(["80:80", "443:443"]);
  const command = builder.build();

  assertStringIncludes(command, "-p 80:80");
  assertStringIncludes(command, "-p 443:443");
});

Deno.test("ContainerRunBuilder - with volumes", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .volumes("-v /host/path:/container/path -v /another:/path");
  const command = builder.build();

  assertStringIncludes(command, "-v /host/path:/container/path");
  assertStringIncludes(command, "-v /another:/path");
});

Deno.test("ContainerRunBuilder - with simple environment variables", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .environment(["NODE_ENV=production", "PORT=3000"]);
  const command = builder.build();

  assertStringIncludes(command, "-e NODE_ENV=production");
  assertStringIncludes(command, "-e PORT=3000");
});

Deno.test("ContainerRunBuilder - with environment variables containing dollar signs (bcrypt)", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .environment([
      "AUTH_USER_PASS=acidtib:$2y$10$XEjmVEckbCl837vv6zcyF./QLeg8gF/zyM8dwD3mqBG7a3g.KwUci",
    ]);
  const command = builder.build();

  // The entire environment variable should be present and properly escaped
  assertStringIncludes(
    command,
    "-e 'AUTH_USER_PASS=acidtib:$2y$10$XEjmVEckbCl837vv6zcyF./QLeg8gF/zyM8dwD3mqBG7a3g.KwUci'",
  );
});

Deno.test("ContainerRunBuilder - with environment variables containing spaces", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .environment(["MESSAGE=Hello World", "DESCRIPTION=A test value"]);
  const command = builder.build();

  assertStringIncludes(command, "-e 'MESSAGE=Hello World'");
  assertStringIncludes(command, "-e 'DESCRIPTION=A test value'");
});

Deno.test("ContainerRunBuilder - with environment variables containing backticks", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .environment(["COMMAND=`whoami`"]);
  const command = builder.build();

  // Backticks should be escaped to prevent command injection
  assertStringIncludes(command, "-e 'COMMAND=`whoami`'");
});

Deno.test("ContainerRunBuilder - with environment variables containing single quotes", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .environment(["MESSAGE=It's a test"]);
  const command = builder.build();

  // Single quotes should be properly escaped
  assertStringIncludes(command, "-e 'MESSAGE=It'\\''s a test'");
});

Deno.test("ContainerRunBuilder - with environment variables containing double quotes", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .environment(['MESSAGE=Say "hello"']);
  const command = builder.build();

  // Double quotes should be preserved
  assertStringIncludes(command, "-e 'MESSAGE=Say \"hello\"'");
});

Deno.test("ContainerRunBuilder - with environment variables containing ampersands", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .environment(["URL=http://example.com?foo=bar&baz=qux"]);
  const command = builder.build();

  assertStringIncludes(command, "-e 'URL=http://example.com?foo=bar&baz=qux'");
});

Deno.test("ContainerRunBuilder - with environment variables containing pipes", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .environment(["COMMAND=cat file | grep test"]);
  const command = builder.build();

  assertStringIncludes(command, "-e 'COMMAND=cat file | grep test'");
});

Deno.test("ContainerRunBuilder - with environment variables containing semicolons", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .environment(["COMMAND=echo hello; rm -rf /"]);
  const command = builder.build();

  assertStringIncludes(command, "-e 'COMMAND=echo hello; rm -rf /'");
});

Deno.test("ContainerRunBuilder - with restart policy", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .restart("unless-stopped");
  const command = builder.build();

  assertStringIncludes(command, "--restart unless-stopped");
});

Deno.test("ContainerRunBuilder - with detached mode", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .detached();
  const command = builder.build();

  assertStringIncludes(command, "--detach");
});

Deno.test("ContainerRunBuilder - complete command with all options", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .network("jiji")
    .dns("10.210.0.1", "jiji")
    .ports(["80:80"])
    .volumes("-v /host:/container")
    .environment([
      "NODE_ENV=production",
      "AUTH_PASS=$2y$10$hash",
      "MESSAGE=Hello World",
    ])
    .restart("unless-stopped")
    .detached();

  const command = builder.build();

  assertStringIncludes(command, "podman run");
  assertStringIncludes(command, "--name test-app");
  assertStringIncludes(command, "--network jiji");
  assertStringIncludes(command, "--dns 10.210.0.1");
  assertStringIncludes(command, "-p 80:80");
  assertStringIncludes(command, "-v /host:/container");
  assertStringIncludes(command, "-e NODE_ENV=production");
  assertStringIncludes(command, "-e 'AUTH_PASS=$2y$10$hash'");
  assertStringIncludes(command, "-e 'MESSAGE=Hello World'");
  assertStringIncludes(command, "--restart unless-stopped");
  assertStringIncludes(command, "--detach");
  assertStringIncludes(command, "nginx:latest");
});

Deno.test("ContainerRunBuilder - buildArgs returns array", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .network("jiji")
    .environment(["NODE_ENV=production"]);

  const args = builder.buildArgs();

  assertEquals(Array.isArray(args), true);
  assertEquals(args.includes("run"), true);
  assertEquals(args.includes("--name"), true);
  assertEquals(args.includes("test-app"), true);
  assertEquals(args.includes("--network"), true);
  assertEquals(args.includes("jiji"), true);
  assertEquals(args.includes("-e"), true);
  assertEquals(args.includes("NODE_ENV=production"), true);
  assertEquals(args.includes("nginx:latest"), true);
});

Deno.test("ContainerRunBuilder - docker engine support", () => {
  const builder = new ContainerRunBuilder("docker", "test-app", "nginx:latest");
  const command = builder.build();

  assertStringIncludes(command, "docker run");
});

Deno.test("ContainerRunBuilder - arguments without special chars are not escaped", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .environment(["SIMPLE=value", "NUMBER=123", "BOOL=true"]);

  const command = builder.build();

  // These should NOT be quoted as they don't contain special characters
  assertStringIncludes(command, "-e SIMPLE=value");
  assertStringIncludes(command, "-e NUMBER=123");
  assertStringIncludes(command, "-e BOOL=true");
});

Deno.test("ContainerRunBuilder - mixed safe and unsafe environment variables", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .environment([
      "SAFE=value",
      "UNSAFE=$pecial",
      "ANOTHER=normal",
      "WITH_SPACE=hello world",
    ]);

  const command = builder.build();

  // Safe values should not be quoted
  assertStringIncludes(command, "-e SAFE=value");
  assertStringIncludes(command, "-e ANOTHER=normal");

  // Unsafe values should be quoted
  assertStringIncludes(command, "-e 'UNSAFE=$pecial'");
  assertStringIncludes(command, "-e 'WITH_SPACE=hello world'");
});
