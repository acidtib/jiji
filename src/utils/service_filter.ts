import type { ServiceConfiguration } from "../lib/configuration/service.ts";

/**
 * Service filter options for targeting specific services during deployment
 */
export interface ServiceFilterOptions {
  /** Service names to include (exact matches) */
  services?: string[];
  /** Service name patterns to include (glob-style matching with * and ?) */
  patterns?: string[];
  /** Service names to exclude (takes precedence over includes) */
  exclude?: string[];
  /** Only services that require building */
  buildOnly?: boolean;
  /** Only services that use pre-built images */
  imageOnly?: boolean;
  /** Services that target specific hosts */
  hosts?: string[];
}

/**
 * Service grouping options for organizing deployment operations
 */
export interface ServiceGroupingOptions {
  /** Group services by their hosts for parallel deployment */
  groupByHosts?: boolean;
  /** Maximum number of services to deploy concurrently */
  maxConcurrent?: number;
  /** Services that should be deployed before others (dependencies) */
  dependencies?: Record<string, string[]>;
}

/**
 * Utility class for filtering and organizing services for deployment
 */
export class ServiceFilter {
  /**
   * Filter services based on the provided criteria
   */
  static filter(
    services: Map<string, ServiceConfiguration>,
    options: ServiceFilterOptions = {},
  ): Map<string, ServiceConfiguration> {
    const filtered = new Map<string, ServiceConfiguration>();

    for (const [name, service] of services) {
      if (this.matchesFilter(name, service, options)) {
        filtered.set(name, service);
      }
    }

    return filtered;
  }

  /**
   * Check if a service matches the filter criteria
   */
  private static matchesFilter(
    name: string,
    service: ServiceConfiguration,
    options: ServiceFilterOptions,
  ): boolean {
    // Check exclusions first (they take precedence)
    if (options.exclude?.includes(name)) {
      return false;
    }

    // Check service name inclusion
    if (options.services && !options.services.includes(name)) {
      return false;
    }

    // Check pattern matching
    if (options.patterns && !this.matchesAnyPattern(name, options.patterns)) {
      return false;
    }

    // Check build requirement filter
    if (options.buildOnly && !service.requiresBuild()) {
      return false;
    }

    // Check image-only filter
    if (options.imageOnly && service.requiresBuild()) {
      return false;
    }

    // Check host targeting
    if (options.hosts && !this.hasMatchingHosts(service, options.hosts)) {
      return false;
    }

    return true;
  }

  /**
   * Check if a service name matches any of the provided patterns
   */
  private static matchesAnyPattern(name: string, patterns: string[]): boolean {
    return patterns.some((pattern) => this.matchesPattern(name, pattern));
  }

  /**
   * Check if a name matches a glob-style pattern (* and ? wildcards)
   */
  private static matchesPattern(name: string, pattern: string): boolean {
    const regex = new RegExp(
      "^" + pattern.replace(/\*/g, ".*").replace(/\?/g, ".") + "$",
    );
    return regex.test(name);
  }

  /**
   * Check if service has any hosts that match the target hosts
   */
  private static hasMatchingHosts(
    service: ServiceConfiguration,
    targetHosts: string[],
  ): boolean {
    return service.hosts.some((host) => targetHosts.includes(host));
  }

  /**
   * Group services for optimal deployment ordering
   */
  static group(
    services: Map<string, ServiceConfiguration>,
    options: ServiceGroupingOptions = {},
  ): ServiceConfiguration[][] {
    if (options.groupByHosts) {
      return this.groupByHosts(services, options.maxConcurrent);
    }

    if (options.dependencies) {
      return this.groupByDependencies(services, options.dependencies);
    }

    // Default: group by max concurrent limit
    return this.groupByConcurrency(services, options.maxConcurrent || 10);
  }

  /**
   * Group services by their target hosts to enable host-parallel deployment
   */
  private static groupByHosts(
    services: Map<string, ServiceConfiguration>,
    maxConcurrent = 10,
  ): ServiceConfiguration[][] {
    const hostGroups = new Map<string, ServiceConfiguration[]>();

    // Group services by their first host (for simplicity)
    for (const service of services.values()) {
      const primaryHost = service.hosts[0];
      if (!hostGroups.has(primaryHost)) {
        hostGroups.set(primaryHost, []);
      }
      hostGroups.get(primaryHost)!.push(service);
    }

    // Convert to batches respecting concurrency limit
    const batches: ServiceConfiguration[][] = [];
    const hostGroupsArray = Array.from(hostGroups.values());

    for (let i = 0; i < hostGroupsArray.length; i += maxConcurrent) {
      const batch = hostGroupsArray.slice(i, i + maxConcurrent).flat();
      batches.push(batch);
    }

    return batches;
  }

