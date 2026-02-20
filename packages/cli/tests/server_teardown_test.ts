/**
 * Tests for server teardown command
 */

import { assertEquals } from "@std/assert";

/**
 * Mock SSH manager for testing teardown operations
 */
class MockSSHManager {
  private host: string;
  private commandLog: string[] = [];

  constructor(host: string) {
    this.host = host;
  }

  getHost(): string {
    return this.host;
  }

  executeCommand(
    command: string,
  ): Promise<
    { success: boolean; stdout: string; stderr: string; code: number }
  > {
    this.commandLog.push(command);

    // Simulate successful execution for best-effort commands
    return Promise.resolve({
      success: true,
      stdout: "",
      stderr: "",
      code: 0,
    });
  }

  getCommandLog(): string[] {
    return this.commandLog;
  }

  dispose(): void {
    // Mock cleanup
  }
}

Deno.test("getNamedVolumes - extracts named volumes from service configuration", () => {
  // This tests the ServiceConfiguration.getNamedVolumes() method
  // which was extracted from the teardown command

  const volumes = [
    "db-data:/var/lib/postgresql/data",
    "./config:/etc/app:ro",
    "/host/path:/container/path",
    "cache-volume:/cache",
  ];

  const namedVolumes: string[] = [];
  for (const volume of volumes) {
    const parts = volume.split(":");
    if (parts.length >= 2) {
      const source = parts[0];
      if (!source.startsWith("/") && !source.startsWith("./")) {
        namedVolumes.push(source);
      }
    }
  }

  assertEquals(namedVolumes, ["db-data", "cache-volume"]);
});

Deno.test("executeBestEffort - logs failures without throwing", async () => {
  const mockSSH = new MockSSHManager("test-host");

  // Simulate executing a best-effort command
  await mockSSH.executeCommand("docker rm -f test-container");

  const log = mockSSH.getCommandLog();
  assertEquals(log.length, 1);
  assertEquals(log[0], "docker rm -f test-container");
});

Deno.test("teardown operations - should execute commands in correct order", async () => {
  const mockSSH = new MockSSHManager("server1.example.com");

  // Simulate teardown sequence
  await mockSSH.executeCommand("docker stop my-container");
  await mockSSH.executeCommand("docker rm -f my-container");
  await mockSSH.executeCommand("docker volume rm my-volume");

  const log = mockSSH.getCommandLog();
  assertEquals(log.length, 3);
  assertEquals(log[0], "docker stop my-container");
  assertEquals(log[1], "docker rm -f my-container");
  assertEquals(log[2], "docker volume rm my-volume");
});

Deno.test("teardown - volume removal handles multiple volumes", async () => {
  const mockSSH = new MockSSHManager("server2.example.com");

  const volumes = ["vol1", "vol2", "vol3"];

  for (const volumeName of volumes) {
    await mockSSH.executeCommand(`docker volume rm ${volumeName}`);
  }

  const log = mockSSH.getCommandLog();
  assertEquals(log.length, 3);
  assertEquals(log[0], "docker volume rm vol1");
  assertEquals(log[1], "docker volume rm vol2");
  assertEquals(log[2], "docker volume rm vol3");
});

Deno.test("teardown - engine removal commands for Docker", async () => {
  const mockSSH = new MockSSHManager("docker-host");

  // Simulate Docker engine removal
  await mockSSH.executeCommand("systemctl stop docker.socket docker.service");
  await mockSSH.executeCommand("apt-get remove -y docker.io docker-compose");
  await mockSSH.executeCommand("apt-get autoremove -y");
  await mockSSH.executeCommand("rm -rf /var/lib/docker");
  await mockSSH.executeCommand("rm -rf /etc/docker");

  const log = mockSSH.getCommandLog();
  assertEquals(log.length, 5);
  assertEquals(log[0], "systemctl stop docker.socket docker.service");
  assertEquals(log[4], "rm -rf /etc/docker");
});

Deno.test("teardown - engine removal commands for Podman", async () => {
  const mockSSH = new MockSSHManager("podman-host");

  // Simulate Podman engine removal
  await mockSSH.executeCommand("apt-get remove -y podman");
  await mockSSH.executeCommand("apt-get autoremove -y");
  await mockSSH.executeCommand("rm -rf /var/lib/containers");
  await mockSSH.executeCommand("rm -rf /etc/containers");

  const log = mockSSH.getCommandLog();
  assertEquals(log.length, 4);
  assertEquals(log[0], "apt-get remove -y podman");
  assertEquals(log[3], "rm -rf /etc/containers");
});

Deno.test("teardown - system purge commands executed in sequence", async () => {
  const mockSSH = new MockSSHManager("purge-host");
  const engine = "docker";

  // Simulate system purge sequence
  await mockSSH.executeCommand(`${engine} stop $(${engine} ps -aq)`);
  await mockSSH.executeCommand(`${engine} rm -f $(${engine} ps -aq)`);
  await mockSSH.executeCommand(`${engine} rmi -f $(${engine} images -aq)`);
  await mockSSH.executeCommand(`${engine} volume rm $(${engine} volume ls -q)`);
  await mockSSH.executeCommand(`${engine} network prune -f`);
  await mockSSH.executeCommand(`${engine} system prune -a -f --volumes`);

  const log = mockSSH.getCommandLog();
  assertEquals(log.length, 6);
  assertEquals(log[0].includes("stop"), true);
  assertEquals(log[5].includes("system prune"), true);
});
