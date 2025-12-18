import { log } from "./logger.ts";
import type { SSHManager } from "./ssh.ts";
import { connect, type Socket } from "node:net";

/**
 * SSH Port Forwarder for tunneling local registry to remote hosts
 * Uses SSH reverse port forwarding to make local registry accessible on remote hosts
 * This enables remote hosts to pull images from a local registry via SSH tunnel
 */
export class PortForwarder {
  private server?: Deno.Listener;
  private forwarding = false;
  private activeConnections: Socket[] = [];

  constructor(
    private ssh: SSHManager,
    private localPort: number,
    private remotePort: number = localPort,
    private remoteHost: string = "localhost",
  ) {}

  /**
   * Start SSH port forwarding
   * Creates reverse tunnel: remote can access local registry via SSH
   * When remote host connects to localhost:remotePort, it gets forwarded to localhost:localPort on local machine
   */
  async startForwarding(): Promise<void> {
    if (this.forwarding) {
      log.debug("Port forwarding already active", "port-forward");
      return;
    }

    if (!this.ssh.isConnected()) {
      throw new Error(
        "SSH connection not established. Cannot start port forwarding.",
      );
    }

    log.info(
      `Starting reverse port forward: remote ${this.remoteHost}:${this.remotePort} -> local localhost:${this.localPort}`,
      "port-forward",
    );

    try {
      // Get the underlying ssh2 client from SSHManager
      // We'll use forwardIn to create a reverse tunnel
      const client = (this.ssh as any).ssh2Client ||
                     (this.ssh as any).ssh?.connection;

      if (!client) {
        throw new Error(
          "SSH client not available. This may be because the connection was made with NodeSSH only.",
        );
      }

      // Set up reverse port forwarding
      // This allows the remote host to connect to localhost:remotePort
      // and have it forwarded to our local registry
      await new Promise<void>((resolve, reject) => {
        client.forwardIn(
          this.remoteHost,
          this.remotePort,
          (err: Error | undefined) => {
            if (err) {
              reject(
                new Error(`Failed to set up port forwarding: ${err.message}`),
              );
              return;
            }
            resolve();
          },
        );
      });

      // Handle incoming connections from the remote side
      client.on(
        "tcp connection",
        (info: any, accept: () => any, reject: () => void) => {
          if (info.destPort === this.remotePort) {
            this.handleTcpConnection(accept, reject);
          }
        },
      );

      this.forwarding = true;
      log.success(
        `Reverse port forwarding established: remote can access local registry via localhost:${this.remotePort}`,
        "port-forward",
      );
    } catch (error) {
      const errorMsg = error instanceof Error
        ? error.message
        : String(error);
      log.error(`Port forwarding setup failed: ${errorMsg}`, "port-forward");
      throw error;
    }
  }

  /**
   * Handle TCP connection from remote host
   * Forward it to local registry
   */
  private handleTcpConnection(accept: () => any, reject: () => void): void {
    log.debug(
      `Incoming connection on forwarded port ${this.remotePort}`,
      "port-forward",
    );

    const remoteStream = accept();
    const localSocket = connect(this.localPort, "localhost");

    this.activeConnections.push(localSocket);

    // Pipe data bidirectionally
    remoteStream.pipe(localSocket);
    localSocket.pipe(remoteStream);

    // Handle errors and cleanup
    const cleanup = () => {
      const index = this.activeConnections.indexOf(localSocket);
      if (index > -1) {
        this.activeConnections.splice(index, 1);
      }
    };

    remoteStream.on("close", () => {
      localSocket.end();
      cleanup();
    });

    localSocket.on("close", () => {
      remoteStream.end();
      cleanup();
    });

    remoteStream.on("error", (err: Error) => {
      log.debug(
        `Remote stream error: ${err.message}`,
        "port-forward",
      );
      localSocket.destroy();
      cleanup();
    });

    localSocket.on("error", (err: Error) => {
      log.debug(
        `Local socket error: ${err.message}`,
        "port-forward",
      );
      remoteStream.destroy();
      cleanup();
    });
  }

  /**
   * Stop SSH port forwarding
   */
  async stopForwarding(): Promise<void> {
    if (!this.forwarding) {
      return;
    }

    log.info("Stopping port forwarding", "port-forward");

    try {
      // Close all active connections
      for (const conn of this.activeConnections) {
        try {
          conn.destroy();
        } catch {
          // Ignore errors on cleanup
        }
      }
      this.activeConnections = [];

      // Cancel the reverse tunnel
      const client = (this.ssh as any).ssh2Client ||
                     (this.ssh as any).ssh?.connection;

      if (client) {
        await new Promise<void>((resolve) => {
          client.unforwardIn(
            this.remoteHost,
            this.remotePort,
            (err: Error | undefined) => {
              if (err) {
                log.debug(
                  `Error canceling port forward: ${err.message}`,
                  "port-forward",
                );
              }
              resolve();
            },
          );
        });
      }

      this.forwarding = false;
      log.success("Port forwarding stopped", "port-forward");
    } catch (error) {
      log.warn(
        `Error stopping port forward: ${
          error instanceof Error ? error.message : String(error)
        }`,
        "port-forward",
      );
    }
  }

  /**
   * Check if port forwarding is active
   */
  isForwarding(): boolean {
    return this.forwarding;
  }

  /**
   * Cleanup resources
   */
  async cleanup(): Promise<void> {
    await this.stopForwarding();
  }
}

/**
 * Port forward manager for handling multiple forwards
 */
export class PortForwardManager {
  private forwarders: Map<string, PortForwarder> = new Map();

  /**
   * Create or get a port forwarder for a host
   */
  getForwarder(
    key: string,
    ssh: SSHManager,
    localPort: number,
    remotePort?: number,
  ): PortForwarder {
    if (!this.forwarders.has(key)) {
      this.forwarders.set(
        key,
        new PortForwarder(ssh, localPort, remotePort),
      );
    }
    return this.forwarders.get(key)!;
  }

  /**
   * Start all port forwards
   */
  async startAll(): Promise<void> {
    const promises = Array.from(this.forwarders.values()).map((f) =>
      f.startForwarding()
    );
    await Promise.all(promises);
  }

  /**
   * Stop all port forwards
   */
  async stopAll(): Promise<void> {
    const promises = Array.from(this.forwarders.values()).map((f) =>
      f.stopForwarding()
    );
    await Promise.all(promises);
  }

  /**
   * Cleanup all forwarders
   */
  async cleanup(): Promise<void> {
    await this.stopAll();
    this.forwarders.clear();
  }
}
