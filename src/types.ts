export interface JijiConfig {
  engine: "podman" | "docker";
  ssh?: SSHConfig;
  services: Record<string, ServiceConfig>;
}

export interface SSHConfig {
  user: string;
  port?: number;
}

export interface ServiceConfig {
  image?: string;
  build?: string | BuildConfig;
  hosts?: string[];
  ports?: string[];
  volumes?: string[];
  environment?: Record<string, string> | string[];
  depends_on?: string[];
  command?: string | string[];
  working_dir?: string;
  restart?: "no" | "always" | "on-failure" | "unless-stopped";
  labels?: Record<string, string>;
  networks?: string[];
}

export interface BuildConfig {
  context: string;
  dockerfile?: string;
  args?: Record<string, string>;
  target?: string;
}

export interface ConfigLoadResult {
  config: JijiConfig;
  configPath: string;
}

export interface AuditEntry {
  timestamp: string;
  action: string;
  details?: Record<string, unknown>;
  user?: string;
  host?: string;
  status: "started" | "success" | "failed" | "warning";
  message?: string;
}

export interface GlobalOptions {
  environment?: string;
  verbose?: boolean;
  version?: string;
  configFile?: string;
  hosts?: string;
  services?: string;
}
