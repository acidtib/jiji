import { assertEquals } from "@std/assert";
import { SSHProxy } from "../ssh_proxy.ts";

Deno.test("SSHProxy - parseProxyString with hostname only", () => {
  const result = SSHProxy.parseProxyString("bastion.example.com");

  assertEquals(result, {
    user: "root",
    host: "bastion.example.com",
    port: 22,
  });
});

Deno.test("SSHProxy - parseProxyString with user and hostname", () => {
  const result = SSHProxy.parseProxyString("deploy@bastion.example.com");

  assertEquals(result, {
    user: "deploy",
    host: "bastion.example.com",
    port: 22,
  });
});

Deno.test("SSHProxy - parseProxyString with user, hostname, and port", () => {
  const result = SSHProxy.parseProxyString("admin@bastion.example.com:2222");

  assertEquals(result, {
    user: "admin",
    host: "bastion.example.com",
    port: 2222,
  });
});

Deno.test("SSHProxy - parseProxyString with hostname and port", () => {
  const result = SSHProxy.parseProxyString("bastion.example.com:2222");

  assertEquals(result, {
    user: "root",
    host: "bastion.example.com",
    port: 2222,
  });
});

Deno.test("SSHProxy - parseProxyString with custom default user", () => {
  const result = SSHProxy.parseProxyString(
    "bastion.example.com",
    "customuser",
  );

  assertEquals(result, {
    user: "customuser",
    host: "bastion.example.com",
    port: 22,
  });
});

Deno.test("SSHProxy - parseProxyString with IP address", () => {
  const result = SSHProxy.parseProxyString("root@192.168.1.1:2222");

  assertEquals(result, {
    user: "root",
    host: "192.168.1.1",
    port: 2222,
  });
});

Deno.test("SSHProxy - parseProxyString with IPv4 only", () => {
  const result = SSHProxy.parseProxyString("192.168.1.1");

  assertEquals(result, {
    user: "root",
    host: "192.168.1.1",
    port: 22,
  });
});

Deno.test("SSHProxy - parseProxyString throws on invalid format", () => {
  try {
    SSHProxy.parseProxyString("");
    throw new Error("Expected parseProxyString to throw");
  } catch (error) {
    assertEquals(
      (error as Error).message,
      "Invalid proxy format: ''. Expected: [user@]hostname[:port]",
    );
  }
});

Deno.test("SSHProxy - parseProxyString throws on empty string", () => {
  try {
    SSHProxy.parseProxyString("");
    throw new Error("Expected parseProxyString to throw");
  } catch (error) {
    assertEquals(
      (error as Error).message,
      "Invalid proxy format: ''. Expected: [user@]hostname[:port]",
    );
  }
});

Deno.test("SSHProxy - parseProxyString with standard SSH port", () => {
  const result = SSHProxy.parseProxyString("user@host.com:22");

  assertEquals(result, {
    user: "user",
    host: "host.com",
    port: 22,
  });
});

Deno.test("SSHProxy - parseProxyString with alternate SSH port", () => {
  const result = SSHProxy.parseProxyString("user@host.com:2222");

  assertEquals(result, {
    user: "user",
    host: "host.com",
    port: 2222,
  });
});

Deno.test("SSHProxy - parseProxyString preserves case in hostname", () => {
  const result = SSHProxy.parseProxyString("user@Bastion.Example.COM");

  assertEquals(result, {
    user: "user",
    host: "Bastion.Example.COM",
    port: 22,
  });
});

Deno.test("SSHProxy - parseProxyString with subdomain", () => {
  const result = SSHProxy.parseProxyString(
    "deploy@jump.prod.example.com:2222",
  );

  assertEquals(result, {
    user: "deploy",
    host: "jump.prod.example.com",
    port: 2222,
  });
});

// Note: Integration tests for createProxySocket would require actual SSH servers
// and are better suited for manual integration testing or CI with Docker-based SSH servers
