import { Command } from "@cliffy/command";

export const initCommand = new Command()
  .description("Create config stub in config/jiji.yml")
  .action(() => {
    console.log("🚀 Init command called!");
    console.log("This will create a config stub in config/jiji.yml");
    console.log("(Not implemented yet)");
  });
