/**
 * Mock SSH manager for testing commands that execute over SSH
 */
export class MockSSHManager {
  private commands: string[] = [];
  private shouldSucceed = true;

  constructor(shouldSucceed = true) {
    this.shouldSucceed = shouldSucceed;
  }

  executeCommand(command: string) {
    this.commands.push(command);

    // Handle container inspect commands for proxy (always succeed for infrastructure checks)
    if (command.includes("inspect") && command.includes("kamal-proxy")) {
      return {
        success: true,
        stdout: "running",
        stderr: "",
      };
    }

    // Handle restart command (always succeed for infrastructure operations)
    if (command.includes("restart") && command.includes("kamal-proxy")) {
      return {
        success: true,
        stdout: "",
        stderr: "",
      };
    }

    // Handle DNS update script (always succeed for infrastructure operations)
    if (command.includes("update-hosts.sh")) {
      return {
        success: true,
        stdout: "",
        stderr: "",
      };
    }

    // For actual deploy commands, respect shouldSucceed flag
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
