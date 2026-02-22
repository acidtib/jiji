import { Command } from "@cliffy/command";
import { logsCommand } from "./logs.ts";
import { rebootCommand } from "./reboot.ts";

export const proxyCommand = new Command()
  .description("Manage kamal-proxy")
  .command("logs", logsCommand)
  .command("reboot", rebootCommand);
