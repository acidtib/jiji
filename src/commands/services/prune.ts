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

import type { GlobalOptions } from "../../types.ts";

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
      // Setup command context (config + SSH connections)
      ctx = await setupCommandContext(globalOptions);
      const context = ctx; // Create non-undefined reference for closure

      if (context.targetHosts.length === 0) {
        log.error(
          "No servers are reachable. Cannot prune images.",
          "prune",
        );
        Deno.exit(1);
      }

      await log.group("Image Pruning", async () => {
        log.info(
          `Pruning images on ${context.targetHosts.length} server(s)`,
          "prune",
        );

        // Create image prune service
        const pruneService = new ImagePruneService(
          context.config.builder.engine,
          context.config.project,
        );

        // Prune images on all connected hosts
        log.status(
          `Pruning images (retaining last ${
            pruneOptions.retain ?? 3
          } per service)...`,
          "prune",
        );

        const results = await pruneService.pruneImagesOnHosts(
          context.sshManagers,
          {
            retain: pruneOptions.retain,
            removeDangling: pruneOptions.dangling,
          },
        );

        // Display results
        const successCount = results.filter((r) => r.success).length;
        const totalRemoved = results.reduce(
          (sum, r) => sum + r.imagesRemoved,
          0,
        );

        log.info("\nPrune Results:", "prune");
        for (const result of results) {
          if (result.success) {
            log.success(
              `  ${result.host}: ${result.imagesRemoved} image(s) removed`,
              "prune",
            );
          } else {
            log.error(`  ${result.host}: ${result.error}`, "prune");
          }
        }

        log.success(
          `\nPruned ${totalRemoved} image(s) across ${successCount} server(s)`,
          "prune",
        );
      });

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
          "prune",
        );
      }
      Deno.exit(1);
    }
  });
