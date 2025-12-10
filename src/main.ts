import { Command } from "@cliffy/command";
import { initCommand } from "./commands/init.ts";
import { serverCommand } from "./commands/server/index.ts";
import { auditCommand } from "./commands/audit.ts";
import { VERSION } from "./version.ts";

await new Command()
  .name("jiji")
  .version(VERSION)
  .description("Jiji CLI - Infrastructure management tool")
  .command("init", initCommand)
  .command("server", serverCommand)
  .command("audit", auditCommand)
  .parse(Deno.args);
