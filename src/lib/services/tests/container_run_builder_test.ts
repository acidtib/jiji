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

Deno.test("ContainerRunBuilder - with ports including protocols", () => {
  const builder = new ContainerRunBuilder(
    "podman",
    "test-app",
    "dns-server:latest",
  )
    .ports([
      "53:53/udp",
      "53:53/tcp",
      "127.0.0.1:5353:53/udp",
      "8080:8080/tcp",
    ]);
  const command = builder.build();

  assertStringIncludes(command, "-p 53:53/udp");
  assertStringIncludes(command, "-p 53:53/tcp");
  assertStringIncludes(command, "-p 127.0.0.1:5353:53/udp");
  assertStringIncludes(command, "-p 8080:8080/tcp");
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

Deno.test("ContainerRunBuilder - with cpus as number", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .cpus(2);
  const command = builder.build();

  assertStringIncludes(command, "--cpus 2");
});

Deno.test("ContainerRunBuilder - with cpus as string", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .cpus("1.5");
  const command = builder.build();

  assertStringIncludes(command, "--cpus 1.5");
});

Deno.test("ContainerRunBuilder - with memory", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .memory("512m");
  const command = builder.build();

  assertStringIncludes(command, "--memory 512m");
});

Deno.test("ContainerRunBuilder - with gpus", () => {
  const builder = new ContainerRunBuilder(
    "docker",
    "test-app",
    "tensorflow:latest",
  )
    .gpus("all");
  const command = builder.build();

  assertStringIncludes(command, "--gpus all");
});

Deno.test("ContainerRunBuilder - with specific GPU devices", () => {
  const builder = new ContainerRunBuilder(
    "docker",
    "test-app",
    "tensorflow:latest",
  )
    .gpus("device=0,1");
  const command = builder.build();

  assertStringIncludes(command, "--gpus device=0,1");
});

Deno.test("ContainerRunBuilder - with devices", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "ffmpeg:latest")
    .devices(["/dev/video0", "/dev/snd"]);
  const command = builder.build();

  assertStringIncludes(command, "--device /dev/video0");
  assertStringIncludes(command, "--device /dev/snd");
});

Deno.test("ContainerRunBuilder - with devices including permissions", () => {
  const builder = new ContainerRunBuilder(
    "podman",
    "test-app",
    "media-server:latest",
  )
    .devices(["/dev/video0:/dev/video0:rwm", "/dev/snd:/dev/snd"]);
  const command = builder.build();

  assertStringIncludes(command, "--device /dev/video0:/dev/video0:rwm");
  assertStringIncludes(command, "--device /dev/snd:/dev/snd");
});

Deno.test("ContainerRunBuilder - complete command with resource constraints", () => {
  const builder = new ContainerRunBuilder(
    "podman",
    "ml-app",
    "tensorflow:latest",
  )
    .network("jiji")
    .ports(["8080:8080"])
    .cpus(4)
    .memory("8g")
    .gpus("all")
    .devices(["/dev/nvidia0", "/dev/nvidiactl"])
    .environment(["CUDA_VISIBLE_DEVICES=0"])
    .restart("unless-stopped")
    .detached();

  const command = builder.build();

  assertStringIncludes(command, "podman run");
  assertStringIncludes(command, "--name ml-app");
  assertStringIncludes(command, "--network jiji");
  assertStringIncludes(command, "-p 8080:8080");
  assertStringIncludes(command, "--cpus 4");
  assertStringIncludes(command, "--memory 8g");
  assertStringIncludes(command, "--gpus all");
  assertStringIncludes(command, "--device /dev/nvidia0");
  assertStringIncludes(command, "--device /dev/nvidiactl");
  assertStringIncludes(command, "-e CUDA_VISIBLE_DEVICES=0");
  assertStringIncludes(command, "--restart unless-stopped");
  assertStringIncludes(command, "--detach");
  assertStringIncludes(command, "tensorflow:latest");
});

Deno.test("ContainerRunBuilder - with privileged flag", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "fuse:latest")
    .privileged();
  const command = builder.build();

  assertStringIncludes(command, "--privileged");
});

Deno.test("ContainerRunBuilder - with single capability", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "fuse:latest")
    .capAdd(["SYS_ADMIN"]);
  const command = builder.build();

  assertStringIncludes(command, "--cap-add SYS_ADMIN");
});

