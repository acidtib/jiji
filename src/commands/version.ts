import { Command } from "@cliffy/command";
import { VERSION } from "../version.ts";

export const versionCommand = new Command()
  .description("Show jiji version")
  .action(() => {
    console.log(VERSION);
  });
