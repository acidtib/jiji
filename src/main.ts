import { Command } from "@cliffy/command";
import { HelpCommand } from "@cliffy/command/help";
import { initCommand } from "./commands/init.ts";
import { serverCommand } from "./commands/server/index.ts";
import { auditCommand } from "./commands/audit.ts";
import { lockCommand } from "./commands/lock.ts";
import { versionCommand } from "./commands/version.ts";
import { deployCommand } from "./commands/deploy.ts";
import { buildCommand } from "./commands/build.ts";
import { removeCommand } from "./commands/remove.ts";
import { servicesCommand } from "./commands/services/index.ts";
import { proxyCommand } from "./commands/proxy/index.ts";
import { registryCommand } from "./commands/registry/index.ts";
import { networkCommand } from "./commands/network.ts";
import { setGlobalLogLevel, setGlobalQuietMode } from "./utils/logger.ts";

const command = new Command()
  .name("jiji")
  .description("Jiji - Infrastructure management tool")
  .globalOption("-v, --verbose", "Detailed logging")
  .globalOption(
    "-q, --quiet",
    "Minimal output (suppress host headers and extra messages)",
  )
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
  .command("build", buildCommand)
  .command("deploy", deployCommand)
  .command("remove", removeCommand)
  .command("services", servicesCommand)
  .command("proxy", proxyCommand)
  .command("server", serverCommand)
  .command("registry", registryCommand)
  .command("network", networkCommand)
  .command("audit", auditCommand)
  .command("lock", lockCommand)
  .command("version", versionCommand)
  .command("help", new HelpCommand());

// Parse arguments
const options = await command.parse(Deno.args);

// Set global log level based on --verbose flag
if (options.options.verbose) {
  setGlobalLogLevel("debug");
}

// Set global quiet mode based on --quiet flag
if (options.options.quiet) {
  setGlobalQuietMode(true);
  // In quiet mode, only show errors and warnings
  if (!options.options.verbose) {
    setGlobalLogLevel("warn");
  }
}
