/**
 * Command to prune old Docker/Podman images
 */

import { Command } from "@cliffy/command";
import { ImagePruneService } from "../../lib/services/image_prune_service.ts";
import {
  cleanupSSHConnections,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { log } from "../../utils/logger.ts";
import { Configuration } from "../../lib/configuration.ts";

import type { GlobalOptions, PruneResult } from "../../types.ts";

interface PruneOptions extends GlobalOptions {
  retain?: number;
  dangling?: boolean;
}

export const pruneCommand = new Command()
  .description("Prune old Docker/Podman images from servers")
  .option(
    "-r, --retain <count:number>",
    "Number of recent images to retain per service",
    { default: 3 },
  )
  .option(
    "--no-dangling",
    "Skip removal of dangling (untagged) images",
  )
  .action(async (options) => {
    const pruneOptions = options as unknown as PruneOptions;
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      log.section("Service Image Pruning:");

      const config = await Configuration.load(
        globalOptions.environment,
        globalOptions.configFile,
      );

      const configPath = config.configPath || "unknown";
      const allHosts = config.getAllServerHosts();

      log.say(`Configuration loaded from: ${configPath}`, 1);
      log.say(`Container engine: ${config.builder.engine}`, 1);
      log.say(
        `Found ${allHosts.length} remote host(s): ${allHosts.join(", ")}`,
        1,
      );

      // Setup command context (config + SSH connections - this will show SSH Connection Setup section)
      ctx = await setupCommandContext(globalOptions);
      const context = ctx; // Create non-undefined reference for closure

      if (context.targetHosts.length === 0) {
        log.error(
          "No servers are reachable. Cannot prune images.",
        );
        Deno.exit(1);
      }

      // Show connection status for each host
      console.log(""); // Empty line
      for (const ssh of context.sshManagers) {
        log.remote(ssh.getHost(), ": Connected", { indent: 1 });
      }

      // Create image prune service
      const pruneService = new ImagePruneService(
        context.config.builder.engine,
        context.config.project,
      );

      // Prune images on all connected hosts
      const retainCount = pruneOptions.retain ?? 3;
      log.section("Pruning Images:");
      log.say(
        `- Pruning images (retaining last ${retainCount} per service)`,
        1,
      );

      const results: PruneResult[] = [];
      for (const ssh of context.sshManagers) {
        const host = ssh.getHost();
        await log.hostBlock(host, async () => {
          const result = await pruneService.pruneImages(ssh, {
            retain: pruneOptions.retain,
            removeDangling: pruneOptions.dangling,
          });
          results.push(result);
        }, { indent: 1 });
      }

      // Display results
      const successCount = results.filter((r) => r.success).length;
      const totalRemoved = results.reduce(
        (sum, r) => sum + r.imagesRemoved,
        0,
      );

      if (totalRemoved === 0) {
        log.say("- No old images were pruned", 1);
      }

      log.section("Pruning Results:");
      for (const result of results) {
        if (result.success) {
          log.say(
            `- ${result.host}: ${result.imagesRemoved} image(s) removed`,
            1,
          );
        } else {
          log.say(`- ${result.host}: ${result.error}`, 1);
        }
      }

      log.success(
        `\nPruned ${totalRemoved} image(s) across ${successCount} server(s)`,
        0,
      );

      // Close SSH connections
      cleanupSSHConnections(context.sshManagers);
    } catch (error) {
      if (ctx) {
        await handleCommandError(error, {
          operation: "Image Pruning",
          component: "prune",
          sshManagers: ctx.sshManagers,
          projectName: ctx.config.project,
          targetHosts: ctx.targetHosts,
        });
      } else {
        log.error(
          `Prune command failed: ${
            error instanceof Error ? error.message : String(error)
          }`,
        );
      }
      Deno.exit(1);
    }
  });
