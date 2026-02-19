import { Command } from "@cliffy/command";
import { VERSION } from "../version.ts";
import { log } from "../utils/logger.ts";

export const versionCommand = new Command()
  .description("Show jiji version")
  .action(() => {
    log.section("Jiji Version:");
    log.say(`- ${VERSION}`, 1);
  });
