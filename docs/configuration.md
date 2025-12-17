# Jiji Configuration System

## Architecture

### Core Classes

#### `Configuration`

The main configuration class that orchestrates all sub-configurations:

```typescript
import { Configuration } from "./src/lib/configuration.ts";

// Load from file
const config = await Configuration.load();

// Access properties
console.log(config.engine); // "podman" | "docker"
console.log(config.ssh.user); // SSH user
console.log(config.getServiceNames()); // ["web", "api", "db"]
```

#### `SSHConfiguration`

Handles SSH connection settings:

```typescript
const ssh = config.ssh;
console.log(ssh.user); // SSH user
console.log(ssh.port); // SSH port (default: 22)
console.log(ssh.keyPath); // Optional SSH key path
console.log(ssh.buildSSHArgs("hostname")); // Returns SSH command args
```

#### `ServiceConfiguration`

Manages individual service definitions:

```typescript
const webService = config.getService("web");
console.log(webService.image); // Docker/Podman image
console.log(webService.hosts); // Deployment hosts
console.log(webService.ports); // Port mappings
console.log(webService.requiresBuild()); // true if uses build config
```

#### `EnvironmentConfiguration`

Handles environment variables and secrets:

```typescript
const env = config.environment;
console.log(env.variables); // Environment variables
console.log(env.secrets); // Secret names to load
const resolved = await env.resolveVariables(); // Resolves secrets from env
```

### Configuration Loading

The system supports multiple configuration file patterns:

```
.jiji/
├── deploy.yml              # Default configuration
├── deploy.production.yml    # Production environment
├── deploy.staging.yml       # Staging environment
├── production.yml           # Alternative production config
└── staging.yml             # Alternative staging config
```

Load configurations:

```typescript
// Load default configuration
const config = await Configuration.load();

// Load environment-specific configuration
const prodConfig = await Configuration.load("production");

// Load from specific file
const config = await Configuration.load(undefined, "/path/to/config.yml");
```

## Configuration File Format

### Basic Structure

```yaml
# Container engine to use
engine: podman # or "docker"

# SSH configuration
ssh:
  user: root
  port: 22
  # Optional settings
  key_path: ~/.ssh/id_rsa
  key_passphrase: "secret"
  connect_timeout: 30
  command_timeout: 300
  options:
    StrictHostKeyChecking: "no"

# Services configuration
services:
  web:
    image: nginx:latest
    hosts:
      - 192.168.1.100
      - 192.168.1.101
    ports:
      - "80:80"
      - "443:443"
    volumes:
      - "/etc/nginx:/etc/nginx:ro"
    environment:
      NGINX_HOST: example.com
    restart: always
    labels:
      app: web
      version: "1.0"

# Environment configuration
env:
  variables:
    APP_ENV: production
    LOG_LEVEL: info
  secrets:
    - DATABASE_PASSWORD
    - API_SECRET_KEY
  files:
    SSL_CERT: /etc/ssl/cert.pem
```

### Service Configuration Options

#### Image-based Service

```yaml
services:
  web:
    image: nginx:latest
    hosts:
      - server1.example.com
    ports:
      - "80:80"
    restart: always
```

#### Build-based Service

```yaml
services:
  app:
    build:
      context: ./app
      dockerfile: Dockerfile
      args:
        NODE_ENV: production
      target: production
    hosts:
      - server1.example.com
    ports:
      - "3000:3000"
    depends_on:
      - database
```

#### Advanced Service Configuration

```yaml
services:
  api:
    image: myapp/api:latest
    hosts:
      - api-server1.example.com
      - api-server2.example.com
    ports:
      - "3000:3000"
    volumes:
      - "/app/logs:/var/log/app"
    environment:
      NODE_ENV: production
      DATABASE_URL: postgresql://user:pass@db:5432/app
    working_dir: /app
    command: ["node", "server.js"]
    restart: unless-stopped
    networks:
      - app-network
    labels:
      traefik.enable: "true"
      traefik.http.routers.api.rule: "Host(`api.example.com`)"
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:3000/health"]
      interval: "30s"
      timeout: "10s"
      retries: 3
      start_period: "60s"
```

## Validation System

The configuration system includes comprehensive validation:

### Built-in Validations

- **Engine**: Must be "docker" or "podman"
- **SSH User**: Required string
- **SSH Port**: Valid port number (1-65535)
- **Services**: Must have at least one service
- **Service Image/Build**: Must specify either image or build (not both)
- **Hosts**: Must specify at least one host per service
- **Ports**: Valid port mapping format
- **Dependencies**: Referenced services must exist
- **Circular Dependencies**: Automatically detected and prevented

### Custom Validation

You can add custom validation rules:

```typescript
import {
  ConfigurationValidator,
  ValidationRules,
} from "./src/lib/configuration.ts";

const validator = new ConfigurationValidator();

// Add custom rule
validator.addRule(
  "services.web.image",
  ValidationRules.pattern(/^nginx:/, "Web service must use nginx image"),
);

const result = validator.validate(configData);
if (!result.valid) {
  result.errors.forEach((error) => {
    console.error(`${error.path}: ${error.message}`);
  });
}
```

## Environment Support

### Environment-specific Files

Create environment-specific configurations:

```bash
# Production
.jiji/deploy.production.yml

# Staging  
.jiji/deploy.staging.yml

# Development
.jiji/deploy.development.yml
```

