import { EnvLoader } from "./env_loader.ts";
import type { EnvLoaderOptions } from "./env_loader.ts";
import type { SecretsConfiguration } from "../lib/configuration/secrets.ts";
import { createSecretAdapter } from "../lib/secrets/factory.ts";
import type { SecretAdapter } from "../lib/secrets/adapter.ts";
import { log } from "./logger.ts";

/**
 * Result of resolving secrets from all sources
 */
export interface SecretResolutionResult {
  /** Merged variables (adapter + .env, with .env taking precedence) */
  variables: Record<string, string>;
  /** Path to the loaded .env file, or null */
  loadedFrom: string | null;
  /** Source description of the adapter, or null if no adapter */
  adapterSource: string | null;
  /** Combined warnings from all sources */
  warnings: string[];
}

/**
 * Resolve secrets by loading .env file and optionally fetching from a secret adapter.
 *
 * Merge strategy: .env variables override adapter variables (allows local overrides).
 * When no adapter is configured, this is a thin wrapper around EnvLoader.loadEnvFile().
 */
export async function resolveSecrets(
  envOptions: EnvLoaderOptions,
  secretsConfig?: SecretsConfiguration,
  /** Inject adapter for testing — bypasses factory creation */
  adapterOverride?: SecretAdapter,
): Promise<SecretResolutionResult> {
  const warnings: string[] = [];

  // 1. Load .env file (existing behavior)
  const envResult = await EnvLoader.loadEnvFile(envOptions);
  warnings.push(...envResult.warnings);

  // 2. Short-circuit if no adapter configured
  if (!secretsConfig?.isConfigured && !adapterOverride) {
    return {
      variables: envResult.variables,
      loadedFrom: envResult.loadedFrom,
      adapterSource: null,
      warnings,
    };
  }

  // 3. Create adapter (or use injected one)
  const adapter = adapterOverride ??
    createSecretAdapter(secretsConfig!.toAdapterConfig());

  // 4. Check dependencies
  const depCheck = await adapter.checkDependencies();
  if (!depCheck.satisfied) {
    const parts = [`Secret adapter '${adapter.name}' dependency check failed`];
    if (depCheck.message) parts.push(depCheck.message);
    if (depCheck.suggestion) parts.push(depCheck.suggestion);
    throw new Error(parts.join(": "));
  }

  // 5. Fetch from adapter
  log.debug(`Fetching secrets from ${adapter.name} adapter...`, "secrets");
  const adapterResult = await adapter.fetch();
  warnings.push(...adapterResult.warnings);

  const adapterCount = Object.keys(adapterResult.variables).length;
  log.debug(
    `Fetched ${adapterCount} secret(s) from ${adapterResult.source}`,
    "secrets",
  );

  // 6. Merge: adapter first, then .env on top (.env wins)
  const merged: Record<string, string> = {
    ...adapterResult.variables,
    ...envResult.variables,
  };

  return {
    variables: merged,
    loadedFrom: envResult.loadedFrom,
    adapterSource: adapterResult.source,
    warnings,
  };
}
