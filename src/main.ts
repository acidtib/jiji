import { Command } from "@cliffy/command";
import { initCommand } from "./commands/init.ts";
import { serverCommand } from "./commands/server/index.ts";

await new Command()
  .name("jiji")
  .version("0.1.0")
  .description("Jiji CLI - Infrastructure management tool")
  .command("init", initCommand)
  .command("server", serverCommand)
  .parse(Deno.args);
