import type {
  DependencyCheckResult,
  SecretAdapter,
  SecretFetchResult,
} from "./adapter.ts";

/**
 * Mock secret adapter for testing
 */
export class MockSecretAdapter implements SecretAdapter {
  readonly name: string;
  private variables: Record<string, string>;
  private dependenciesSatisfied: boolean;
  private fetchError?: Error;

  constructor(
    options: {
      name?: string;
      variables?: Record<string, string>;
      dependenciesSatisfied?: boolean;
      fetchError?: Error;
    } = {},
  ) {
    this.name = options.name ?? "mock";
    this.variables = options.variables ?? {};
    this.dependenciesSatisfied = options.dependenciesSatisfied ?? true;
    this.fetchError = options.fetchError;
  }

  checkDependencies(): Promise<DependencyCheckResult> {
    return Promise.resolve({
      satisfied: this.dependenciesSatisfied,
      message: this.dependenciesSatisfied
        ? undefined
        : "Mock dependency not satisfied",
      suggestion: this.dependenciesSatisfied ? undefined : "Install mock",
    });
  }

  fetch(): Promise<SecretFetchResult> {
    if (this.fetchError) {
      return Promise.reject(this.fetchError);
    }
    return Promise.resolve({
      variables: { ...this.variables },
      source: `Mock (${this.name})`,
      warnings: [],
    });
  }
}
