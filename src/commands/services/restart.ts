/**
 * Command to restart services on servers
 */

import { Command } from "@cliffy/command";
import { ContainerDeploymentService } from "../../lib/services/container_deployment_service.ts";
import {
  cleanupSSHConnections,
  displayCommandHeader,
  executeBestEffort,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { log } from "../../utils/logger.ts";

import type { GlobalOptions } from "../../types.ts";
import type { ServiceConfiguration } from "../../lib/configuration/service.ts";

export const restartCommand = new Command()
  .description("Restart services on servers")
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      // Validate that either hosts or services are specified
      if (!globalOptions.hosts && !globalOptions.services) {
        log.error(
          "You must specify either --hosts (-H) or --services (-S) to target specific services.",
        );
        log.say(
          "This prevents accidentally restarting all services. Use --help for usage examples.",
          1,
        );
        Deno.exit(1);
      }

      // Setup command context (load config and establish SSH connections)
      ctx = await setupCommandContext(globalOptions);
      const context = ctx; // Create non-undefined reference for closure
      const { config } = context;

      // Display standardized command header
      displayCommandHeader(
        "Service Restart:",
        config,
        context.sshManagers,
        { showServices: context.matchingServices },
      );

      if (context.targetHosts.length === 0) {
        log.error(
          "No servers are reachable. Cannot restart services.",
        );
        Deno.exit(1);
      }

      let servicesToRestart = Array.from(context.config.services.values());

      if (context.matchingServices && context.matchingServices.length > 0) {
        servicesToRestart = servicesToRestart.filter((
          service: ServiceConfiguration,
        ) => context.matchingServices!.includes(service.name));
      }

      if (servicesToRestart.length === 0) {
        log.error("No services found to restart.");
        Deno.exit(1);
      }

      log.section("Restarting Services:");
      log.say(
        `- Services: ${
          servicesToRestart.map((s: ServiceConfiguration) => s.name).join(
            ", ",
          )
        }`,
        1,
      );

      // Create deployment service
      const deploymentService = new ContainerDeploymentService(
        context.config.builder.engine,
        context.config,
      );

      // Restart each service
      const allResults = [];
      for (const service of servicesToRestart) {
        log.say(`- Restarting ${service.name} containers`, 1);

        const serviceHosts = service.servers
          .map((server: { host: string }) => server.host)
          .filter((host: string) => context.targetHosts.includes(host));

        if (serviceHosts.length === 0) {
          log.warn(
            `No target hosts found for service ${service.name}`,
          );
          continue;
        }

        // Stop existing containers first
        for (const host of serviceHosts) {
          const ssh = context.sshManagers.find((ssh) => ssh.getHost() === host);
          if (!ssh) {
            log.warn(`SSH connection not found for host ${host}`);
            continue;
          }

          await log.hostBlock(host, async () => {
            const containerName = service.getContainerName();
            log.say(`├── Stopping ${containerName}`, 2);

            // Stop the container
            await executeBestEffort(
              ssh,
              `${context.config.builder.engine} stop ${containerName}`,
              `stopping ${containerName}`,
            );

            // Remove the container
            await executeBestEffort(
              ssh,
              `${context.config.builder.engine} rm -f ${containerName}`,
              `removing ${containerName}`,
            );

            log.say(`└── Stopped ${containerName}`, 2);
          }, { indent: 1 });
        }

        // Deploy the service (which will start new containers)
        const results = await deploymentService.deployServiceToServers(
          service,
          context.sshManagers,
          context.targetHosts,
        );

        allResults.push(...results);

        const successCount = results.filter((r) => r.success).length;
        const totalCount = results.length;

        if (successCount === totalCount) {
          log.say(
            `- ${service.name} restarted successfully on all ${totalCount} server(s)`,
            1,
          );
        } else {
          log.warn(
            `- ${service.name} restarted on ${successCount}/${totalCount} server(s)`,
          );

          // Log failed deployments
          results
            .filter((r) => !r.success)
            .forEach((r) => {
              log.say(`  ${r.host}: ${r.error}`, 2);
            });
        }
      }

      // Summary
      const totalSuccess = allResults.filter((r) => r.success).length;
      const totalAttempted = allResults.length;

      log.section("Restart Summary:");
      for (const result of allResults) {
        if (result.success) {
          log.say(
            `- ${result.service} on ${result.host}: Successfully restarted`,
            1,
          );
        } else {
          log.say(
            `- ${result.service} on ${result.host}: ${result.error}`,
            1,
          );
        }
      }

      if (totalSuccess === totalAttempted) {
        log.success(
          `\nAll services restarted successfully (${totalSuccess}/${totalAttempted})`,
          0,
        );
      } else {
        log.warn(
          `\nPartial success: ${totalSuccess}/${totalAttempted} services restarted`,
        );
        if (totalSuccess === 0) {
          Deno.exit(1);
        }
      }

      // Close SSH connections
      cleanupSSHConnections(context.sshManagers);
    } catch (error) {
      if (ctx) {
        await handleCommandError(error, {
          operation: "Service Restart",
          component: "restart",
          sshManagers: ctx.sshManagers,
          projectName: ctx.config.project,
          targetHosts: ctx.targetHosts,
        });
      } else {
        log.error(
          `Restart command failed: ${
            error instanceof Error ? error.message : String(error)
          }`,
        );
      }
      Deno.exit(1);
    }
  });
