import { Command } from "@cliffy/command";
import { HelpCommand } from "@cliffy/command/help";
import { initCommand } from "./commands/init.ts";
import { serverCommand } from "./commands/server/index.ts";
import { auditCommand } from "./commands/audit.ts";
import { lockCommand } from "./commands/lock.ts";
import { versionCommand } from "./commands/version.ts";

const command = new Command()
  .name("jiji")
  .description("Jiji - Infrastructure management tool")
  .globalOption("-v, --verbose", "Detailed logging")
  .globalOption(
    "--version=<VERSION:string>",
    "Run commands against a specific app version",
  )
  .globalOption(
    "-c, --config-file=<CONFIG_FILE:string>",
    "Path to config file",
    {
      default: ".jiji/deploy.yml",
    },
  )
  .globalOption(
    "-e, --environment=<ENVIRONMENT:string>",
    "Specify environment to be used for config file (staging -> jiji.staging.yml)",
  )
  .globalOption(
    "-H, --hosts=<HOSTS:string>",
    "Run commands on these hosts instead of all (separate by comma, supports wildcards with *)",
  )
  .globalOption(
    "-S, --services=<SERVICES:string>",
    "Run commands on these services instead of all (separate by comma, supports wildcards with *)",
  )
  .action(() => {
    command.showHelp();
    Deno.exit(0);
  })
  .command("init", initCommand)
  .command("server", serverCommand)
  .command("audit", auditCommand)
  .command("lock", lockCommand)
  .command("version", versionCommand)
  .command("help", new HelpCommand());

await command.parse(Deno.args);
