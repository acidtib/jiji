import { Command } from "@cliffy/command";

export const bootstrapCommand = new Command()
  .description("Bootstrap servers with curl and Podman or Docker")
  .action(() => {
    console.log("🖥️  Server bootstrap command called!");
    console.log("This will bootstrap servers with curl and Podman or Docker");
    console.log("(Not implemented yet)");
  });
