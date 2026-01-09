/**
 * Network command - main entry point for network-related commands
 */

import { Command } from "@cliffy/command";
import { statusCommand } from "./network/status.ts";
import { teardownCommand } from "./network/teardown.ts";
import { gcCommand } from "./network/gc.ts";
import { dnsCommand } from "./network/dns.ts";
import { inspectCommand } from "./network/inspect.ts";
import { dbCommand } from "./network/db.ts";

export const networkCommand = new Command()
  .description("Manage private network")
  .action(function () {
    this.showHelp();
  })
  .command("status", statusCommand)
  .command("teardown", teardownCommand)
  .command("gc", gcCommand)
  .command("dns", dnsCommand)
  .command("inspect", inspectCommand)
  .command("db", dbCommand);
