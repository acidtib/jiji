import { Command } from "@cliffy/command";
import { HelpCommand } from "@cliffy/command/help";
import { initCommand } from "./commands/init.ts";
import { serverCommand } from "./commands/server/index.ts";
import { auditCommand } from "./commands/audit.ts";
import { versionCommand } from "./commands/version.ts";

const command = new Command()
  .name("jiji")
  .description("Jiji - Infrastructure management tool")
  .option("-v, --verbose", "Detailed logging")
  .option(
    "--version=<VERSION:string>",
    "Run commands against a specific app version",
  )
  .option("-c, --config-file=<CONFIG_FILE:string>", "Path to config file", {
    default: "config/jiji.yml",
  })
  .option(
    "-e, --environment=<ENVIRONMENT:string>",
    "Specify environment to be used for config file (staging -> jiji.staging.yml)",
  )
  .action(() => {
    command.showHelp();
    Deno.exit(0);
  })
  .command("init", initCommand)
  .command("server", serverCommand)
  .command("audit", auditCommand)
  .command("version", versionCommand)
  .command("help", new HelpCommand());

await command.parse(Deno.args);
