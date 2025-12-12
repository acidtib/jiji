import { Command } from "@cliffy/command";
import { Confirm } from "@cliffy/prompt";
import type { GlobalOptions } from "../types.ts";
import {
  buildConfigPath,
  configFileExists,
  createConfigFile,
  readConfigTemplate,
} from "../utils/config.ts";

async function promptForOverwrite(configPath: string): Promise<boolean> {
  return await Confirm.prompt({
    message: `Config file already exists at ${configPath}. Overwrite it?`,
    default: false,
  });
}

export const initCommand = new Command()
  .description("Create config stub in config/jiji.yml")
  .action(async (options) => {
    console.log("Initializing jiji configuration...");

    try {
      const globalOptions = options as unknown as GlobalOptions;
      const configPath = buildConfigPath(globalOptions.environment);

      console.log(`Creating config: ${configPath}`);

      // Check if config file exists and handle accordingly
      const fileExists = await configFileExists(configPath);
      if (fileExists) {
        const shouldOverwrite = await promptForOverwrite(configPath);
        if (!shouldOverwrite) {
          console.log("Init command cancelled by user.");
          return;
        }
      }

      // Read template and create config file
      const configTemplate = await readConfigTemplate();
      await createConfigFile(configPath, configTemplate);

      console.log(`Config file created successfully at ${configPath}`);
    } catch (error) {
      console.error(
        "Error:",
        error instanceof Error ? error.message : String(error),
      );
      Deno.exit(1);
    }
  });
