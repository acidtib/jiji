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
): Promise<void> {
  if (files.length === 0) return;

  const filesDir = `.jiji/${project}/files`;

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
): Promise<void> {
  if (directories.length === 0) return;

  const directoriesBase = `.jiji/${project}/directories`;

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
 * Build container mount arguments for files
 */
export function buildFileMountArgs(
  files: FileMountConfig[],
  project: string,
): string[] {
  return files.map((fileMount) => {
    const parsed = parseMountConfig(fileMount);
    const remoteFilePath = `.jiji/${project}/files/${parsed.local}`;
    let mountArg = `${remoteFilePath}:${parsed.remote}`;

    // Add options if specified
    if (parsed.options) {
      mountArg += `:${parsed.options}`;
    }

    return `-v ${mountArg}`;
  });
}

/**
 * Build container mount arguments for directories
 */
export function buildDirectoryMountArgs(
  directories: DirectoryMountConfig[],
  project: string,
): string[] {
  return directories.map((dirMount) => {
    const parsed = parseMountConfig(dirMount);
    const remoteDirPath = `.jiji/${project}/directories/${parsed.local}`;
    let mountArg = `${remoteDirPath}:${parsed.remote}`;

    // Add options if specified
    if (parsed.options) {
      mountArg += `:${parsed.options}`;
    }

    return `-v ${mountArg}`;
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
): string {
  const fileArgs = buildFileMountArgs(files, project);
  const directoryArgs = buildDirectoryMountArgs(directories, project);
  const volumeArgs = volumes.map((v) => `-v ${v}`);

  return [...fileArgs, ...directoryArgs, ...volumeArgs].join(" ");
}
