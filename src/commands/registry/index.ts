import { Command } from "@cliffy/command";
import { loginCommand } from "./login.ts";
import { logoutCommand } from "./logout.ts";
import { removeCommand } from "./remove.ts";
import { setupCommand } from "./setup.ts";

export const registryCommand = new Command()
  .description("Registry management commands")
  .command("login", loginCommand)
  .command("logout", logoutCommand)
  .command("remove", removeCommand)
  .command("setup", setupCommand);
