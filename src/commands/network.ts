/**
 * Network command - main entry point for network-related commands
 */

import { Command } from "@cliffy/command";
import { statusCommand } from "./network/status.ts";
import { teardownCommand } from "./network/teardown.ts";

export const networkCommand = new Command()
  .description("Manage private network")
  .action(function () {
    this.showHelp();
  })
  .command("status", statusCommand)
  .command("teardown", teardownCommand);
