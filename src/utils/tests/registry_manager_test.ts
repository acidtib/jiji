import { assertEquals } from "@std/assert";
import { RegistryManager } from "../registry_manager.ts";

// Note: These tests interact with Docker/Podman and should be run in an environment
// where the container engine is available. They are integration tests.

// Detect available container engine
async function detectEngine(): Promise<"docker" | "podman" | null> {
  // Try Docker first
  try {
    const dockerCmd = new Deno.Command("docker", {
      args: ["--version"],
      stdout: "null",
      stderr: "null",
    });
    const result = await dockerCmd.output();
    if (result.code === 0) return "docker";
  } catch {
    // Docker not available
  }

  // Try Podman
  try {
    const podmanCmd = new Deno.Command("podman", {
      args: ["--version"],
      stdout: "null",
      stderr: "null",
    });
    const result = await podmanCmd.output();
    if (result.code === 0) return "podman";
  } catch {
    // Podman not available
  }

  return null;
}

const engine = await detectEngine();

if (engine) {
  Deno.test({
    name: "RegistryManager - check if registry is running",
    async fn() {
      const manager = new RegistryManager(engine, 6767);

      const isRunning = await manager.isRunning();
      assertEquals(typeof isRunning, "boolean");
    },
  });

  Deno.test({
    name: "RegistryManager - get registry status",
    async fn() {
      const manager = new RegistryManager(engine, 6767);

      const status = await manager.getStatus();

      assertEquals(typeof status.running, "boolean");
      assertEquals(typeof status.port, "number");
      assertEquals(status.port, 6767);

      if (status.running) {
        assertEquals(typeof status.containerId, "string");
      }
    },
  });

  Deno.test({
    name: "RegistryManager - start and stop registry",
    async fn() {
      const testPort = 5001; // Use a different port to avoid conflicts
      const manager = new RegistryManager(engine, testPort);

      try {
        // Ensure registry is not running
        if (await manager.isRunning()) {
          await manager.stop();
        }

        // Start registry
        await manager.start();

        // Check it's running
        const isRunning = await manager.isRunning();
        assertEquals(isRunning, true);

        const status = await manager.getStatus();
        assertEquals(status.running, true);
        assertEquals(status.port, testPort);

        // Starting again should be idempotent
        await manager.start();
        assertEquals(await manager.isRunning(), true);

        // Stop registry
        await manager.stop();

        // Check it's stopped
        const isRunningStopped = await manager.isRunning();
        assertEquals(isRunningStopped, false);

        // Stopping again should be idempotent
        await manager.stop();
      } finally {
        // Cleanup - ensure registry is stopped and removed
        try {
          if (await manager.isRunning()) {
            await manager.stop();
          }
          await manager.remove();
        } catch {
          // Ignore cleanup errors
        }
      }
    },
    sanitizeResources: false,
    sanitizeOps: false,
  });

  Deno.test({
    name: "RegistryManager - remove registry container",
    async fn() {
      const testPort = 5002;
      const manager = new RegistryManager(engine, testPort);

      try {
        // Start registry
        await manager.start();
        assertEquals(await manager.isRunning(), true);

        // Remove registry (should stop and remove)
        await manager.remove();

        // Check it's not running
        assertEquals(await manager.isRunning(), false);

        // Removing again should be idempotent
        await manager.remove();
      } finally {
        // Cleanup
        try {
          await manager.remove();
        } catch {
          // Ignore cleanup errors
        }
      }
    },
    sanitizeResources: false,
    sanitizeOps: false,
  });

  Deno.test({
    name: "RegistryManager - remove registry with volume cleanup",
    async fn() {
      const testPort = 5005;
      const manager = new RegistryManager(engine, testPort);

      try {
        // Start registry (creates volume)
        await manager.start();
        assertEquals(await manager.isRunning(), true);

        // Remove registry (should stop container and remove volume)
        await manager.remove();

        // Check it's not running
        assertEquals(await manager.isRunning(), false);

        // Verify volume is cleaned up by checking if we can create registry again
        // without conflicts (this indirectly tests volume cleanup)
        await manager.start();
        assertEquals(await manager.isRunning(), true);
        await manager.remove();
      } finally {
        // Cleanup
        try {
          await manager.remove();
        } catch {
          // Ignore cleanup errors
        }
      }
    },
    sanitizeResources: false,
    sanitizeOps: false,
  });

  Deno.test({
    name: "RegistryManager - restart existing stopped container",
    async fn() {
      const testPort = 5003;
      const manager = new RegistryManager(engine, testPort);

      try {
        // Start and stop to create a stopped container
        await manager.start();
        await manager.stop();

        assertEquals(await manager.isRunning(), false);

        // Start again - should reuse existing container
        await manager.start();

        assertEquals(await manager.isRunning(), true);
      } finally {
        // Cleanup
        try {
          if (await manager.isRunning()) {
            await manager.stop();
          }
          await manager.remove();
        } catch {
          // Ignore cleanup errors
        }
      }
    },
    sanitizeResources: false,
    sanitizeOps: false,
  });

  Deno.test({
    name: "RegistryManager - uses custom port",
    async fn() {
      const testPort = 5004;
      const manager = new RegistryManager(engine, testPort);

      try {
        await manager.start();

        const status = await manager.getStatus();
        assertEquals(status.port, testPort);
        assertEquals(status.running, true);
      } finally {
        // Cleanup
        try {
          if (await manager.isRunning()) {
            await manager.stop();
          }
          await manager.remove();
        } catch {
          // Ignore cleanup errors
        }
      }
    },
    sanitizeResources: false,
    sanitizeOps: false,
  });

  Deno.test({
    name: "RegistryManager - different engines use same container name",
    async fn() {
      // This test verifies that the container name is consistent
      const manager = new RegistryManager(engine, 6767);

      // The container name should always be 'jiji-registry'
      // We can't directly access private static fields, but we can verify
      // by checking if operations work consistently

      try {
        await manager.start();
        const running1 = await manager.isRunning();

        // Create another manager instance with same engine
        const manager2 = new RegistryManager(engine, 6767);
        const running2 = await manager2.isRunning();

        // Both should see the same container
        assertEquals(running1, running2);
        assertEquals(running1, true);
      } finally {
        // Cleanup
        try {
          if (await manager.isRunning()) {
            await manager.stop();
          }
          await manager.remove();
        } catch {
          // Ignore cleanup errors
        }
      }
    },
    sanitizeResources: false,
    sanitizeOps: false,
  });
} else {
  Deno.test("RegistryManager - skipped (no container engine available)", () => {
    console.log(
      "Skipping RegistryManager tests - Docker/Podman not available",
    );
  });
}
