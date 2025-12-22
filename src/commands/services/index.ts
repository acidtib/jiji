import { Command } from "@cliffy/command";
import { pruneCommand } from "./prune.ts";
import { restartCommand } from "./restart.ts";
import { logsCommand } from "./logs.ts";

export const servicesCommand = new Command()
  .description("Manage services")
  .command("prune", pruneCommand)
  .command("restart", restartCommand)
  .command("logs", logsCommand);
