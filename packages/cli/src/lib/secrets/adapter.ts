/**
 * Result of checking whether an adapter's dependencies are satisfied
 */
export interface DependencyCheckResult {
  /** Whether all dependencies are satisfied */
  satisfied: boolean;
  /** Human-readable message explaining the check result */
  message?: string;
  /** Suggestion for how to satisfy the dependency */
  suggestion?: string;
}

/**
 * Result of fetching secrets from an adapter
 */
export interface SecretFetchResult {
  /** Fetched secret variables */
  variables: Record<string, string>;
  /** Human-readable source description (e.g., "Doppler (project: my-app, config: prd)") */
  source: string;
  /** Any warnings during fetching */
  warnings: string[];
}

/**
 * Configuration passed to a secret adapter
 */
export interface SecretAdapterConfig {
  /** Adapter name (e.g., "doppler") */
  adapter: string;
  /** Provider-specific project name */
  project?: string;
  /** Provider-specific config/environment name */
  config?: string;
}

/**
 * Interface for secret provider adapters
 */
export interface SecretAdapter {
  /** Adapter name for display/logging */
  readonly name: string;

  /** Check whether the adapter's external dependencies are available */
  checkDependencies(): Promise<DependencyCheckResult>;

  /** Fetch secrets from the provider */
  fetch(): Promise<SecretFetchResult>;
}