  /**
   * Group services respecting dependency order
   */
  private static groupByDependencies(
    services: Map<string, ServiceConfiguration>,
    dependencies: Record<string, string[]>,
  ): ServiceConfiguration[][] {
    const servicesArray = Array.from(services.values());
    const resolved: Set<string> = new Set();
    const batches: ServiceConfiguration[][] = [];

    // Simple dependency resolution - services with no dependencies go first
    while (resolved.size < servicesArray.length) {
      const batch: ServiceConfiguration[] = [];

      for (const service of servicesArray) {
        if (resolved.has(service.name)) continue;

        const deps = dependencies[service.name] || [];
        const allDepsMet = deps.every((dep) => resolved.has(dep));

        if (allDepsMet) {
          batch.push(service);
        }
      }

      if (batch.length === 0) {
        // Circular dependency or missing service - add remaining services
        const remaining = servicesArray.filter((s) => !resolved.has(s.name));
        batch.push(...remaining);
      }

      // Mark all services in this batch as resolved
      for (const service of batch) {
        resolved.add(service.name);
      }

      batches.push(batch);
    }

    return batches;
  }

  /**
   * Group services into batches based on concurrency limit
   */
  private static groupByConcurrency(
    services: Map<string, ServiceConfiguration>,
    maxConcurrent: number,
  ): ServiceConfiguration[][] {
    const servicesArray = Array.from(services.values());
    const batches: ServiceConfiguration[][] = [];

    for (let i = 0; i < servicesArray.length; i += maxConcurrent) {
      batches.push(servicesArray.slice(i, i + maxConcurrent));
    }

    return batches;
  }

  /**
   * Create a human-readable summary of service selection
   */
  static createFilterSummary(
    allServices: Map<string, ServiceConfiguration>,
    filteredServices: Map<string, ServiceConfiguration>,
    options: ServiceFilterOptions,
  ): string {
    const lines: string[] = [];

    lines.push(`Service Selection Summary:`);
    lines.push(`  Total services: ${allServices.size}`);
    lines.push(`  Selected services: ${filteredServices.size}`);

    if (filteredServices.size > 0) {
      lines.push(
        `  Services: ${Array.from(filteredServices.keys()).join(", ")}`,
      );
    }

    if (options.services?.length) {
      lines.push(`  Filter by names: ${options.services.join(", ")}`);
    }

    if (options.patterns?.length) {
      lines.push(`  Filter by patterns: ${options.patterns.join(", ")}`);
    }

    if (options.exclude?.length) {
      lines.push(`  Excluded: ${options.exclude.join(", ")}`);
    }

    if (options.buildOnly) {
      lines.push(`  Filter: build-only services`);
    }

    if (options.imageOnly) {
      lines.push(`  Filter: image-only services`);
    }

    if (options.hosts?.length) {
      lines.push(`  Filter by hosts: ${options.hosts.join(", ")}`);
    }

    return lines.join("\n");
  }

  /**
   * Get all unique hosts across selected services
   */
  static getUniqueHosts(
    services: Map<string, ServiceConfiguration>,
  ): string[] {
    const hosts = new Set<string>();
    for (const service of services.values()) {
      service.hosts.forEach((host) => hosts.add(host));
    }
    return Array.from(hosts).sort();
  }

  /**
   * Get services that target a specific host
   */
  static getServicesForHost(
    services: Map<string, ServiceConfiguration>,
    host: string,
  ): ServiceConfiguration[] {
    return Array.from(services.values()).filter((service) =>
      service.hosts.includes(host)
    );
  }

  /**
   * Validate service filter options
   */
  static validateFilterOptions(options: ServiceFilterOptions): string[] {
    const errors: string[] = [];

    if (options.buildOnly && options.imageOnly) {
      errors.push("Cannot specify both buildOnly and imageOnly filters");
    }

    if (options.services?.some((name) => !name.trim())) {
      errors.push("Service names cannot be empty");
    }

    if (options.patterns?.some((pattern) => !pattern.trim())) {
      errors.push("Service patterns cannot be empty");
    }

    if (options.exclude?.some((name) => !name.trim())) {
      errors.push("Excluded service names cannot be empty");
    }

    if (options.hosts?.some((host) => !host.trim())) {
      errors.push("Host names cannot be empty");
    }

    return errors;
  }
}
