/**
 * Network setup command
 *
 * Reconciles the private network by re-running the full network setup
 * across all servers. Fixes missing WireGuard peers, broken routing,
 * stale DNS, and other network issues.
 */

import { Command } from "@cliffy/command";
import {
  cleanupSSHConnections,
  displayCommandHeader,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { setupNetwork } from "../../lib/network/setup.ts";
import { log } from "../../utils/logger.ts";
import type { GlobalOptions } from "../../types.ts";

export const setupCommand = new Command()
  .description("Set up or repair private network across all servers")
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      ctx = await setupCommandContext(globalOptions, {
        useAllDefinedServers: true,
      });
      const { config, sshManagers } = ctx;

      displayCommandHeader("Network Setup:", config, sshManagers);

      if (!config.network.enabled) {
        log.error(
          "Network is not enabled in configuration. Set network.enabled: true in your deploy config.",
        );
        return;
      }

      const results = await setupNetwork(config, sshManagers);

      const failed = results.filter((r) => !r.success);
      if (failed.length > 0) {
        for (const result of failed) {
          log.error(`${result.host}: ${result.error}`);
        }
        Deno.exit(1);
      }

      log.success("\nNetwork setup complete", 0);
    } catch (error) {
      await handleCommandError(error, {
        operation: "Network setup",
        component: "network",
        sshManagers: ctx?.sshManagers,
        projectName: ctx?.config?.project,
        targetHosts: ctx?.targetHosts,
      });
    } finally {
      if (ctx?.sshManagers) {
        cleanupSSHConnections(ctx.sshManagers);
      }
    }
  });
