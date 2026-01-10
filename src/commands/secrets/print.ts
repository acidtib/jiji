import { Command } from "@cliffy/command";
import { colors } from "@cliffy/ansi/colors";
import { Configuration } from "../../lib/configuration.ts";
import { EnvLoader } from "../../utils/env_loader.ts";
import type { GlobalOptions } from "../../types.ts";
import { log } from "../../utils/logger.ts";

export const printCommand = new Command()
  .description("Print resolved secrets for debugging")
  .option(
    "--show-values",
    "Show actual secret values (use with caution)",
    { default: false },
  )
  .action(async (options) => {
    const globalOptions = options as unknown as GlobalOptions;

    try {
      // Load configuration
      const config = await Configuration.load(
        globalOptions.environment,
        globalOptions.configFile,
      );

      // Determine project root from config path
      const projectRoot = config.getProjectRoot();
      const allowHostEnv = globalOptions.hostEnv ?? false;

      // Load .env file
      const envResult = await EnvLoader.loadEnvFile({
        envPath: config.secretsPath,
        environment: config.environmentName,
        projectRoot,
        allowHostEnv,
      });

      log.section("Secrets Configuration:");
      log.say(`Environment: ${config.environmentName || "default"}`, 1);
      log.say(`Config file: ${config.configPath || "unknown"}`, 1);
      log.say(`Project root: ${projectRoot}`, 1);
      log.say(
        `Secrets file: ${envResult.loadedFrom || colors.yellow("not found")}`,
        1,
      );
      log.say(
        `Host env fallback: ${
          allowHostEnv ? colors.green("enabled") : colors.dim("disabled")
        }`,
        1,
      );

      // Show warnings from loading
      if (envResult.warnings.length > 0) {
        console.log("");
        log.section("Warnings:");
        for (const warning of envResult.warnings) {
          log.warn(warning);
        }
      }

      console.log("");

      // Parse --services filter if provided
      const serviceFilter = globalOptions.services
        ? globalOptions.services.split(",").map((s) => s.trim())
        : null;

      // Show global secrets
      const globalEnv = config.environment;
      const globalSecrets = [...globalEnv.secrets];

      // Also collect env var references from clear values
      for (const [_key, value] of Object.entries(globalEnv.clear)) {
        if (globalEnv.isEnvVarReference(value)) {
          if (!globalSecrets.includes(value)) {
            globalSecrets.push(value);
          }
        }
      }

      if (globalSecrets.length > 0) {
        log.section("Global Secrets:");
        printSecretsList(
          globalSecrets,
          envResult.variables,
          allowHostEnv,
          options.showValues,
        );
      }

      // Show per-service secrets
      const serviceNames = config.getServiceNames();
      const filteredServices = serviceFilter
        ? serviceNames.filter((name) =>
          serviceFilter.some((filter) => matchesFilter(name, filter))
        )
        : serviceNames;

      for (const serviceName of filteredServices) {
        const service = config.getService(serviceName);
        const serviceEnv = service.environment;
        const serviceSecrets = [...serviceEnv.secrets];

        // Also collect env var references from service clear values
        for (const [_key, value] of Object.entries(serviceEnv.clear)) {
          if (serviceEnv.isEnvVarReference(value)) {
            if (!serviceSecrets.includes(value)) {
              serviceSecrets.push(value);
            }
          }
        }

        if (serviceSecrets.length > 0) {
          console.log("");
          log.section(`Service '${serviceName}' Secrets:`);
          printSecretsList(
            serviceSecrets,
            envResult.variables,
            allowHostEnv,
            options.showValues,
          );
        }
      }

      // Show registry password if it's a secret (ALL_CAPS env var reference)
      const registry = config.builder.registry;
      const rawPassword = registry.password;
      if (rawPassword && /^[A-Z][A-Z0-9_]*$/.test(rawPassword)) {
        console.log("");
        log.section("Registry Password:");

        const value = envResult.variables[rawPassword] ??
          (allowHostEnv ? Deno.env.get(rawPassword) : undefined);

        const display = options.showValues && value
          ? value
          : (value ? colors.green("[SET]") : colors.red("[MISSING]"));

        log.say(`${rawPassword}: ${display}`, 1);
      }

      // Summary
      console.log("");
      const allSecrets = collectAllSecrets(config, globalEnv);
      const missingCount = countMissingSecrets(
        allSecrets,
        envResult.variables,
        allowHostEnv,
      );

      if (missingCount > 0) {
        log.warn(
          `${missingCount} secret(s) are missing. Deployment will fail until these are provided.`,
        );
        if (!allowHostEnv) {
          log.say(
            colors.dim(
              "Tip: Use --host-env flag to also check host environment variables",
            ),
          );
        }
      } else if (allSecrets.length > 0) {
        log.success("All secrets are configured correctly.", 0);
      }
    } catch (error) {
      log.error(
        `Failed to print secrets: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
      Deno.exit(1);
    }
  });

/**
 * Print a list of secrets with their status
 */
function printSecretsList(
  secrets: string[],
  envVars: Record<string, string>,
  allowHostEnv: boolean,
  showValues: boolean,
): void {
  for (const secret of secrets) {
    const value = envVars[secret] ??
      (allowHostEnv ? Deno.env.get(secret) : undefined);

    let display: string;
    if (value !== undefined) {
      display = showValues ? value : colors.green("[SET]");
    } else {
      display = colors.red("[MISSING]");
    }

    log.say(`${secret}: ${display}`, 1);
  }
}

/**
 * Check if a service name matches a filter (supports wildcards)
 */
function matchesFilter(name: string, filter: string): boolean {
  if (filter === "*") {
    return true;
  }
  if (filter.includes("*")) {
    const regex = new RegExp(
      "^" + filter.replace(/\*/g, ".*") + "$",
    );
    return regex.test(name);
  }
  return name === filter;
}

/**
 * Collect all unique secrets from config
 */
function collectAllSecrets(
  config: Configuration,
  globalEnv: {
    secrets: string[];
    clear: Record<string, string>;
    isEnvVarReference: (v: string) => boolean;
  },
): string[] {
  const allSecrets = new Set<string>();

  // Global secrets
  for (const secret of globalEnv.secrets) {
    allSecrets.add(secret);
  }
  for (const value of Object.values(globalEnv.clear)) {
    if (globalEnv.isEnvVarReference(value)) {
      allSecrets.add(value);
    }
  }

  // Service secrets
  for (const serviceName of config.getServiceNames()) {
    const service = config.getService(serviceName);
    for (const secret of service.environment.secrets) {
      allSecrets.add(secret);
    }
    for (const value of Object.values(service.environment.clear)) {
      if (service.environment.isEnvVarReference(value)) {
        allSecrets.add(value);
      }
    }
  }

  // Registry password (only if it's an ALL_CAPS env var reference)
  const rawPassword = config.builder.registry.password;
  if (rawPassword && /^[A-Z][A-Z0-9_]*$/.test(rawPassword)) {
    allSecrets.add(rawPassword);
  }

  return [...allSecrets];
}

/**
 * Count missing secrets
 */
function countMissingSecrets(
  secrets: string[],
  envVars: Record<string, string>,
  allowHostEnv: boolean,
): number {
  let count = 0;
  for (const secret of secrets) {
    const value = envVars[secret] ??
      (allowHostEnv ? Deno.env.get(secret) : undefined);
    if (value === undefined) {
      count++;
    }
  }
  return count;
}
