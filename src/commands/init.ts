import { Command } from "@cliffy/command";
import { Confirm } from "@cliffy/prompt";
import type { GlobalOptions } from "../types.ts";
import { log, Logger } from "../utils/logger.ts";
import {
  buildConfigPath,
  checkEngineAvailability,
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

async function validateEngine(engine: string): Promise<void> {
  const initLogger = new Logger({ prefix: "init" });

  initLogger.info(`Checking ${engine} availability...`);

  const isAvailable = await checkEngineAvailability(engine);

  if (!isAvailable) {
    initLogger.warn(`${engine} is not available on this system`);
    initLogger.info(
      `Please install ${engine} or edit the config to use a different engine`,
    );
  } else {
    initLogger.success(`${engine} is available ✓`);
  }
}

export const initCommand = new Command()
  .description("Create config stub in .jiji/deploy.yml")
  .action(async (options) => {
    const initLogger = new Logger({ prefix: "init" });

    try {
      await log.group("Initializing Jiji Configuration", async () => {
        const globalOptions = options as unknown as GlobalOptions;
        const configPath = buildConfigPath(globalOptions.environment);

        initLogger.info("Setting up deployment configuration...");
        initLogger.status(`Target config: ${configPath}`, "config");

        // Check if config file exists and handle accordingly
        const fileExists = await configFileExists(configPath);
        if (fileExists) {
          initLogger.warn(`Configuration already exists at ${configPath}`);

          const shouldOverwrite = await promptForOverwrite(configPath);
          if (!shouldOverwrite) {
            initLogger.info("Init command cancelled by user");
            return;
          }

          initLogger.info("Proceeding with overwrite...");
        }

        // Read template and create config file
        initLogger.info("Loading configuration template...");
        const configTemplate = await readConfigTemplate();

        initLogger.info("Creating configuration file...");
        await createConfigFile(configPath, configTemplate);

        initLogger.success(`Config file created at ${configPath}`);

        // Parse and validate the template to check the engine
        const templateLines = configTemplate.split("\n");
        const engineLine = templateLines.find((line) =>
          line.startsWith("engine:")
        );
        if (engineLine) {
          const engine = engineLine.split(":")[1].trim();
          await validateEngine(engine);
        }
      });

      // Final success message
      console.log();
      log.success("Jiji configuration initialized successfully!");
      log.info("Next steps:");
      log.info("  1. Edit the config file to match your deployment needs");
      log.info("  2. Configure your services and hosts");
      log.info("  3. Run 'jiji server bootstrap' to setup the server");
      log.info("  4. Run 'jiji deploy' to start deploying");
    } catch (error) {
      console.log();
      log.error(
        `❌ Initialization failed: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
      log.warn("Please check the error above and try again");
      Deno.exit(1);
    }
  });
