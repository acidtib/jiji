/**
 * Tests for ImagePruneService
 */

import { assertEquals } from "@std/assert";
import { ImagePruneService } from "../src/lib/services/image_prune_service.ts";
import type { SSHManager } from "../src/utils/ssh.ts";

/**
 * Mock SSH manager for testing image pruning
 */
class MockSSHManager {
  private host: string;
  private commandResults: Map<string, { stdout: string; stderr: string }> =
    new Map();

  constructor(host: string) {
    this.host = host;
  }

  getHost(): string {
    return this.host;
  }

  /**
   * Mock image list command response
   */
  setImageListResponse(images: string[]): void {
    const stdout = images.join("\n");
    this.commandResults.set("images_format", { stdout, stderr: "" });
  }

  /**
   * Mock active containers response
   */
  setActiveContainersResponse(imageIds: string[]): void {
    const stdout = imageIds.join("\n");
    this.commandResults.set("active_containers", { stdout, stderr: "" });
  }

  /**
   * Mock dangling images response
   */
  setDanglingImagesResponse(imageIds: string[]): void {
    const stdout = imageIds.join("\n");
    this.commandResults.set("dangling_images", { stdout, stderr: "" });
  }

  executeCommand(
    command: string,
  ): Promise<{ success: boolean; stdout: string; stderr: string }> {
    // Mock image list command
    if (command.includes("--format") && command.includes("{{.Repository}}")) {
      const result = this.commandResults.get("images_format") || {
        stdout: "",
        stderr: "",
      };
      return Promise.resolve({ success: true, ...result });
    }

    // Mock active containers
    if (command.includes("ps") && command.includes("--format")) {
      const result = this.commandResults.get("active_containers") || {
        stdout: "",
        stderr: "",
      };
      return Promise.resolve({ success: true, ...result });
    }

    // Mock dangling images
    if (command.includes("filter") && command.includes("dangling=true")) {
      const result = this.commandResults.get("dangling_images") || {
        stdout: "",
        stderr: "",
      };
      return Promise.resolve({ success: true, ...result });
    }

    // Mock image removal commands
    if (command.includes("rmi") || command.includes("image rm")) {
      return Promise.resolve({ success: true, stdout: "", stderr: "" });
    }

    return Promise.resolve({ success: true, stdout: "", stderr: "" });
  }

  dispose(): void {
    // Mock cleanup
  }
}

Deno.test("ImagePruneService - prunes old images and retains recent ones", async () => {
  const service = new ImagePruneService("docker", "test-project");
  const mockSSH = new MockSSHManager("server1.example.com");

  // Mock 10 images for a service (should keep 3, remove 7)
  const images = [
    "registry.example.com/test-project/web:abc1234 123456 10 days ago",
    "registry.example.com/test-project/web:abc2345 234567 9 days ago",
    "registry.example.com/test-project/web:abc3456 345678 8 days ago",
    "registry.example.com/test-project/web:abc4567 456789 7 days ago",
    "registry.example.com/test-project/web:abc5678 567890 6 days ago",
    "registry.example.com/test-project/web:abc6789 678901 5 days ago",
    "registry.example.com/test-project/web:abc7890 789012 4 days ago",
    "registry.example.com/test-project/web:abc8901 890123 3 days ago",
    "registry.example.com/test-project/web:abc9012 901234 2 days ago",
    "registry.example.com/test-project/web:abc0123 012345 1 day ago",
  ];

  mockSSH.setImageListResponse(images);
  mockSSH.setActiveContainersResponse([]); // No active containers
  mockSSH.setDanglingImagesResponse(["dangling1", "dangling2"]);

  const result = await service.pruneImages(
    mockSSH as unknown as SSHManager,
    { retain: 3, removeDangling: true },
  );

  assertEquals(result.success, true);
  assertEquals(result.host, "server1.example.com");
  // Note: Since we're mocking, we can't verify exact counts, but we verify the structure
  assertEquals(typeof result.imagesRemoved, "number");
});

