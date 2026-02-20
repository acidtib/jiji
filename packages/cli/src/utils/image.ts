/**
 * Utilities for container image name handling
 */

/**
 * Normalizes a container image name to include a registry if not already present.
 *
 * This is particularly important for Podman, which requires fully qualified image names.
 * Docker also benefits from explicit registry names for clarity.
 *
 * Examples:
 * - "nginx" -> "docker.io/library/nginx"
 * - "nginx:latest" -> "docker.io/library/nginx:latest"
 * - "dxflrs/garage:v2.1.0" -> "docker.io/dxflrs/garage:v2.1.0"
 * - "ghcr.io/owner/repo:v1" -> "ghcr.io/owner/repo:v1" (unchanged)
 * - "localhost:5000/myimage" -> "localhost:5000/myimage" (unchanged)
 *
 * @param imageName - The image name to normalize
 * @returns The normalized image name with registry
 */
export function normalizeImageName(imageName: string): string {
  // If the image name already has a registry, return as-is
  // A registry is present if the first component (before any slash) contains a dot or colon
  const firstSlashIndex = imageName.indexOf("/");

  if (firstSlashIndex === -1) {
    // No slash - this is an official library image (e.g., "nginx", "postgres")
    return `docker.io/library/${imageName}`;
  }

  const firstComponent = imageName.substring(0, firstSlashIndex);

  // Check if first component is a registry
  // A registry is either:
  // - "localhost" (special case)
  // - Contains a dot (e.g., docker.io, ghcr.io, registry.example.com)
  // - Contains a colon (e.g., localhost:5000, 10.0.0.1:5000)
  if (
    firstComponent === "localhost" ||
    firstComponent.includes(".") ||
    firstComponent.includes(":")
  ) {
    // Already has a registry
    return imageName;
  }

  // No registry - this is a user/org image (e.g., "dxflrs/garage")
  // Prepend docker.io
  return `docker.io/${imageName}`;
}
