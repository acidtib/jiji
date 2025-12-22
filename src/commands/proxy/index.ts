import { Command } from "@cliffy/command";
import { logsCommand } from "./logs.ts";

export const proxyCommand = new Command()
  .description("Manage kamal-proxy")
  .command("logs", logsCommand);