Load environment-specific config:

```bash
jiji deploy -e production
jiji init -e staging
```

### Environment Variables

Configure environment variables in the config:

```yaml
env:
  variables:
    APP_ENV: production
    LOG_LEVEL: info
  secrets:
    - DATABASE_PASSWORD # Loaded from env var
    - API_SECRET_KEY
  files:
    SSL_CERT: /path/to/cert.pem # Loaded from file
```

Access in code:

```typescript
const env = config.environment;
const resolved = await env.resolveVariables();
console.log(resolved.DATABASE_PASSWORD); // From environment
```

## Configuration Loading

Load configuration using the Configuration class:

```typescript
import { Configuration } from "./src/lib/configuration.ts";

// Load default configuration
const config = await Configuration.load();
console.log(config.engine);

// Load environment-specific configuration
const config = await Configuration.load("production");

// Load from specific file path
const config = await Configuration.load(undefined, "./custom/config.yml");
```

## Best Practices

### 1. Environment-specific Configurations

```bash
# Use environment-specific files
.jiji/deploy.production.yml   # Production settings
.jiji/deploy.staging.yml      # Staging settings
.jiji/deploy.yml              # Default/development
```

### 2. Secret Management

```yaml
# Don't store secrets in config files
env:
  secrets:
    - DATABASE_PASSWORD
    - API_SECRET_KEY
    - JWT_SECRET
```

### 3. Service Organization

```yaml
# Group related services
services:
  # Frontend services
  web:
    image: nginx:latest
    hosts: ["web1.example.com", "web2.example.com"]

  # Backend services
  api:
    image: myapp/api:latest
    hosts: ["api1.example.com", "api2.example.com"]
    depends_on: ["database"]

  # Data services
  database:
    image: postgres:15
    hosts: ["db1.example.com"]
```

### 4. Health Checks

```yaml
services:
  api:
    image: myapp/api:latest
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:3000/health"]
      interval: "30s"
      timeout: "10s"
      retries: 3
```

### 5. Proper Restart Policies

```yaml
services:
  # Critical services
  api:
    restart: always

  # One-time jobs
  migration:
    restart: "no"

  # Services that should restart unless manually stopped
  worker:
    restart: unless-stopped
```

## Error Handling

The configuration system provides detailed error messages:

```typescript
try {
  const config = await Configuration.load();
} catch (error) {
  if (error instanceof ConfigurationError) {
    console.error("Configuration Error:", error.message);
    console.error("Context:", error.context);
  }
}
```

Common errors:

- **Missing required fields**: Clear indication of what's missing
- **Invalid values**: Specific validation failure details
- **Circular dependencies**: Shows the dependency chain
- **File not found**: Suggests possible file locations
- **Permission errors**: Clear indication of access issues

## Testing Configuration

Validate configuration files:

```bash
# Validate current configuration
jiji config validate

# Validate specific file
jiji config validate /path/to/config.yml

# Validate for specific environment
jiji config validate -e production
```

In code:

```typescript
// Validate without loading fully
const result = await Configuration.validateFile("config.yml");

if (!result.valid) {
  result.errors.forEach((error) => {
    console.error(`${error.path}: ${error.message}`);
  });
}
```

## Extending the Configuration System

### Adding New Service Options

1. Update `ServiceConfiguration` class:

```typescript
// In src/lib/configuration/service.ts
get myNewOption(): string | undefined {
  if (!this._myNewOption && this.has("my_new_option")) {
    this._myNewOption = this.validateString(
      this.get("my_new_option"), 
      "my_new_option", 
      this.name
    );
  }
  return this._myNewOption;
}
```

2. Add validation rules:

```typescript
// In validation preset
validator.addRule("services.*.my_new_option", ValidationRules.string());
```

### Adding New Configuration Sections

1. Create new configuration class:

```typescript
// src/lib/configuration/monitoring.ts
export class MonitoringConfiguration extends BaseConfiguration {
  get enabled(): boolean {
    return this.get("enabled", false);
  }

  get endpoint(): string {
    return this.getRequired("endpoint");
  }
}
```

2. Add to main Configuration class:

```typescript
get monitoring(): MonitoringConfiguration {
  if (!this._monitoring) {
    const config = this.has("monitoring") ? this.get("monitoring") : {};
    this._monitoring = new MonitoringConfiguration(config);
  }
  return this._monitoring;
}
```

## Performance Considerations

- **Lazy Loading**: Configuration sections are only loaded when accessed
- **Caching**: Parsed values are cached to avoid repeated processing
- **Validation**: Only runs when explicitly requested or during load
- **File Watching**: Not implemented - restart required for config changes

## Troubleshooting

### Common Issues

1. **Configuration not found**
   - Check file exists in `.jiji/` directory
   - Verify file permissions
   - Use `jiji init` to create initial config

2. **Validation errors**
   - Check syntax with YAML validator
   - Verify all required fields are present
   - Check data types match expectations

3. **Service dependencies**
   - Ensure referenced services exist
   - Check for circular dependencies
   - Verify service names match exactly

4. **SSH connection issues**
   - Verify SSH user has appropriate permissions
   - Check SSH key path and permissions
   - Test SSH connection manually

For more help, see the troubleshooting guide or open an issue on GitHub.
