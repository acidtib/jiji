/**
 * Network server commands — manage servers registered in the cluster.
 *
 * Currently only `remove` is implemented, which tombstones a server in
 * Corrosion. Once the row gossips out, every node's daemon reconciler will
 * drop the corresponding WireGuard peer on its next tick (~30s) and the
 * garbage collector will prune any container records still referencing the
 * dead server.
 *
 * `remove` is metadata-only: it does NOT SSH to the target server, stop
 * services, or revoke keys. Use this AFTER a server is physically
 * decommissioned, or alongside `jiji server teardown` for in-place cleanup.
 */

import { Command } from "@cliffy/command";
import { Confirm } from "@cliffy/prompt";
import {
  cleanupSSHConnections,
  displayCommandHeader,
  setupCommandContext,
} from "../../utils/command_helpers.ts";
import { handleCommandError } from "../../utils/error_handler.ts";
import { log } from "../../utils/logger.ts";
import { tombstoneServer } from "../../lib/network/corrosion.ts";
import { loadTopology } from "../../lib/network/topology.ts";
import type { GlobalOptions } from "../../types.ts";

const removeCommand = new Command()
  .description("Tombstone a server in the cluster, removing it from the mesh")
  .arguments("<id:string>")
  .option("-y, --confirmed", "Skip confirmation prompt", { default: false })
  .action(async (options, id) => {
    const globalOptions = options as unknown as GlobalOptions;
    let ctx: Awaited<ReturnType<typeof setupCommandContext>> | undefined;

    try {
      ctx = await setupCommandContext(globalOptions, {
        useAllDefinedServers: true,
      });
      const { config, sshManagers } = ctx;

      displayCommandHeader("Network Server Remove:", config, sshManagers);

      if (!config.network.enabled) {
        log.error(
          "Network is not enabled in configuration. Tombstones only apply to clusters with network.enabled: true.",
        );
        Deno.exit(1);
      }

      // We only need one reachable cluster member; Corrosion is distributed.
      const writer = sshManagers[0];
      if (!writer) {
        log.error(
          "No SSH connections available. At least one cluster member must be reachable to write the tombstone.",
        );
        Deno.exit(1);
      }

      // Show a hint about what we're about to remove if we can resolve it.
      let hostnameHint = "";
      try {
        const topology = await loadTopology(writer);
        const match = topology?.servers.find((s) => s.id === id);
        if (match) {
          hostnameHint = ` (${match.hostname})`;
        }
      } catch {
        // Topology lookup is informational only.
      }

      const confirmed = options.confirmed as boolean;
      if (!confirmed) {
        console.log();
        log.warn(`This will tombstone server '${id}'${hostnameHint}.`);
        log.say(
          "Every node will drop its WireGuard peer for this server on the next daemon tick,",
          1,
        );
        log.say(
          "and any container records pointing at it will be garbage-collected.",
          1,
        );
        console.log();

        const ok = await Confirm.prompt({
          message: `Tombstone ${id}?`,
          default: false,
        });
        if (!ok) {
          log.say("Cancelled by user");
          return;
        }
      }

      const result = await tombstoneServer(writer, id);

      if (!result.tombstoned) {
        log.error(
          `Server '${id}' not found in Corrosion. Run \`jiji network status\` to list known server ids.`,
        );
        Deno.exit(1);
      }

      if (result.alreadyRemoved) {
        log.say(`Server '${id}' was already tombstoned — no change.`);
        return;
      }

      log.success(`\nServer '${id}'${hostnameHint} tombstoned.`);
      log.say(
        "Peers will be removed and containers GC'd within ~30s on each node.",
        1,
      );
    } catch (error) {
      await handleCommandError(error, {
        operation: "Network server remove",
        component: "network-server-remove",
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

export const serverCommand = new Command()
  .description("Manage servers registered in the cluster")
  .action(function () {
    this.showHelp();
  })
  .command("remove", removeCommand);
