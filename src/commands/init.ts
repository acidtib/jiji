import { Command } from "@cliffy/command";
import { Confirm } from "@cliffy/prompt";
import type { GlobalOptions } from "../types.ts";
import { log, Logger } from "../utils/logger.ts";
import {
  buildConfigPath,
  checkEngineAvailability,
  configFileExists,
  createConfigFile,
  getAvailableConfigs,
  readConfigTemplate,
} from "../utils/config.ts";
import { Configuration, ConfigurationError } from "../lib/configuration.ts";

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
    initLogger.success(`${engine} is available`);
  }
}

async function validateConfiguration(configPath: string): Promise<void> {
  const initLogger = new Logger({ prefix: "init" });

  try {
    initLogger.info("Validating configuration...");
    const validationResult = await Configuration.validateFile(configPath);

    if (validationResult.valid) {
      initLogger.success("Configuration is valid");

      if (validationResult.warnings.length > 0) {
        initLogger.warn(
          `Found ${validationResult.warnings.length} warning(s):`,
        );
        validationResult.warnings.forEach((warning) => {
          initLogger.warn(`  - ${warning.path}: ${warning.message}`);
        });
      }
    } else {
      initLogger.error(
        `Configuration validation failed with ${validationResult.errors.length} error(s):`,
      );
      validationResult.errors.forEach((error) => {
        initLogger.error(`  - ${error.path}: ${error.message}`);
      });
      throw new Error("Configuration validation failed");
    }
  } catch (error) {
    if (error instanceof ConfigurationError) {
      throw error;
    }
    throw new Error(
      `Configuration validation error: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
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

        // Check for existing configurations
        const existingConfigs = await getAvailableConfigs();
        if (existingConfigs.length > 0) {
          initLogger.info(
            `Found ${existingConfigs.length} existing configuration(s):`,
          );
          existingConfigs.forEach((config) => {
            initLogger.info(`  - ${config}`);
          });
        }

        // Check if target config file exists and handle accordingly
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

        // Use default template
        initLogger.info("Loading default configuration template...");
        const configTemplate = await readConfigTemplate();

        initLogger.info("Creating configuration file...");
        await createConfigFile(configPath, configTemplate);

        initLogger.success(`Config file created at ${configPath}`);

        // Validate the template configuration
        await validateConfiguration(configPath);

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
      log.info("  1. Review and customize the configuration file");
      log.info("  2. Configure your services and deployment targets");
      log.info("  3. Set up any required environment variables or secrets");
      log.info("  4. Run 'jiji server init' to prepare your servers");
      log.info("  5. Run 'jiji deploy' to start deploying your services");

      console.log();
      log.info(
        `Configuration file: ${
          buildConfigPath((options as unknown as GlobalOptions).environment)
        }`,
      );
    } catch (error) {
      console.log();
      log.error(
        `Initialization failed: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );

      if (error instanceof ConfigurationError) {
        log.warn(
          "Configuration validation failed. Please check the template or try again.",
        );
      } else {
        log.warn("Please check the error above and try again");
      }

      Deno.exit(1);
    }
  });
