/**
 * Application-wide constants
 */

/**
 * Network and WireGuard configuration
 */
export const WIREGUARD_PORT = 31820;
export const DEFAULT_CLUSTER_CIDR = "10.210.0.0/16";
export const DEFAULT_SERVICE_DOMAIN = "jiji";

/**
 * Container engine versions
 * These are the supported versions that jiji installs and tests against.
 * Bump these after verifying compatibility with newer versions.
 */
export const DOCKER_VERSION = "28.2.0";
export const DOCKER_MIN_VERSION = "28.2.0";
export const PODMAN_VERSION = "4.9.3";
export const PODMAN_MIN_VERSION = "4.9.3";

/**
 * Container and deployment configuration
 */
export const CONTAINER_START_MAX_ATTEMPTS = 10;
export const CONTAINER_START_RETRY_DELAY_MS = 1000;
export const CONTAINER_LOG_TAIL_LINES = 50;

/**
 * Logging configuration
 */
export const DEFAULT_MAX_PREFIX_LENGTH = 25;

/**
 * Registry configuration
 */
export const DEFAULT_LOCAL_REGISTRY_PORT = 31270;

/**
 * Network names
 */
export const JIJI_NETWORK_NAME = "jiji";
export const KAMAL_PROXY_NETWORK_NAME = "kamal-proxy";

/**
 * Proxy configuration
 */
export const KAMAL_PROXY_CONTAINER_NAME = "kamal-proxy";
export const KAMAL_PROXY_INTERNAL_HTTP_PORT = 8080;
export const KAMAL_PROXY_INTERNAL_HTTPS_PORT = 8443;
export const KAMAL_PROXY_CONFIG_VOLUME = "kamal-proxy-config";

/**
 * Corrosion (distributed database) configuration
 */
export const CORROSION_API_PORT = 31220;
export const CORROSION_GOSSIP_PORT = 31280;
export const CORROSION_SYNC_TIMEOUT_SECONDS = 300; // 5 minutes
export const CORROSION_SYNC_POLL_INTERVAL_MS = 2000; // 2 seconds
export const CORROSION_SYNC_LOG_INTERVAL_MS = 5000; // 5 seconds

/**
 * Audit trail configuration
 */
export const AUDIT_LOG_BASE_PATH = "/var/log/jiji";
