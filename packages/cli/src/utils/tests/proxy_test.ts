import { assertEquals, assertThrows } from "@std/assert";
import {
  buildDeployCommandArgs,
  buildKamalProxyOptionsFromTarget,
  type KamalProxyDeployOptions,
} from "../proxy.ts";
import { ProxyConfiguration } from "../../lib/configuration/proxy.ts";
import type { ProxyTarget } from "../../lib/configuration/proxy.ts";

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

// Tests for command-based health checks

Deno.test("buildDeployCommandArgs - command health check with runtime", () => {
  const options: KamalProxyDeployOptions = {
    serviceName: "test",
    target: "container:3000",
    hosts: ["example.com"],
    healthCheckCmd: "test -f /app/ready",
    healthCheckCmdRuntime: "docker",
    healthCheckInterval: "10s",
  };

  const args = buildDeployCommandArgs(options);

  assertEquals(args.includes('--health-check-cmd="test -f /app/ready"'), true);
  assertEquals(args.includes("--health-check-cmd-runtime=docker"), true);
  assertEquals(args.includes("--health-check-interval=10s"), true);
  // Should not include HTTP health check
  assertEquals(args.includes("--health-check-path"), false);
});

Deno.test("buildDeployCommandArgs - command health check with podman runtime", () => {
  const options: KamalProxyDeployOptions = {
    serviceName: "test",
    target: "container:3000",
    hosts: ["example.com"],
    healthCheckCmd: "curl -f http://localhost:3000/health",
    healthCheckCmdRuntime: "podman",
  };

  const args = buildDeployCommandArgs(options);

  assertEquals(
    args.includes('--health-check-cmd="curl -f http://localhost:3000/health"'),
    true,
  );
  assertEquals(args.includes("--health-check-cmd-runtime=podman"), true);
});

Deno.test("buildKamalProxyOptionsFromTarget - auto-detects runtime from builder engine", () => {
  const target: ProxyTarget = {
    app_port: 3000,
    host: "example.com",
    healthcheck: {
      cmd: "test -f /app/ready",
      interval: "10s",
    },
  };

  const options = buildKamalProxyOptionsFromTarget(
    "test-service",
    target,
    3000,
    "myproject",
    undefined,
    "podman", // Builder engine is podman
  );

  assertEquals(options.healthCheckCmd, "test -f /app/ready");
  assertEquals(options.healthCheckCmdRuntime, "podman"); // Should auto-detect from builder
  assertEquals(options.healthCheckInterval, "10s");
});

Deno.test("buildKamalProxyOptionsFromTarget - respects explicit cmd_runtime over builder engine", () => {
  const target: ProxyTarget = {
    app_port: 3000,
    host: "example.com",
    healthcheck: {
      cmd: "test -f /app/ready",
      cmd_runtime: "docker", // Explicitly set to docker
      interval: "10s",
    },
  };

  const options = buildKamalProxyOptionsFromTarget(
    "test-service",
    target,
    3000,
    "myproject",
    undefined,
    "podman", // Builder engine is podman
  );

  assertEquals(options.healthCheckCmd, "test -f /app/ready");
  assertEquals(options.healthCheckCmdRuntime, "docker"); // Should use explicit value, not builder
});

Deno.test("buildKamalProxyOptionsFromTarget - no runtime without cmd", () => {
  const target: ProxyTarget = {
    app_port: 3000,
    host: "example.com",
    healthcheck: {
      path: "/health",
      interval: "10s",
    },
  };

  const options = buildKamalProxyOptionsFromTarget(
    "test-service",
    target,
    3000,
    "myproject",
    undefined,
    "podman", // Builder engine is podman
  );

  assertEquals(options.healthCheckPath, "/health");
  assertEquals(options.healthCheckCmd, undefined);
  assertEquals(options.healthCheckCmdRuntime, undefined); // No runtime for HTTP health checks
});

// Tests for custom TLS certificate support

Deno.test("buildDeployCommandArgs - with tlsCertificatePath and tlsPrivateKeyPath", () => {
  const options: KamalProxyDeployOptions = {
    serviceName: "web",
    target: "container:3000",
    hosts: ["example.com"],
    tls: true,
    tlsCertificatePath: "/jiji-certs/myproject/web-3000/cert.pem",
    tlsPrivateKeyPath: "/jiji-certs/myproject/web-3000/key.pem",
  };

  const args = buildDeployCommandArgs(options);

  assertEquals(args.includes("--tls"), true); // --tls required even with custom certs
  assertEquals(
    args.includes(
      "--tls-certificate-path=/jiji-certs/myproject/web-3000/cert.pem",
    ),
    true,
  );
  assertEquals(
    args.includes(
      "--tls-private-key-path=/jiji-certs/myproject/web-3000/key.pem",
    ),
    true,
  );
});

Deno.test("buildDeployCommandArgs - tls:true uses --tls, not cert paths", () => {
  const options: KamalProxyDeployOptions = {
    serviceName: "web",
    target: "container:3000",
    hosts: ["example.com"],
    tls: true,
  };

  const args = buildDeployCommandArgs(options);

  assertEquals(args.includes("--tls"), true);
  assertEquals(
    args.some((a) => a.startsWith("--tls-certificate-path")),
    false,
  );
  assertEquals(
    args.some((a) => a.startsWith("--tls-private-key-path")),
    false,
  );
});

Deno.test("buildDeployCommandArgs - no TLS flags without tls or cert paths", () => {
  const options: KamalProxyDeployOptions = {
    serviceName: "web",
    target: "container:3000",
    hosts: ["example.com"],
  };

  const args = buildDeployCommandArgs(options);

  assertEquals(args.includes("--tls"), false);
  assertEquals(
    args.some((a) => a.startsWith("--tls-certificate-path")),
    false,
  );
  assertEquals(
    args.some((a) => a.startsWith("--tls-private-key-path")),
    false,
  );
});

Deno.test("ProxyConfiguration - parses ssl object into ProxySslCerts", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    host: "example.com",
    ssl: {
      certificate_pem: "CERTIFICATE_PEM",
      private_key_pem: "PRIVATE_KEY_PEM",
    },
  });

  const ssl = config.targets[0].ssl;
  assertEquals(typeof ssl, "object");
  assertEquals(
    (ssl as { certificate_pem: string }).certificate_pem,
    "CERTIFICATE_PEM",
  );
  assertEquals(
    (ssl as { private_key_pem: string }).private_key_pem,
    "PRIVATE_KEY_PEM",
  );
});

Deno.test("ProxyConfiguration - ssl object missing private_key_pem throws ConfigurationError", () => {
  const config = new ProxyConfiguration({
    app_port: 3000,
    host: "example.com",
    ssl: {
      certificate_pem: "CERTIFICATE_PEM",
    },
  });

  assertThrows(
    () => config.validate(),
    Error,
    "ssl requires both",
  );
});