Deno.test("ContainerRunBuilder - with multiple capabilities", () => {
  const builder = new ContainerRunBuilder(
    "podman",
    "test-app",
    "network:latest",
  )
    .capAdd(["SYS_ADMIN", "NET_ADMIN", "NET_RAW"]);
  const command = builder.build();

  assertStringIncludes(command, "--cap-add SYS_ADMIN");
  assertStringIncludes(command, "--cap-add NET_ADMIN");
  assertStringIncludes(command, "--cap-add NET_RAW");
});

Deno.test("ContainerRunBuilder - complete FUSE configuration", () => {
  const builder = new ContainerRunBuilder("podman", "rclone", "rclone:latest")
    .network("jiji")
    .ports(["8080:8080"])
    .devices(["/dev/fuse"])
    .privileged()
    .capAdd(["SYS_ADMIN"])
    .memory("2g")
    .environment(["RCLONE_CONFIG=/config/rclone.conf"])
    .restart("unless-stopped")
    .detached();

  const command = builder.build();

  assertStringIncludes(command, "podman run");
  assertStringIncludes(command, "--name rclone");
  assertStringIncludes(command, "--network jiji");
  assertStringIncludes(command, "-p 8080:8080");
  assertStringIncludes(command, "--device /dev/fuse");
  assertStringIncludes(command, "--privileged");
  assertStringIncludes(command, "--cap-add SYS_ADMIN");
  assertStringIncludes(command, "--memory 2g");
  assertStringIncludes(command, "-e RCLONE_CONFIG=/config/rclone.conf");
  assertStringIncludes(command, "--restart unless-stopped");
  assertStringIncludes(command, "--detach");
  assertStringIncludes(command, "rclone:latest");
});

// Command feature tests
Deno.test("ContainerRunBuilder - with string command", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "redis:latest")
    .command("redis-server --appendonly yes");
  const command = builder.build();

  // String command with spaces gets shell-escaped as single argument
  assertStringIncludes(command, "redis:latest 'redis-server --appendonly yes'");
});

Deno.test("ContainerRunBuilder - with array command", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "node:latest")
    .command(["npm", "run", "dev"]);
  const command = builder.build();

  // Each array element should be a separate argument after image
  assertStringIncludes(command, "node:latest npm run dev");
});

Deno.test("ContainerRunBuilder - command with special characters", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "alpine:latest")
    .command(["sh", "-c", "echo $HOME"]);
  const command = builder.build();

  // Special characters should be properly escaped
  assertStringIncludes(command, "alpine:latest sh -c 'echo $HOME'");
});

Deno.test("ContainerRunBuilder - command with single quotes", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .command(["nginx", "-g", "daemon off;"]);
  const command = builder.build();

  // Semicolon should be escaped
  assertStringIncludes(command, "nginx:latest nginx -g 'daemon off;'");
});

Deno.test("ContainerRunBuilder - command appears after image in full command", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "redis:latest")
    .network("jiji")
    .ports(["6379:6379"])
    .detached()
    .command(["redis-server", "--appendonly", "yes"]);
  const command = builder.build();

  // Verify command structure: options, then image, then command args
  assertStringIncludes(command, "podman run");
  assertStringIncludes(command, "--name test-app");
  assertStringIncludes(command, "--network jiji");
  assertStringIncludes(command, "-p 6379:6379");
  assertStringIncludes(command, "--detach");
  // Image and command should be at the end
  assertStringIncludes(command, "redis:latest redis-server --appendonly yes");
});

Deno.test("ContainerRunBuilder - buildArgs includes command", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "node:latest")
    .network("jiji")
    .command(["npm", "start"]);

  const args = builder.buildArgs();

  // Verify command args are at the end after image
  const imageIndex = args.indexOf("node:latest");
  assertEquals(args[imageIndex + 1], "npm");
  assertEquals(args[imageIndex + 2], "start");
});

Deno.test("ContainerRunBuilder - no command does not add extra args", () => {
  const builder = new ContainerRunBuilder("podman", "test-app", "nginx:latest")
    .network("jiji");
  const command = builder.build();

  // Should end with just the image name
  assertEquals(command.endsWith("nginx:latest"), true);
});
