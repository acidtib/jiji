import { exists } from "@std/fs";
import { dirname, join } from "@std/path";

/**
 * Options for loading environment variables
 */
export interface EnvLoaderOptions {
  /** Custom path to .env file (relative to project root) */
  envPath?: string;
  /** Environment name (e.g., "staging", "production") */
  environment?: string;
  /** Root directory of the project (where .jiji/ folder is) */
  projectRoot: string;
  /** Whether to fallback to host environment variables */
  allowHostEnv?: boolean;
}

/**
 * Result of loading environment variables
 */
export interface EnvLoadResult {
  /** Loaded environment variables */
  variables: Record<string, string>;
  /** Path to the loaded .env file, or null if none found */
  loadedFrom: string | null;
  /** Any warnings during loading */
  warnings: string[];
}

/**
 * Environment variable loader
 * Handles parsing and loading .env files with environment-specific support
 */
export class EnvLoader {
  /**
   * Parse .env file content into a Record
   * Supports:
   * - KEY=value
   * - KEY="quoted value"
   * - KEY='single quoted value'
   * - # comments
   * - Empty lines
   * - Inline comments (after unquoted values)
   */
  static parseEnvFile(content: string): Record<string, string> {
    const result: Record<string, string> = {};
    const lines = content.split("\n");
    let i = 0;

    while (i < lines.length) {
      const trimmed = lines[i].trim();
      i++;

      // Skip empty lines and comments
      if (!trimmed || trimmed.startsWith("#")) {
        continue;
      }

      // Find the first = sign
      const eqIndex = trimmed.indexOf("=");
      if (eqIndex === -1) {
        continue; // Invalid line, skip
      }

      const key = trimmed.slice(0, eqIndex).trim();
      let value = trimmed.slice(eqIndex + 1);

      // Validate key is a valid env var name
      if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(key)) {
        continue; // Invalid key, skip
      }

      // Handle quoted values (including multiline)
      value = value.trim();
      if (value.startsWith('"') || value.startsWith("'")) {
        const quote = value[0];
        // Check if the closing quote is on the same line
        const rest = value.slice(1);
        const closeIdx = rest.indexOf(quote);
        if (closeIdx !== -1) {
          // Single-line quoted value
          value = rest.slice(0, closeIdx);
        } else {
          // Multiline quoted value — accumulate lines until closing quote
          const parts: string[] = [rest];
          while (i < lines.length) {
            const nextLine = lines[i];
            i++;
            const endIdx = nextLine.indexOf(quote);
            if (endIdx !== -1) {
              // Found the closing quote
              parts.push(nextLine.slice(0, endIdx));
              break;
            }
            parts.push(nextLine);
          }
          value = parts.join("\n");
        }
      } else {
        // Remove inline comments for unquoted values
        const commentIndex = value.indexOf(" #");
        if (commentIndex !== -1) {
          value = value.slice(0, commentIndex).trim();
        }
      }

      result[key] = value;
    }

    return result;
  }

  /**
   * Build the list of .env file paths to search for
   * Priority: .env.{environment} > .env
   */
  static buildEnvFilePaths(
    projectRoot: string,
    environment?: string,
    customPath?: string,
  ): string[] {
    const paths: string[] = [];

    if (customPath) {
      // If custom path specified, check environment-specific first
      if (environment) {
        paths.push(join(projectRoot, `${customPath}.${environment}`));
      }
      paths.push(join(projectRoot, customPath));
    } else {
      // Default: check project root for .env.{environment} then .env
      if (environment) {
        paths.push(join(projectRoot, `.env.${environment}`));
      }
      paths.push(join(projectRoot, ".env"));
    }

    return paths;
  }

  /**
   * Load environment variables from .env file
   * Searches for files in priority order and returns the first one found
   */
  static async loadEnvFile(options: EnvLoaderOptions): Promise<EnvLoadResult> {
    const { envPath, environment, projectRoot } = options;
    const warnings: string[] = [];

    const searchPaths = this.buildEnvFilePaths(
      projectRoot,
      environment,
      envPath,
    );

    for (const filePath of searchPaths) {
      try {
        if (await exists(filePath)) {
          const content = await Deno.readTextFile(filePath);
          const variables = this.parseEnvFile(content);

          return {
            variables,
            loadedFrom: filePath,
            warnings,
          };
        }
      } catch (error) {
        warnings.push(
          `Failed to read ${filePath}: ${
            error instanceof Error ? error.message : String(error)
          }`,
        );
      }
    }

    // No .env file found
    return {
      variables: {},
      loadedFrom: null,
      warnings,
    };
  }

  /**
   * Resolve a variable value from loaded env vars and optionally host environment
   */
  static resolveVariable(
    name: string,
    envVars: Record<string, string>,
    allowHostEnv: boolean,
  ): string | undefined {
    // First check loaded .env variables
    if (envVars[name] !== undefined) {
      return envVars[name];
    }

    // Then check host environment if allowed
    if (allowHostEnv) {
      return Deno.env.get(name);
    }

    return undefined;
  }

  /**
   * Check if a value looks like an environment variable reference
   * (ALL_CAPS_WITH_UNDERSCORES pattern)
   */
  static isEnvVarReference(value: string): boolean {
    return /^[A-Z][A-Z0-9_]*$/.test(value);
  }

  /**
   * Get project root from config path
   * Config is typically at .jiji/deploy.yml, so go up one level from .jiji/
   */
  static getProjectRootFromConfigPath(configPath: string): string {
    // configPath is like /path/to/project/.jiji/deploy.yml
    // We need /path/to/project
    const jijiDir = dirname(configPath);
    return dirname(jijiDir);
  }
}
