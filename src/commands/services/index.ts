import { Command } from "@cliffy/command";
import { pruneCommand } from "./prune.ts";

export const servicesCommand = new Command()
  .description("Manage services")
  .command("prune", pruneCommand);
