/**
 * Command to reboot kamal-proxy on servers
 */

import { Command } from "@cliffy/command";
import {
  cleanupSSHConnections,
  displayCommandHeader,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { log } from "../../utils/logger.ts";
import { ProxyCommands } from "../../utils/proxy.ts";
import { getDnsServerForHost } from "../../utils/network_helpers.ts";

import type { GlobalOptions } from "../../types.ts";

export const rebootCommand = new Command()
  .description("Reboot kamal-proxy on servers to pick up configuration changes")
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      ctx = await setupCommandContext(globalOptions, {
        skipServiceFiltering: true,
      });
      const { config, sshManagers, targetHosts } = ctx;

      displayCommandHeader("Proxy Reboot:", config, sshManagers);

      if (targetHosts.length === 0) {
        log.error("No servers are reachable. Cannot reboot proxy.");
        Deno.exit(1);
      }

      let failCount = 0;

      for (const host of targetHosts) {
        const ssh = sshManagers.find((s) => s.getHost() === host);
        if (!ssh) {
          log.warn(`SSH connection not found for host ${host}`);
          failCount++;
          continue;
        }

        await log.hostBlock(host, async () => {
          const proxyCmd = new ProxyCommands(config.builder.engine, ssh);

          const isRunning = await proxyCmd.isRunning();
          if (!isRunning) {
            log.say(`└── kamal-proxy is not running on ${host}, skipping`, 2);
            return;
          }

          log.say(`├── Rebooting kamal-proxy on ${host}...`, 2);

          try {
            let dnsServer: string | undefined;
            if (config.network.enabled) {
              dnsServer = await getDnsServerForHost(
                ssh,
                host,
                config.network.enabled,
              );
            }

            await proxyCmd.run({ dnsServer });
            await proxyCmd.waitForReady();

            const version = await proxyCmd.getVersion();
            log.say(
              `└── kamal-proxy rebooted on ${host} (version: ${
                version || "unknown"
              })`,
              2,
            );
          } catch (error) {
            const msg = error instanceof Error
              ? error.message
              : String(error);
            log.error(`Failed to reboot proxy on ${host}: ${msg}`, 2);
            failCount++;
          }
        }, { indent: 1 });
      }

      if (failCount > 0) {
        log.error(
          `kamal-proxy reboot failed on ${failCount} host(s)`,
          1,
        );
      }

      cleanupSSHConnections(sshManagers);
    } catch (error) {
      if (ctx) {
        await handleCommandError(error, {
          operation: "Proxy Reboot",
          component: "proxy-reboot",
          sshManagers: ctx.sshManagers,
          projectName: ctx.config.project,
          targetHosts: ctx.targetHosts,
        });
      } else {
        log.error(
          `Proxy reboot command failed: ${
            error instanceof Error ? error.message : String(error)
          }`,
        );
      }
      Deno.exit(1);
    }
  });
