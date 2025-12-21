import { Command } from "@cliffy/command";
import { initCommand } from "./init.ts";
import { execCommand } from "./exec.ts";

export const serverCommand = new Command()
  .description("Server management commands")
  .command("init", initCommand)
  .command("exec", execCommand);
