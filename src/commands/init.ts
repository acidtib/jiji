import { Command } from "@cliffy/command";
import { Confirm } from "@cliffy/prompt";
import type { GlobalOptions } from "../types.ts";
import { log } from "../utils/logger.ts";
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
  log.step(`Checking ${engine} availability`);

  const isAvailable = await checkEngineAvailability(engine);

  if (!isAvailable) {
    log.warn(`${engine} is not available on this system`);
    log.say(
      `Please install ${engine} or edit the config to use a different engine`,
      1,
    );
  } else {
    log.say(`${engine} is available`, 1);
  }
}

async function validateConfiguration(configPath: string): Promise<void> {
  try {
    log.step("Validating configuration");
    const validationResult = await Configuration.validateFile(configPath);

    if (validationResult.valid) {
      log.say("Configuration is valid", 1);

      if (validationResult.warnings.length > 0) {
        log.say(`Found ${validationResult.warnings.length} warning(s):`, 1);
        validationResult.warnings.forEach((warning) => {
          log.say(`- ${warning.path}: ${warning.message}`, 2);
        });
      }
    } else {
      log.error(
        `Configuration validation failed with ${validationResult.errors.length} error(s):`,
      );
      validationResult.errors.forEach((error) => {
        log.say(`- ${error.path}: ${error.message}`, 1);
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
    try {
      const tracker = log.createStepTracker("Jiji Configuration");
      const globalOptions = options as unknown as GlobalOptions;
      const configPath = buildConfigPath(globalOptions.environment);

      tracker.step("Setting up deployment configuration");
      log.say(`Target config: ${configPath}`, 1);

      const existingConfigs = await getAvailableConfigs();
      if (existingConfigs.length > 0) {
        log.say(
          `Found ${existingConfigs.length} existing configuration(s):`,
          1,
        );
        existingConfigs.forEach((config) => {
          log.say(`- ${config}`, 2);
        });
      }

      const fileExists = await configFileExists(configPath);
      if (fileExists) {
        log.warn(`Configuration already exists at ${configPath}`);

        const shouldOverwrite = await promptForOverwrite(configPath);
        if (!shouldOverwrite) {
          log.say("Init command cancelled by user");
          return;
        }

        log.say("Proceeding with overwrite", 1);
      }

      tracker.step("Loading default configuration template");
      const configTemplate = await readConfigTemplate();

      tracker.step("Creating configuration file");
      await createConfigFile(configPath, configTemplate);
      log.say(`Config file created at ${configPath}`, 1);

      await validateConfiguration(configPath);

      const templateLines = configTemplate.split("\n");
      const engineLine = templateLines.find((line) =>
        line.startsWith("engine:")
      );
      if (engineLine) {
        const engine = engineLine.split(":")[1].trim();
        await validateEngine(engine);
      }

      tracker.finish();

      log.section("Next Steps");
      log.step("Review and customize the configuration file");
      log.step("Configure your services and deployment targets");
      log.step("Set up any required environment variables or secrets");
      log.step("Run 'jiji server init' to prepare your servers");
      log.step("Run 'jiji deploy' to start deploying your services");

      console.log();
      log.say(
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
        log.say(
          "Configuration validation failed. Please check the template or try again.",
          1,
        );
      } else {
        log.say("Please check the error above and try again", 1);
      }

      Deno.exit(1);
    }
  });
