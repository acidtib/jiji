import { Client, type ClientChannel } from "ssh2";

/**
 * SSH Proxy helper for creating tunneled connections through bastion/jump hosts
 */
export class SSHProxy {
  /**
   * Create a proxy socket that tunnels through a bastion host
   *
   * @param proxyHost - The bastion/jump host hostname
   * @param proxyPort - The bastion host SSH port
   * @param proxyUser - The username for the bastion host
   * @param targetHost - The final destination hostname
   * @param targetPort - The final destination SSH port
   * @param agentSocket - Optional SSH agent socket path
   * @returns A stream that can be used as a socket for the final SSH connection
   */
  async createProxySocket(
    proxyHost: string,
    proxyPort: number,
    proxyUser: string,
    targetHost: string,
    targetPort: number,
    agentSocket?: string,
  ): Promise<ClientChannel> {
    return new Promise((resolve, reject) => {
      const proxyClient = new Client();

      proxyClient.on("ready", () => {
        // Create a forwarded connection through the bastion to the target
        proxyClient.forwardOut(
          "127.0.0.1", // Source address (doesn't matter for our use case)
          0, // Source port (0 = any available)
          targetHost, // Destination address
          targetPort, // Destination port
          (err: Error | undefined, stream: ClientChannel) => {
            if (err) {
              proxyClient.end();
              reject(
                new Error(
                  `Failed to create proxy tunnel: ${err.message}`,
                ),
              );
              return;
            }

            // Return the stream to be used as the socket for the final connection
            resolve(stream);
          },
        );
      });

      proxyClient.on("error", (err: Error) => {
        reject(
          new Error(
            `Proxy connection failed to ${proxyUser}@${proxyHost}:${proxyPort}: ${err.message}`,
          ),
        );
      });

      // Connect to the bastion host
      const connectConfig: any = {
        host: proxyHost,
        port: proxyPort,
        username: proxyUser,
        readyTimeout: 60000,
      };

      // Use SSH agent if available
      if (agentSocket) {
        connectConfig.agent = agentSocket;
      }

      proxyClient.connect(connectConfig);
    });
  }

  /**
   * Parse a proxy string in the format [user@]hostname[:port]
   *
   * @param proxy - Proxy string to parse
   * @param defaultUser - Default username if not specified in proxy string
   * @returns Parsed proxy configuration
   */
  static parseProxyString(
    proxy: string,
    defaultUser: string = "root",
  ): { user: string; host: string; port: number } {
    const proxyRegex = /^(?:([^@]+)@)?([^:]+)(?::(\d+))?$/;
    const match = proxy.match(proxyRegex);

    if (!match) {
      throw new Error(
        `Invalid proxy format: '${proxy}'. Expected: [user@]hostname[:port]`,
      );
    }

    const [, user, host, portStr] = match;

    return {
      user: user || defaultUser,
      host,
      port: portStr ? parseInt(portStr) : 22,
    };
  }
}
