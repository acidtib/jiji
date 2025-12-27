import { assertEquals } from "@std/assert";
import {
  buildDeployCommandArgs,
  type KamalProxyDeployOptions,
} from "../proxy.ts";
import { ProxyConfiguration } from "../../lib/configuration/proxy.ts";

// Unit tests for buildDeployCommandArgs

Deno.test("buildDeployCommandArgs - builds correct argument array with all options", () => {
  const options: KamalProxyDeployOptions = {
    serviceName: "test",
    target: "container:3000",
    hosts: ["example.com"],
    pathPrefix: "/api",
    tls: true,
    healthCheckPath: "/health",
    healthCheckInterval: "30s",
  };

  const args = buildDeployCommandArgs(options);

  assertEquals(args.includes("--target=container:3000"), true);
  assertEquals(args.includes("--host=example.com"), true);
  assertEquals(args.includes("--path-prefix=/api"), true);
  assertEquals(args.includes("--tls"), true);
  assertEquals(args.includes("--health-check-path=/health"), true);
  assertEquals(args.includes("--health-check-interval=30s"), true);
});

Deno.test("buildDeployCommandArgs - minimal options", () => {
  const options: KamalProxyDeployOptions = {
    serviceName: "minimal",
    target: "min-container:8080",
  };

  const args = buildDeployCommandArgs(options);

  assertEquals(args.length, 1);
  assertEquals(args[0], "--target=min-container:8080");
});

Deno.test("buildDeployCommandArgs - with host and SSL only", () => {
  const options: KamalProxyDeployOptions = {
    serviceName: "secure",
    target: "secure-container:443",
    hosts: ["secure.example.com"],
    tls: true,
  };

  const args = buildDeployCommandArgs(options);

  assertEquals(args.includes("--target=secure-container:443"), true);
  assertEquals(args.includes("--host=secure.example.com"), true);
  assertEquals(args.includes("--tls"), true);
  assertEquals(args.includes("--path-prefix"), false);
  assertEquals(args.includes("--health-check-path"), false);
});

Deno.test("buildDeployCommandArgs - with path prefix only", () => {
  const options: KamalProxyDeployOptions = {
    serviceName: "path-service",
    target: "path-container:3000",
    pathPrefix: "/admin",
  };

  const args = buildDeployCommandArgs(options);

  assertEquals(args.includes("--target=path-container:3000"), true);
  assertEquals(args.includes("--path-prefix=/admin"), true);
  assertEquals(args.length, 2);
});

Deno.test("buildDeployCommandArgs - respects false tls value", () => {
  const options: KamalProxyDeployOptions = {
    serviceName: "no-tls",
    target: "container:3000",
    hosts: ["http.example.com"],
    tls: false,
  };

  const args = buildDeployCommandArgs(options);

  assertEquals(args.includes("--tls"), false);
  assertEquals(args.includes("--host=http.example.com"), true);
});

Deno.test("buildDeployCommandArgs - multiple hosts", () => {
  const options: KamalProxyDeployOptions = {
    serviceName: "multi-host",
    target: "container:3000",
    hosts: ["domain.com", "other.domain.com", "www.domain.com"],
    tls: true,
  };

  const args = buildDeployCommandArgs(options);

  assertEquals(args.includes("--target=container:3000"), true);
  assertEquals(args.includes("--host=domain.com"), true);
  assertEquals(args.includes("--host=other.domain.com"), true);
  assertEquals(args.includes("--host=www.domain.com"), true);
  assertEquals(args.includes("--tls"), true);

  // Verify all three hosts are present in the args
  const hostArgs = args.filter((arg) => arg.startsWith("--host="));
  assertEquals(hostArgs.length, 3);
});

Deno.test("ProxyConfiguration - supports hosts array", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    ssl: true,
    hosts: ["domain.com", "www.domain.com"],
  });

  assertEquals(config.targets[0].hosts, ["domain.com", "www.domain.com"]);
  assertEquals(config.enabled, true);
  assertEquals(config.targets[0].ssl, true);
});
