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
  log.say(`- Checking ${engine} availability`, 1);

  const isAvailable = await checkEngineAvailability(engine);

  if (!isAvailable) {
    log.warn(`  ${engine} is not available on this system`);
    log.say(
      `  Please install ${engine} or edit the config to use a different engine`,
      2,
    );
  } else {
    log.say(`  ${engine} is available`, 2);
  }
}

async function validateConfiguration(configPath: string): Promise<void> {
  try {
    log.say("- Validating configuration", 1);
    const validationResult = await Configuration.validateFile(configPath);

    if (validationResult.valid) {
      log.say("  Configuration is valid", 2);

      if (validationResult.warnings.length > 0) {
        log.say(`  Found ${validationResult.warnings.length} warning(s):`, 2);
        validationResult.warnings.forEach((warning) => {
          log.say(`    ${warning.path}: ${warning.message}`, 3);
        });
      }
    } else {
      log.error(
        `Configuration validation failed with ${validationResult.errors.length} error(s):`,
      );
      validationResult.errors.forEach((error) => {
        log.say(`  ${error.path}: ${error.message}`, 2);
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
      log.section("Configuration Initialization:");

      const globalOptions = options as unknown as GlobalOptions;
      const configPath = buildConfigPath(globalOptions.environment);

      log.say(`- Target config: ${configPath}`, 1);

      const existingConfigs = await getAvailableConfigs();
      if (existingConfigs.length > 0) {
        log.say(
          `- Found ${existingConfigs.length} existing configuration(s):`,
          1,
        );
        existingConfigs.forEach((config) => {
          log.say(`  ${config}`, 2);
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

        log.say("- Proceeding with overwrite", 1);
      }

      log.section("Creating Configuration:");
      log.say("- Loading default configuration template", 1);
      const configTemplate = await readConfigTemplate();

      log.say("- Creating configuration file", 1);
      await createConfigFile(configPath, configTemplate);
      log.say(`- Config file created at ${configPath}`, 1);

      log.section("Validation:");
      await validateConfiguration(configPath);

      const templateLines = configTemplate.split("\n");
      const engineLine = templateLines.find((line) =>
        line.startsWith("engine:")
      );
      if (engineLine) {
        const engine = engineLine.split(":")[1].trim();
        await validateEngine(engine);
      }

      log.section("Next Steps:");
      log.say("- Review and customize the configuration file", 1);
      log.say("- Configure your services and deployment targets", 1);
      log.say("- Set up any required environment variables or secrets", 1);
      log.say("- Run 'jiji server init' to prepare your servers", 1);
      log.say("- Run 'jiji deploy' to start deploying your services", 1);

      log.success(
        `\nConfiguration file created: ${
          buildConfigPath((options as unknown as GlobalOptions).environment)
        }`,
        0,
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
