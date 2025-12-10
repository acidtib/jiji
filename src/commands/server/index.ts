import { Command } from "@cliffy/command";
import { bootstrapCommand } from "./bootstrap.ts";
import { execCommand } from "./exec.ts";

export const serverCommand = new Command()
  .description("Server management commands")
  .command("bootstrap", bootstrapCommand)
  .command("exec", execCommand);
