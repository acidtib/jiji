export interface JijiConfig {
  engine: "podman" | "docker";
  services: Record<string, ServiceConfig>;
}

export interface ServiceConfig {
  image?: string;
  build?: string | BuildConfig;
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
