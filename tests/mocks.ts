/**
 * Mock SSH manager for testing commands that execute over SSH
 */
export class MockSSHManager {
  private commands: string[] = [];
  private shouldSucceed = true;

  constructor(shouldSucceed = true) {
    this.shouldSucceed = shouldSucceed;
  }

  async executeCommand(command: string) {
    this.commands.push(command);
    return {
      success: this.shouldSucceed,
      stdout: this.shouldSucceed ? "success" : "",
      stderr: this.shouldSucceed ? "" : "error",
    };
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
    return "test-host";
  }
}
