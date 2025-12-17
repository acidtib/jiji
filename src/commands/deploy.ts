import { Command } from "@cliffy/command";
import { Configuration } from "../lib/configuration.ts";
import { setupSSHConnections, type SSHManager } from "../utils/ssh.ts";
import { createServerAuditLogger } from "../utils/audit.ts";
import { log } from "../utils/logger.ts";
import { extractAppPort, ProxyCommands } from "../utils/proxy.ts";

import type { GlobalOptions } from "../types.ts";

export const deployCommand = new Command()
  .description("Deploy services to servers")
  .action(async (options) => {
    let uniqueHosts: string[] = [];
    let config: Configuration | undefined;
    let sshManagers: SSHManager[] | undefined;

    try {
      await log.group("Service Deployment", async () => {
        log.info("Starting service deployment process", "deploy");

        // Cast options to GlobalOptions
        const globalOptions = options as unknown as GlobalOptions;

        // Load configuration
        config = await Configuration.load(
          globalOptions.environment,
          globalOptions.configFile,
        );
        const configPath = config.configPath || "unknown";
        log.success(`Configuration loaded from: ${configPath}`, "config");
        log.info(`Container engine: ${config.engine}`, "engine");

        // Collect all unique hosts
        const allHosts = config.getAllHosts();
        uniqueHosts = allHosts;

        if (uniqueHosts.length === 0) {
          log.error(
            `No remote hosts found in configuration at: ${configPath}`,
            "config",
          );
          Deno.exit(1);
        }

        log.info(
          `Found ${uniqueHosts.length} remote host(s): ${
            uniqueHosts.join(", ")
          }`,
          "deploy",
        );

        await log.group("SSH Connection Setup", async () => {
          const result = await setupSSHConnections(
            uniqueHosts,
            {
              user: config!.ssh.user,
              port: config!.ssh.port,
              proxy: config!.ssh.proxy,
              proxy_command: config!.ssh.proxyCommand,
              keys: config!.ssh.allKeys.length > 0
                ? config!.ssh.allKeys
                : undefined,
              keyData: config!.ssh.keyData,
              keysOnly: config!.ssh.keysOnly,
              dnsRetries: config!.ssh.dnsRetries,
            },
            { allowPartialConnection: true },
          );

          sshManagers = result.managers;
          uniqueHosts = result.connectedHosts;
        });

        // Check which services have proxy configuration
        const servicesWithProxy = Array.from(config.services.values())
          .filter((service) => service.proxy?.enabled);

        if (servicesWithProxy.length > 0) {
          await log.group("Proxy Installation", async () => {
            log.info(
              `Found ${servicesWithProxy.length} service(s) with proxy configuration`,
              "proxy",
            );

            // Get unique hosts that need proxy
            const proxyHosts = new Set<string>();
            for (const service of servicesWithProxy) {
              for (const host of service.hosts) {
                if (uniqueHosts.includes(host)) {
                  proxyHosts.add(host);
                }
              }
            }

            log.info(
              `Installing kamal-proxy on ${proxyHosts.size} host(s): ${
                Array.from(proxyHosts).join(", ")
              }`,
              "proxy",
            );

            // Install proxy on each host that needs it
            const proxyResults: Array<{
              host: string;
              success: boolean;
              message?: string;
            }> = [];

            for (const host of proxyHosts) {
              const hostSsh = sshManagers!.find((ssh) =>
                ssh.getHost() === host
              );
              if (!hostSsh) continue;

              try {
                const proxyCmd = new ProxyCommands(config.engine, hostSsh);

                // Ensure network exists
                await proxyCmd.ensureNetwork();

                // Check if proxy is already running
                const isRunning = await proxyCmd.isRunning();

                if (isRunning) {
                  const version = await proxyCmd.getVersion();
                  log.info(
                    `kamal-proxy already running on ${host} (version: ${
                      version || "unknown"
                    })`,
                    "proxy",
                  );
                  proxyResults.push({
                    host,
                    success: true,
                    message: "Already running",
                  });
                } else {
                  // Boot the proxy
                  log.status(`Booting kamal-proxy on ${host}...`, "proxy");
                  await proxyCmd.boot();

                  const version = await proxyCmd.getVersion();
                  log.success(
                    `kamal-proxy started on ${host} (version: ${
                      version || "unknown"
                    })`,
                    "proxy",
                  );
                  proxyResults.push({
                    host,
                    success: true,
                    message: "Started",
                  });

                  // Log to audit
                  const hostLogger = createServerAuditLogger(
                    hostSsh,
                    config.project,
                  );
                  await hostLogger.logProxyEvent(
                    "boot",
                    "success",
                    `kamal-proxy ${version || "unknown"} started`,
                  );
                }
              } catch (error) {
                const errorMessage = error instanceof Error
                  ? error.message
                  : String(error);
                log.error(
                  `Failed to install proxy on ${host}: ${errorMessage}`,
                  "proxy",
                );
                proxyResults.push({
                  host,
                  success: false,
                  message: errorMessage,
                });

                // Log failure to audit
                const hostLogger = createServerAuditLogger(
                  hostSsh,
                  config.project,
                );
                await hostLogger.logProxyEvent(
                  "boot",
                  "failed",
                  errorMessage,
                );
              }
            }

            // Summary
            const successCount = proxyResults.filter((r) => r.success).length;
            const failCount = proxyResults.filter((r) => !r.success).length;

            if (successCount > 0) {
              log.success(
                `kamal-proxy ready on ${successCount} host(s)`,
                "proxy",
              );
            }
            if (failCount > 0) {
              log.error(
                `kamal-proxy installation failed on ${failCount} host(s)`,
                "proxy",
              );
            }
          });

          // Deploy service containers
          await log.group("Service Container Deployment", async () => {
            for (const service of servicesWithProxy) {
              log.status(`Deploying ${service.name} containers`, "deploy");

              for (const host of service.hosts) {
                if (!uniqueHosts.includes(host)) {
                  log.warn(
                    `Skipping ${service.name} on unreachable host: ${host}`,
                    "deploy",
                  );
                  continue;
                }

                const hostSsh = sshManagers!.find((ssh) =>
                  ssh.getHost() === host
                );
                if (!hostSsh) continue;

                try {
                  const containerName = service.getContainerName();
                  const imageName = service.getImageName();

                  // For Podman, ensure image has full registry path
                  const fullImageName = imageName.includes("/")
                    ? imageName
                    : `docker.io/library/${imageName}`;

                  // Pull image
                  log.status(
                    `Pulling image ${fullImageName} on ${host}`,
                    "deploy",
                  );
                  const pullResult = await hostSsh.executeCommand(
                    `${config.engine} pull ${fullImageName}`,
                  );
                  if (!pullResult.success) {
                    throw new Error(
                      `Failed to pull image: ${pullResult.stderr}`,
                    );
                  }

                  // Stop and remove existing container
                  await hostSsh.executeCommand(
                    `${config.engine} rm -f ${containerName} 2>/dev/null || true`,
                  );

                  // Build container run command
                  const portArgs = service.ports
                    .map((p) => `-p ${p}`)
                    .join(" ");
                  const volumeArgs = service.volumes
                    .map((v) => `-v ${v}`)
                    .join(" ");
                  const envArgs = service.environment?.toObject
                    ? Object.entries(service.environment.toObject())
                      .map(([k, v]) => `-e ${k}="${v}"`)
                      .join(" ")
                    : "";

                  const runCommand =
                    `${config.engine} run --name ${containerName} --network jiji --detach --restart unless-stopped ${portArgs} ${volumeArgs} ${envArgs} ${fullImageName}`;

                  log.status(
                    `Starting container ${containerName} on ${host}`,
                    "deploy",
                  );
                  const runResult = await hostSsh.executeCommand(runCommand);
                  if (!runResult.success) {
                    throw new Error(
                      `Failed to start container: ${runResult.stderr}`,
                    );
                  }

                  // Wait for container to be running
                  let attempts = 0;
                  const maxAttempts = 10;
                  while (attempts < maxAttempts) {
                    const statusResult = await hostSsh.executeCommand(
                      `${config.engine} inspect ${containerName} --format '{{.State.Status}}'`,
                    );
                    if (
                      statusResult.success &&
                      statusResult.stdout.trim() === "running"
                    ) {
                      break;
                    }
                    attempts++;
                    await new Promise((resolve) => setTimeout(resolve, 1000));
                  }

                  if (attempts >= maxAttempts) {
                    throw new Error(
                      `Container ${containerName} did not start within ${maxAttempts} seconds`,
                    );
                  }

                  log.success(
                    `${service.name} deployed successfully on ${host}`,
                    "deploy",
                  );
                } catch (error) {
                  log.error(
                    `Failed to deploy ${service.name} on ${host}: ${
                      error instanceof Error ? error.message : String(error)
                    }`,
                    "deploy",
                  );
                }
              }
            }
          });

          // Deploy services to proxy
          await log.group("Service Proxy Configuration", async () => {
            for (const service of servicesWithProxy) {
              log.status(
                `Configuring proxy for service: ${service.name}`,
                "proxy",
              );

              const proxyConfig = service.proxy!;
              const appPort = extractAppPort(service.ports);

              for (const host of service.hosts) {
                if (!uniqueHosts.includes(host)) {
                  log.warn(
                    `Skipping ${service.name} on unreachable host: ${host}`,
                    "proxy",
                  );
                  continue;
                }

                const hostSsh = sshManagers!.find((ssh) =>
                  ssh.getHost() === host
                );
                if (!hostSsh) continue;

                try {
                  const proxyCmd = new ProxyCommands(config.engine, hostSsh);
                  const containerName = service.getContainerName();

                  await proxyCmd.deploy(
                    service.name,
                    containerName,
                    proxyConfig,
                    appPort,
                  );

                  log.success(
                    `${service.name} configured on proxy at ${host} (${proxyConfig.host}, port ${appPort})`,
                    "proxy",
                  );

                  // Log to audit
                  const hostLogger = createServerAuditLogger(
                    hostSsh,
                    config.project,
                  );
                  await hostLogger.logProxyEvent(
                    "deploy",
                    "success",
                    `${service.name} -> ${proxyConfig.host}:${appPort} (SSL: ${proxyConfig.ssl})`,
                  );
                } catch (error) {
                  const errorMessage = error instanceof Error
                    ? error.message
                    : String(error);
                  log.error(
                    `Failed to configure ${service.name} on proxy at ${host}: ${errorMessage}`,
                    "proxy",
                  );
                }
              }
            }
          });
        } else {
          log.info("No services configured with proxy", "deploy");
        }

        log.success("Deployment process completed", "deploy");
      });
    } catch (error) {
      const errorMessage = error instanceof Error
        ? error.message
        : String(error);
      log.error("Deployment failed:", "deploy");
      log.error(errorMessage, "deploy");
      Deno.exit(1);
    } finally {
      // Clean up SSH connections
      if (sshManagers) {
        sshManagers.forEach((ssh) => {
          try {
            ssh.dispose();
          } catch (error) {
            log.debug(`Failed to dispose SSH connection: ${error}`, "ssh");
          }
        });
      }
    }
  });
