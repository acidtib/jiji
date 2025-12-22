/**
 * Mock SSH manager for testing commands that execute over SSH
 */
export class MockSSHManager {
  private commands: string[] = [];
  private shouldSucceed = true;
  private host: string;
  private mockResponses: Map<
    string,
    { success: boolean; stdout: string; stderr: string; code: number | null }
  > = new Map();

  constructor(host = "test-host", shouldSucceed = true) {
    this.host = host;
    this.shouldSucceed = shouldSucceed;
  }

  addMockResponse(
    commandPattern: string,
    response: {
      success: boolean;
      stdout: string;
      stderr: string;
      code: number | null;
    },
  ) {
    this.mockResponses.set(commandPattern, response);
  }

  // Additional methods to match SSHManager interface
  getConfig() {
    return { host: this.host };
  }

  isConnected(): boolean {
    return true;
  }

  async connect(): Promise<void> {
    // Mock connect
  }

  async disconnect(): Promise<void> {
    // Mock disconnect
  }

  executeCommand(
    command: string,
  ): Promise<
    { success: boolean; stdout: string; stderr: string; code: number | null }
  > {
    this.commands.push(command);

    // Check for custom mock responses first
    for (const [pattern, response] of this.mockResponses) {
      if (command.includes(pattern)) {
        return Promise.resolve(response);
      }
    }

    // Handle proxy-specific commands
    if (command.includes("docker network create")) {
      return Promise.resolve({
        success: true,
        stdout: "",
        stderr: "",
        code: 0,
      });
    }

    if (command.includes("docker inspect kamal-proxy")) {
      return Promise.resolve({
        success: true,
        stdout: "running",
        stderr: "",
        code: 0,
      });
    }

    if (command.includes("docker run") && command.includes("kamal-proxy")) {
      return Promise.resolve({
        success: true,
        stdout: "",
        stderr: "",
        code: 0,
      });
    }

    if (command.includes("kamal-proxy version")) {
      return Promise.resolve({
        success: true,
        stdout: "v0.5.0",
        stderr: "",
        code: 0,
      });
    }

    if (command.includes("kamal-proxy deploy")) {
      return Promise.resolve({
        success: this.shouldSucceed,
        stdout: this.shouldSucceed ? "" : "",
        stderr: this.shouldSucceed ? "" : "Deploy failed",
        code: this.shouldSucceed ? 0 : 1,
      });
    }

    if (command.includes("kamal-proxy list")) {
      return Promise.resolve({
        success: true,
        stdout: "web deployed http://web:3000",
        stderr: "",
        code: 0,
      });
    }

    if (command.includes("docker logs")) {
      return Promise.resolve({
        success: true,
        stdout: "Container logs for debugging",
        stderr: "",
        code: 0,
      });
    }

    // Handle restart command (always succeed for infrastructure operations)
    if (command.includes("restart") && command.includes("kamal-proxy")) {
      return Promise.resolve({
        success: true,
        stdout: "",
        stderr: "",
        code: 0,
      });
    }

    // Handle DNS update script (always succeed for infrastructure operations)
    if (command.includes("update-hosts.sh")) {
      return Promise.resolve({
        success: true,
        stdout: "",
        stderr: "",
        code: 0,
      });
    }

    // For actual deploy commands, respect shouldSucceed flag
    return Promise.resolve({
      success: this.shouldSucceed,
      stdout: this.shouldSucceed ? "success" : "",
      stderr: this.shouldSucceed ? "" : "error",
      code: this.shouldSucceed ? 0 : 1,
    });
  }

  getLastCommand(): string {
    return this.commands[this.commands.length - 1] || "";
  }

  getAllCommands(): string[] {
    return [...this.commands];
  }

  clearCommands(): void {
    this.commands = [];
  }

  getHost(): string {
    return this.host;
  }

  async dispose(): Promise<void> {
    // Mock cleanup
  }
}
