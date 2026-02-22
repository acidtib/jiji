import type {
  DependencyCheckResult,
  SecretAdapter,
  SecretAdapterConfig,
  SecretFetchResult,
} from "./adapter.ts";

const DOPPLER_TIMEOUT_MS = 30_000;
const DOPPLER_DEP_CHECK_TIMEOUT_MS = 10_000;

/** Doppler internal keys that are always injected and should be filtered out */
const DOPPLER_INTERNAL_KEYS = new Set([
  "DOPPLER_PROJECT",
  "DOPPLER_CONFIG",
  "DOPPLER_ENVIRONMENT",
]);

/**
 * Secret adapter for Doppler (https://www.doppler.com/)
 *
 * Auth is resolved in order:
 *  1. DOPPLER_TOKEN env var (service tokens, CI/CD)
 *  2. Interactive `doppler login` session
 *
 * When project/config are omitted from the jiji config the adapter
 * delegates to the Doppler CLI's own project resolution (doppler.yaml
 * or interactive selection).
 */
export class DopplerAdapter implements SecretAdapter {
  readonly name = "doppler";
  private project?: string;
  private config?: string;

  constructor(adapterConfig: SecretAdapterConfig) {
    this.project = adapterConfig.project;
    this.config = adapterConfig.config;
  }

  async checkDependencies(): Promise<DependencyCheckResult> {
    // Check if doppler CLI is installed
    try {
      const cmd = new Deno.Command("doppler", {
        args: ["--version"],
        stdout: "piped",
        stderr: "piped",
        signal: AbortSignal.timeout(DOPPLER_DEP_CHECK_TIMEOUT_MS),
      });
      const { success } = await cmd.output();
      if (!success) {
        return {
          satisfied: false,
          message: "Doppler CLI is installed but returned an error",
          suggestion: "Run 'doppler --version' to check your installation",
        };
      }
    } catch {
      return {
        satisfied: false,
        message: "Doppler CLI is not installed",
        suggestion:
          "Install it with: brew install dopplerhq/cli/doppler (macOS) or see https://docs.doppler.com/docs/install-cli",
      };
    }

    // Check authentication: DOPPLER_TOKEN env var or interactive login
    const hasToken = !!Deno.env.get("DOPPLER_TOKEN");
    if (hasToken) {
      return { satisfied: true };
    }

    try {
      const cmd = new Deno.Command("doppler", {
        args: ["me"],
        stdout: "piped",
        stderr: "piped",
        signal: AbortSignal.timeout(DOPPLER_DEP_CHECK_TIMEOUT_MS),
      });
      const { success } = await cmd.output();
      if (!success) {
        return {
          satisfied: false,
          message: "Doppler CLI is not authenticated",
          suggestion: "Set DOPPLER_TOKEN env var or run 'doppler login'",
        };
      }
    } catch {
      return {
        satisfied: false,
        message: "Failed to check Doppler authentication",
        suggestion: "Set DOPPLER_TOKEN env var or run 'doppler login'",
      };
    }

    return { satisfied: true };
  }

  async fetch(): Promise<SecretFetchResult> {
    const args = ["secrets", "download", "--no-file", "--format", "json"];
    if (this.project) {
      args.push("--project", this.project);
    }
    if (this.config) {
      args.push("--config", this.config);
    }

    const cmd = new Deno.Command("doppler", {
      args,
      stdout: "piped",
      stderr: "piped",
      signal: AbortSignal.timeout(DOPPLER_TIMEOUT_MS),
    });

    const { success, stdout, stderr } = await cmd.output();

    if (!success) {
      const errMsg = new TextDecoder().decode(stderr).trim();
      throw new Error(`Doppler secrets download failed: ${errMsg}`);
    }

    const raw = JSON.parse(new TextDecoder().decode(stdout)) as Record<
      string,
      unknown
    >;

    // Filter out Doppler internal keys and skip non-string values
    const variables: Record<string, string> = {};
    const warnings: string[] = [];
    for (const [key, value] of Object.entries(raw)) {
      if (DOPPLER_INTERNAL_KEYS.has(key)) continue;
      if (typeof value !== "string") {
        warnings.push(
          `Doppler secret '${key}' has non-string type (${typeof value}), skipping`,
        );
        continue;
      }
      variables[key] = value;
    }

    const sourceParts = ["Doppler"];
    if (this.project) sourceParts.push(`project: ${this.project}`);
    if (this.config) sourceParts.push(`config: ${this.config}`);
    const source = sourceParts.length > 1
      ? `${sourceParts[0]} (${sourceParts.slice(1).join(", ")})`
      : sourceParts[0];

    return { variables, source, warnings };
  }
}