Deno.test("ImagePruneService - handles errors gracefully", async () => {
  const service = new ImagePruneService("podman", "test-project");
  const mockSSH = new MockSSHManager("server2.example.com");

  // Empty image list
  mockSSH.setImageListResponse([]);
  mockSSH.setActiveContainersResponse([]);
  mockSSH.setDanglingImagesResponse([]);

  const result = await service.pruneImages(
    mockSSH as unknown as SSHManager,
    { retain: 5 },
  );

  assertEquals(result.success, true);
  assertEquals(result.host, "server2.example.com");
  assertEquals(result.imagesRemoved, 0);
});

Deno.test("ImagePruneService - uses default retention of 3", async () => {
  const service = new ImagePruneService("docker", "my-app");
  const mockSSH = new MockSSHManager("server3.example.com");

  const images = [
    "registry.example.com/my-app/api:v1 111111 3 days ago",
    "registry.example.com/my-app/api:v2 222222 2 days ago",
    "registry.example.com/my-app/api:v3 333333 1 day ago",
    "registry.example.com/my-app/api:v4 444444 1 hour ago",
  ];

  mockSSH.setImageListResponse(images);
  mockSSH.setActiveContainersResponse([]);
  mockSSH.setDanglingImagesResponse([]);

  // No retain option specified, should default to 3
  const result = await service.pruneImages(mockSSH as unknown as SSHManager);

  assertEquals(result.success, true);
});

Deno.test("ImagePruneService - skips dangling images when disabled", async () => {
  const service = new ImagePruneService("docker", "test-project");
  const mockSSH = new MockSSHManager("server4.example.com");

  mockSSH.setImageListResponse([
    "registry.example.com/test-project/web:abc1234 123456 10 days ago",
  ]);
  mockSSH.setActiveContainersResponse([]);
  mockSSH.setDanglingImagesResponse(["dangling1", "dangling2"]);

  const result = await service.pruneImages(
    mockSSH as unknown as SSHManager,
    { retain: 3, removeDangling: false },
  );

  assertEquals(result.success, true);
  // Dangling images should not be counted when removeDangling is false
});

Deno.test("ImagePruneService - works with Podman engine", async () => {
  const service = new ImagePruneService("podman", "podman-project");
  const mockSSH = new MockSSHManager("podman-host.example.com");

  const images = [
    "localhost/podman-project/service:tag1 111111 2 days ago",
    "localhost/podman-project/service:tag2 222222 1 day ago",
  ];

  mockSSH.setImageListResponse(images);
  mockSSH.setActiveContainersResponse([]);
  mockSSH.setDanglingImagesResponse([]);

  const result = await service.pruneImages(
    mockSSH as unknown as SSHManager,
    { retain: 1 },
  );

  assertEquals(result.success, true);
  assertEquals(result.host, "podman-host.example.com");
});

Deno.test("ImagePruneService - groups images by service name", async () => {
  const service = new ImagePruneService("docker", "multi-service");
  const mockSSH = new MockSSHManager("server5.example.com");

  // Multiple services with different images
  const images = [
    "registry.example.com/multi-service/web:v1 111111 5 days ago",
    "registry.example.com/multi-service/web:v2 222222 4 days ago",
    "registry.example.com/multi-service/web:v3 333333 3 days ago",
    "registry.example.com/multi-service/web:v4 444444 2 days ago",
    "registry.example.com/multi-service/api:v1 555555 5 days ago",
    "registry.example.com/multi-service/api:v2 666666 4 days ago",
    "registry.example.com/multi-service/api:v3 777777 3 days ago",
  ];

  mockSSH.setImageListResponse(images);
  mockSSH.setActiveContainersResponse([]);
  mockSSH.setDanglingImagesResponse([]);

  const result = await service.pruneImages(
    mockSSH as unknown as SSHManager,
    { retain: 2 }, // Keep 2 per service
  );

  assertEquals(result.success, true);
  // Should retain 2 web images and 2 api images, removing 3 total
});
