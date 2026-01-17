import type { SSHManager } from "./ssh.ts";
import type {
  DirectoryMountConfig,
  FileMountConfig,
} from "../lib/configuration/service.ts";
import { join } from "@std/path";

/**
 * Parsed mount configuration
 */
export interface ParsedMount {
  local: string;
  remote: string;
  mode?: string;
  owner?: string;
  options?: string;
}

/**
 * Parse a mount configuration (file or directory) into a standardized format
 */
export function parseMountConfig(
  mount: FileMountConfig | DirectoryMountConfig,
): ParsedMount {
  if (typeof mount === "string") {
    // Parse string format: "local:remote" or "local:remote:options"
    const parts = mount.split(":");
    return {
      local: parts[0],
      remote: parts[1],
      options: parts[2],
    };
  } else {
    // Already in hash format
    return {
      local: mount.local,
      remote: mount.remote,
      mode: mount.mode,
      owner: mount.owner,
      options: mount.options,
    };
  }
}

/**
 * Upload files to remote host and prepare them for mounting
 */
export async function prepareMountFiles(
  ssh: SSHManager,
  files: FileMountConfig[],
  project: string,
  serviceName: string,
): Promise<void> {
  if (files.length === 0) return;

  const filesDir = `.jiji/${project}/files/${serviceName}`;

  // Create files directory on remote host
  await ssh.executeCommand(`mkdir -p ${filesDir}`);

  for (const fileMount of files) {
    const parsed = parseMountConfig(fileMount);
    const remoteFilePath = join(filesDir, parsed.local);

    // Upload file to remote host
    await ssh.uploadFile(parsed.local, remoteFilePath);

    // Set permissions if specified
    if (parsed.mode) {
      await ssh.executeCommand(`chmod ${parsed.mode} ${remoteFilePath}`);
    }

    // Set ownership if specified (requires root)
    if (parsed.owner) {
      await ssh.executeCommand(`chown ${parsed.owner} ${remoteFilePath}`);
    }
  }
}

/**
 * Create directories on remote host and prepare them for mounting
 */
export async function prepareMountDirectories(
  ssh: SSHManager,
  directories: DirectoryMountConfig[],
  project: string,
  serviceName: string,
): Promise<void> {
  if (directories.length === 0) return;

  const directoriesBase = `.jiji/${project}/directories/${serviceName}`;

  // Create base directories folder on remote host
  await ssh.executeCommand(`mkdir -p ${directoriesBase}`);

  for (const dirMount of directories) {
    const parsed = parseMountConfig(dirMount);
    const remoteDirPath = `${directoriesBase}/${parsed.local}`;

    // Check if local directory exists
    try {
      const stat = await Deno.stat(parsed.local);

      if (stat.isDirectory) {
        // Upload directory contents to remote host
        await ssh.uploadDirectory(parsed.local, remoteDirPath);
      } else {
        throw new Error(`${parsed.local} is not a directory`);
      }
    } catch (error) {
      // If local directory doesn't exist, create empty remote directory
      if (error instanceof Deno.errors.NotFound) {
        await ssh.executeCommand(`mkdir -p ${remoteDirPath}`);
      } else {
        throw error;
      }
    }

    // Set permissions if specified
    if (parsed.mode) {
      await ssh.executeCommand(`chmod -R ${parsed.mode} ${remoteDirPath}`);
    }

    // Set ownership if specified (requires root)
    if (parsed.owner) {
      await ssh.executeCommand(`chown -R ${parsed.owner} ${remoteDirPath}`);
    }
  }
}

/**
 * Build container mount arguments for a specific mount type
 */
function buildMountArgs(
  mounts: FileMountConfig[] | DirectoryMountConfig[],
  project: string,
  type: "files" | "directories",
  serviceName: string,
): string[] {
  return mounts.map((mount) => {
    const parsed = parseMountConfig(mount);
    // Include service name in path to avoid collisions between services
    const basePath = `.jiji/${project}/${type}/${serviceName}`;
    const remotePath = `${basePath}/${parsed.local}`;
    let mountArg = `${remotePath}:${parsed.remote}`;

    // Add options if specified
    if (parsed.options) {
      mountArg += `:${parsed.options}`;
    }

    return `-v ${mountArg}`;
  });
}

/**
 * Build container mount arguments for files
 */
export function buildFileMountArgs(
  files: FileMountConfig[],
  project: string,
  serviceName: string,
): string[] {
  return buildMountArgs(files, project, "files", serviceName);
}

/**
 * Build container mount arguments for directories
 */
export function buildDirectoryMountArgs(
  directories: DirectoryMountConfig[],
  project: string,
  serviceName: string,
): string[] {
  return buildMountArgs(directories, project, "directories", serviceName);
}

/**
 * Build volume mount arguments with service name prefix for named volumes.
 * Named volumes are prefixed with service name to prevent conflicts between services.
 * Host path mounts (starting with /) are passed through unchanged.
 */
export function buildVolumeArgs(
  volumes: string[],
  serviceName: string,
): string[] {
  return volumes.map((volume) => {
    const colonIndex = volume.indexOf(":");

    // Invalid format (no colon) - pass through as-is
    if (colonIndex === -1) {
      return `-v ${volume}`;
    }

    const source = volume.substring(0, colonIndex);
    const targetAndOptions = volume.substring(colonIndex);

    // Host path mounts start with / - pass through unchanged
    if (source.startsWith("/")) {
      return `-v ${volume}`;
    }

    // Named volume - prefix with service name to prevent conflicts
    return `-v ${serviceName}-${source}${targetAndOptions}`;
  });
}

/**
 * Build all mount arguments for a service
 */
export function buildAllMountArgs(
  files: FileMountConfig[],
  directories: DirectoryMountConfig[],
  volumes: string[],
  project: string,
  serviceName: string,
): string {
  const fileArgs = buildMountArgs(files, project, "files", serviceName);
  const directoryArgs = buildMountArgs(
    directories,
    project,
    "directories",
    serviceName,
  );
  const volumeArgs = buildVolumeArgs(volumes, serviceName);

  return [...fileArgs, ...directoryArgs, ...volumeArgs].join(" ");
}
