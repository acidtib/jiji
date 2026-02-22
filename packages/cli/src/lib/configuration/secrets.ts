import {
  BaseConfiguration,
  ConfigurationError,
  type Validatable,
} from "./base.ts";
import { SUPPORTED_ADAPTERS } from "../secrets/factory.ts";
import type { SecretAdapterConfig } from "../secrets/adapter.ts";

/**
 * Configuration for external secret adapters (e.g., Doppler)
 *
 * Parsed from the top-level `secrets` key in deploy.yml:
 *
 * ```yaml
 * secrets:
 *   adapter: doppler
 *   project: my-app
 *   config: prd
 * ```
 */
export class SecretsConfiguration extends BaseConfiguration
  implements Validatable {
  private _adapter?: string;
  private _project?: string;
  private _configName?: string;

  constructor(config: Record<string, unknown> = {}) {
    super(config);
  }

  /** Adapter name (e.g., "doppler") */
  get adapter(): string {
    if (!this._adapter) {
      this._adapter = this.getRequired<string>("adapter");
      this.validateString(this._adapter, "adapter", "secrets");
    }
    return this._adapter;
  }

  /** Provider-specific project name */
  get project(): string | undefined {
    if (this._project === undefined && this.has("project")) {
      this._project = this.validateString(
        this.get("project"),
        "project",
        "secrets",
      );
    }
    return this._project;
  }

  /** Provider-specific config/environment name */
  get configName(): string | undefined {
    if (this._configName === undefined && this.has("config")) {
      this._configName = this.validateString(
        this.get("config"),
        "config",
        "secrets",
      );
    }
    return this._configName;
  }

  /** Whether a secrets adapter is configured */
  get isConfigured(): boolean {
    return this.has("adapter");
  }

  /** Convert to SecretAdapterConfig for passing to the factory */
  toAdapterConfig(): SecretAdapterConfig {
    return {
      adapter: this.adapter,
      project: this.project,
      config: this.configName,
    };
  }

  validate(): void {
    if (!this.isConfigured) return;

    const adapter = this.adapter;
    if (
      !SUPPORTED_ADAPTERS.includes(adapter as typeof SUPPORTED_ADAPTERS[number])
    ) {
      throw new ConfigurationError(
        `Unsupported secret adapter: '${adapter}'. Supported adapters: ${
          SUPPORTED_ADAPTERS.join(", ")
        }`,
      );
    }
  }
}
