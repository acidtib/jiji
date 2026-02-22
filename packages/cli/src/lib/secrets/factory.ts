import type { SecretAdapter, SecretAdapterConfig } from "./adapter.ts";
import { DopplerAdapter } from "./doppler.ts";

/** Supported adapter names */
export const SUPPORTED_ADAPTERS = ["doppler"] as const;
export type SupportedAdapter = (typeof SUPPORTED_ADAPTERS)[number];

/**
 * Create a secret adapter instance from configuration
 *
 * @throws Error if the adapter name is not supported
 */
export function createSecretAdapter(
  config: SecretAdapterConfig,
): SecretAdapter {
  switch (config.adapter) {
    case "doppler":
      return new DopplerAdapter(config);
    default:
      throw new Error(
        `Unsupported secret adapter: '${config.adapter}'. Supported adapters: ${
          SUPPORTED_ADAPTERS.join(", ")
        }`,
      );
  }
}
