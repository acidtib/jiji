import { Command } from "@cliffy/command";
import { HelpCommand } from "@cliffy/command/help";
import { initCommand } from "./commands/init.ts";
import { serverCommand } from "./commands/server/index.ts";
import { auditCommand } from "./commands/audit.ts";
import { VERSION } from "./version.ts";

const command = new Command()
  .name("jiji")
  .version(VERSION)
  .description("Jiji")
  .action(() => {
    command.showHelp();
    Deno.exit(0);
  })
  .command("init", initCommand)
  .command("server", serverCommand)
  .command("audit", auditCommand)
  .command("help", new HelpCommand());

await command.parse(Deno.args);
