# Logger

A TypeScript/Deno logger utility. This logger provides structured, colorized
output with server prefixes, timestamps, and progress tracking.

## Features

- **Colorized Output**: Different colors for different log levels
- **Server Prefixes**: Identify which server or component is logging
- **Timestamps**: Optional millisecond-precision timestamps
- **Progress Tracking**: Built-in progress bars for long operations
- **Grouped Operations**: Section-based logging for related tasks
- **Configurable**: Customize colors, prefixes, and formatting
- **Child Loggers**: Create nested loggers with inherited configuration

## Quick Start

```typescript
import { log } from "./src/utils/logger.ts";

// Basic logging
log.info("Application starting");
log.success("Database connected");
log.warn("Config file missing");
log.error("Failed to load module");

// With server prefix
log.info("Deployment started", "web-1");
log.executing("docker pull myapp:latest", "web-1");
```

## API Reference

### Basic Logging Methods

```typescript
log.info(message: string, prefix?: string)     // Cyan colored
log.success(message: string, prefix?: string)  // Green colored
log.warn(message: string, prefix?: string)     // Yellow colored
log.error(message: string, prefix?: string)    // Red colored
log.debug(message: string, prefix?: string)    // Magenta colored
log.trace(message: string, prefix?: string)    // Gray colored
```

### Methods

```typescript
// Command execution logging
log.executing(command: string, server?: string)

// Status updates
log.status(message: string, server?: string)

// Progress tracking
log.progress(message: string, current: number, total: number, prefix?: string)

// Grouped operations
log.group(title: string, fn: () => void | Promise<void>)
```

### Logger Class

Create custom loggers with specific configurations:

```typescript
import { Logger } from "./src/utils/logger.ts";

const logger = new Logger({
  prefix: "deploy", // Default prefix for all logs
  showTimestamp: true, // Show timestamps (default: true)
  maxPrefixLength: 20, // Max prefix width (default: 20)
  colors: true, // Enable colors (default: true)
});
```

### Server-Specific Loggers

Create multiple loggers for different servers:

```typescript
const servers = ["web-1", "web-2", "db-primary"];
const loggers = Logger.forServers(servers);

loggers.get("web-1")?.info("Starting deployment");
loggers.get("web-2")?.success("Container started");
loggers.get("db-primary")?.executing("pg_dump database");
```

### Child Loggers

Create nested loggers with inherited configuration:

```typescript
const mainLogger = new Logger({ prefix: "app" });
const dbLogger = mainLogger.child("database");
const cacheLogger = mainLogger.child("cache");

mainLogger.info("Application starting");
dbLogger.info("Connecting to PostgreSQL");
cacheLogger.info("Connecting to Redis");
```

## Examples

### Deployment Logging

```typescript
import { log, Logger } from "./src/utils/logger.ts";

await log.group("Deploying Application", async () => {
  // Build phase
  log.executing("docker build -t myapp:v1.0.0 .", "local");
  await buildImage();
  log.success("Image built successfully", "local");

  // Deploy to servers
  const servers = ["web-1.prod", "web-2.prod"];
  const serverLoggers = Logger.forServers(servers);

  for (const server of servers) {
    const logger = serverLoggers.get(server)!;
    logger.executing("docker pull myapp:v1.0.0");
    logger.executing("docker stop myapp || true");
    logger.executing("docker run -d --name myapp myapp:v1.0.0");
    logger.success("Container started");
  }
});
```

### Progress Tracking

```typescript
// File upload with progress
for (let i = 0; i <= 100; i += 10) {
  log.progress("Uploading assets", i, 100, "cdn");
  await new Promise((resolve) => setTimeout(resolve, 100));
}
```

### Error Handling

```typescript
try {
  log.debug("Attempting database migration", "db");
  await runMigration();
  log.success("Migration completed", "db");
} catch (error) {
  log.error(`Migration failed: ${error.message}`, "db");
  log.trace("Check database connection and permissions", "db");
}
```

### Health Checks

```typescript
await log.group("Health Checks", async () => {
  const services = ["web", "api", "database", "cache"];

  for (const service of services) {
    log.info(`Checking ${service}...`, "health");
    const isHealthy = await checkServiceHealth(service);

    if (isHealthy) {
      log.success(`${service}: healthy`, "health");
    } else {
      log.error(`${service}: unhealthy`, "health");
    }
  }
});
```

## Output Examples

Here's what the logger output looks like:

```
12:34:56.789 deploy               [INFO ] Starting deployment process
12:34:56.801 web-1                [INFO ] $ docker pull myapp:latest
12:34:57.123 web-1                [INFO ] $ docker run -d --name myapp myapp:latest
12:34:57.456 web-1                [SUCC ] Container started successfully
12:34:57.789 upload               [INFO ] Uploading files [████████████████████] 100% (10/10)

────────────────────────────────────────────────────────────────
Deploying Application
────────────────────────────────────────────────────────────────
12:34:58.001 deploy               [INFO ] Preparing deployment
12:34:58.234 registry             [INFO ] Pushing to registry
12:34:58.567 deploy               [SUCC ] Deployment completed
────────────────────────────────────────────────────────────────
```

## Configuration Options

### LoggerOptions

```typescript
interface LoggerOptions {
  prefix?: string; // Default prefix for all log messages
  showTimestamp?: boolean; // Show timestamps (default: true)
  maxPrefixLength?: number; // Maximum prefix width (default: 20)
  colors?: boolean; // Enable colorized output (default: true)
}
```

### Log Levels

- `info`: General information (cyan)
- `success`: Success messages (green)
- `warn`: Warnings (yellow)
- `error`: Errors (red)
- `debug`: Debug information (magenta)
- `trace`: Trace information (gray)

## Running the Example

To see the logger in action, run the included example:

```bash
deno run --allow-all src/logger-example.ts
```

This will demonstrate all the logger features with simulated deployment
scenarios.

## Integration with Deno

The logger is built for Deno and uses:

- `std/fmt/colors` for colorization
- Native `console` methods for output
- TypeScript for type safety
