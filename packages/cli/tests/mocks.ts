/**
 * Mock SSH manager for testing commands that execute over SSH
 */
type MockResponse = {
  success: boolean;
  stdout: string;
  stderr: string;
  code: number | null;
};

export class MockSSHManager {
  private commands: string[] = [];
  private shouldSucceed = true;
  private host: string;
  private mockResponses: Map<string, MockResponse> = new Map();
  // Sequenced responses: each matching command consumes one entry; the last
  // entry is retained as a fallback once the sequence is exhausted.
  private mockSequences: Map<string, MockResponse[]> = new Map();

  constructor(host = "test-host", shouldSucceed = true) {
    this.host = host;
    this.shouldSucceed = shouldSucceed;
  }

  addMockResponse(
    commandPattern: string,
    response: MockResponse,
  ) {
    this.mockResponses.set(commandPattern, response);
  }

  /**
   * Register an ordered sequence of responses for commands matching this
   * pattern. The Nth match returns the Nth entry; once exhausted, the last
   * entry is returned for every subsequent match. Use this when a command
   * needs to behave differently across calls (e.g. an interface check that
   * fails before a recovery action and succeeds after).
   */
  addMockResponseSequence(
    commandPattern: string,
    responses: MockResponse[],
  ) {
    if (responses.length === 0) {
      throw new Error("addMockResponseSequence requires at least one response");
    }
    this.mockSequences.set(commandPattern, [...responses]);
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

    // Sequenced responses take precedence over single-shot responses so
    // tests that want to switch behaviour mid-run aren't shadowed by a
    // broader pattern set up earlier.
    for (const [pattern, queue] of this.mockSequences) {
      if (command.includes(pattern)) {
        const response = queue.length > 1 ? queue.shift()! : queue[0];
        return Promise.resolve(response);
      }
    }

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
      // hasCertsMount() checks for "jiji-certs" in HostConfig.Binds output
      if (command.includes("HostConfig.Binds")) {
        return Promise.resolve({
          success: true,
          stdout: '["$HOME/.jiji/certs:/jiji-certs:ro"]',
          stderr: "",
          code: 0,
        });
      }
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
