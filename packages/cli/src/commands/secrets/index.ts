import { Command } from "@cliffy/command";
import { printCommand } from "./print.ts";

export const secretsCommand = new Command()
  .description("Secrets management commands")
  .action(function () {
    this.showHelp();
  })
  .command("print", printCommand);
