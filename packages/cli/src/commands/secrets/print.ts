import { Command } from "@cliffy/command";
import { colors } from "@cliffy/ansi/colors";
import { Configuration } from "../../lib/configuration.ts";
import { resolveSecrets } from "../../utils/secret_resolver.ts";
import { EnvLoader } from "../../utils/env_loader.ts";
import type { GlobalOptions } from "../../types.ts";
import { log } from "../../utils/logger.ts";

/**
 * A secret reference found in the configuration, with context about where it came from
 */
interface SecretRef {
  /** The env var name (e.g., "DATABASE_PASSWORD") */
  name: string;
  /** Where in the config it was found (e.g., "services.web.proxy.ssl") */
  source: string;
}

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

      // Load .env file and optional secret adapter
      const secretResult = await resolveSecrets(
        {
          envPath: config.secretsPath,
          environment: config.environmentName,
          projectRoot,
          allowHostEnv,
        },
        config.secretsAdapter,
      );

      log.section("Secrets Configuration:");
      log.say(`Environment: ${config.environmentName || "default"}`, 1);
      log.say(`Config file: ${config.configPath || "unknown"}`, 1);
      log.say(`Project root: ${projectRoot}`, 1);
      log.say(
        `Secrets file: ${
          secretResult.loadedFrom || colors.yellow("not found")
        }`,
        1,
      );
      if (secretResult.adapterSource) {
        log.say(
          `Secret adapter: ${colors.green(secretResult.adapterSource)}`,
          1,
        );
      }
      log.say(
        `Host env fallback: ${
          allowHostEnv ? colors.green("enabled") : colors.dim("disabled")
        }`,
        1,
      );

      // Show warnings from loading
      if (secretResult.warnings.length > 0) {
        console.log("");
        log.section("Warnings:");
        for (const warning of secretResult.warnings) {
          log.warn(warning);
        }
      }

      // Parse --services filter if provided
      const serviceFilter = globalOptions.services
        ? globalOptions.services.split(",").map((s) => s.trim())
        : null;

      // Collect ALL env var references from the config
      const refs = collectAllRefs(config, serviceFilter);

      // Group refs by source for display
      const grouped = new Map<string, SecretRef[]>();
      for (const ref of refs) {
        if (!grouped.has(ref.source)) {
          grouped.set(ref.source, []);
        }
        grouped.get(ref.source)!.push(ref);
      }

      // Display each group
      for (const [source, groupRefs] of grouped) {
        console.log("");
        log.section(`${source}:`);
        const names = groupRefs.map((r) => r.name);
        printSecretsList(
          names,
          secretResult.variables,
          allowHostEnv,
          options.showValues,
        );
      }

      // Summary
      console.log("");
      const uniqueNames = new Set(refs.map((r) => r.name));
      const missingCount = countMissing(
        [...uniqueNames],
        secretResult.variables,
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
      } else if (uniqueNames.size > 0) {
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
 * Collect ALL env var references from the entire configuration
 */
function collectAllRefs(
  config: Configuration,
  serviceFilter: string[] | null,
): SecretRef[] {
  const refs: SecretRef[] = [];
  const rawConfig = config.getRawConfig();

  // 1. Server host env var references
  const serversRaw = rawConfig.servers as Record<string, unknown> | undefined;
  if (serversRaw) {
    for (const [name, serverVal] of Object.entries(serversRaw)) {
      if (serverVal && typeof serverVal === "object") {
        const server = serverVal as Record<string, unknown>;
        addIfEnvRef(refs, server.host, `servers.${name}.host`);
        addIfEnvRef(refs, server.key_passphrase, `servers.${name}.ssh`);
        if (Array.isArray(server.key_data)) {
          for (const entry of server.key_data) {
            addIfEnvRef(refs, entry, `servers.${name}.ssh`);
          }
        }
      }
    }
  }

  // 2. Global SSH key_passphrase and key_data
  const sshRaw = rawConfig.ssh as Record<string, unknown> | undefined;
  if (sshRaw) {
    addIfEnvRef(refs, sshRaw.key_passphrase, "ssh");
    if (Array.isArray(sshRaw.key_data)) {
      for (const entry of sshRaw.key_data) {
        addIfEnvRef(refs, entry, "ssh.key_data");
      }
    }
  }

  // 3. Registry password
  const builderRaw = rawConfig.builder as Record<string, unknown> | undefined;
  if (builderRaw) {
    const registryRaw = builderRaw.registry as
      | Record<string, unknown>
      | undefined;
    if (registryRaw) {
      addIfEnvRef(refs, registryRaw.password, "builder.registry");
    }
  }

  // 4. Global environment secrets
  const globalEnv = config.environment;
  for (const secret of globalEnv.secrets) {
    refs.push({ name: secret, source: "environment.secrets" });
  }

  // 5. Per-service: environment secrets, proxy SSL certs, build args
  const serviceNames = config.getServiceNames();
  const filteredServices = serviceFilter
    ? serviceNames.filter((name) =>
      serviceFilter.some((f) => matchesFilter(name, f))
    )
    : serviceNames;

  for (const serviceName of filteredServices) {
    const service = config.getService(serviceName);
    const prefix = `services.${serviceName}`;

    // Environment secrets
    for (const secret of service.environment.secrets) {
      refs.push({ name: secret, source: `${prefix}.environment.secrets` });
    }

    // Proxy SSL certs
    if (service.proxy) {
      for (const target of service.proxy.targets) {
        if (target.ssl && typeof target.ssl === "object") {
          const certs = target.ssl as {
            certificate_pem: string;
            private_key_pem: string;
          };
          addIfEnvRef(refs, certs.certificate_pem, `${prefix}.proxy.ssl`);
          addIfEnvRef(refs, certs.private_key_pem, `${prefix}.proxy.ssl`);
        }
      }
    }

    // Build args that are env var references
    if (service.build && typeof service.build === "object") {
      const build = service.build as { args?: Record<string, string> };
      if (build.args) {
        for (const [key, value] of Object.entries(build.args)) {
          if (EnvLoader.isEnvVarReference(value)) {
            refs.push({
              name: value,
              source: `${prefix}.build.args.${key}`,
            });
          }
        }
      }
    }
  }

  return refs;
}

/**
 * Add a ref if the value matches the ALL_CAPS env var pattern
 */
function addIfEnvRef(
  refs: SecretRef[],
  value: unknown,
  source: string,
): void {
  if (typeof value === "string" && EnvLoader.isEnvVarReference(value)) {
    refs.push({ name: value, source });
  }
}

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
 * Count missing secrets
 */
function countMissing(
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
