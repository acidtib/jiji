import { assertEquals } from "@std/assert";
import { SSHConfigParser } from "../../../utils/ssh_config_parser.ts";

Deno.test("SSHConfigParser - Basic parsing", () => {
  const parser = new SSHConfigParser();
  const config = `
# This is a comment
Host example.com
    User myuser
    Port 2222
    IdentityFile ~/.ssh/id_rsa

Host *.dev
    User developer
    ProxyJump bastion.example.com

Host *
    User root
    ConnectTimeout 30
`;

  parser.parseContent(config);
  const hosts = parser.getHosts();

  assertEquals(hosts.length, 3);
  assertEquals(hosts[0].pattern, "example.com");
  assertEquals(hosts[0].config.user, "myuser");
  assertEquals(hosts[0].config.port, "2222");
  assertEquals(hosts[0].config.identityfile, "~/.ssh/id_rsa");

  assertEquals(hosts[1].pattern, "*.dev");
  assertEquals(hosts[1].config.user, "developer");
  assertEquals(hosts[1].config.proxyjump, "bastion.example.com");

  assertEquals(hosts[2].pattern, "*");
  assertEquals(hosts[2].config.user, "root");
  assertEquals(hosts[2].config.connecttimeout, "30");
});

Deno.test("SSHConfigParser - Host pattern matching", () => {
  const parser = new SSHConfigParser();

  // Test pattern matching by creating configs and checking results
  const config1 = `
Host example.com
    User test1

Host *.com
    User test2

Host *.example.com
    User test3

Host example.*
    User test4

Host ?est
    User test5

Host ??st
    User test6

Host *
    User testall
`;

  parser.parseContent(config1);

  // Test exact match
  const exact = parser.getConfigForHost("example.com");
  assertEquals(exact.user, "test1");

  // Test wildcard matches
  const wildcard = parser.getConfigForHost("other.com");
  assertEquals(wildcard.user, "test2");

  const subdomain = parser.getConfigForHost("test.example.com");
  assertEquals(subdomain.user, "test2");

  const extension = parser.getConfigForHost("example.org");
  assertEquals(extension.user, "test4");

  // Test single char wildcard
  parser.clear();
  const config2 = `
Host ?est
    User singlechar
`;
  parser.parseContent(config2);
  const singleChar = parser.getConfigForHost("test");
  assertEquals(singleChar.user, "singlechar");
});

Deno.test("SSHConfigParser - Multiple patterns in single Host", () => {
  const parser = new SSHConfigParser();
  const config = `
Host web1.example.com web2.example.com *.staging.com
    User deploy
    Port 2222
`;

  parser.parseContent(config);

  const config1 = parser.getConfigForHost("web1.example.com");
  assertEquals(config1.user, "deploy");

  const config2 = parser.getConfigForHost("web2.example.com");
  assertEquals(config2.user, "deploy");

  const config3 = parser.getConfigForHost("app.staging.com");
  assertEquals(config3.user, "deploy");

  const config4 = parser.getConfigForHost("other.com");
  assertEquals(config4.user, undefined);
});

Deno.test("SSHConfigParser - Negation patterns", () => {
  const parser = new SSHConfigParser();
  const config = `
Host *.example.com !private.example.com
    User public

Host private.example.com
    User admin
`;

  parser.parseContent(config);

  const publicConfig = parser.getConfigForHost("web.example.com");
  assertEquals(publicConfig.user, "public");

  const privateConfig = parser.getConfigForHost("private.example.com");
  assertEquals(privateConfig.user, "admin");
});

Deno.test("SSHConfigParser - Configuration precedence", () => {
  const parser = new SSHConfigParser();
  const config = `
Host *.example.com
    User deploy
    Port 2222
    ConnectTimeout 10

Host web.example.com
    User webuser
    ConnectTimeout 30

Host *
    User root
    Port 22
`;

  parser.parseContent(config);

  // web.example.com should get configuration from first matching pattern (*.example.com)
  const webConfig = parser.getConfigForHost("web.example.com");
  assertEquals(webConfig.user, "deploy"); // First match takes precedence
  assertEquals(webConfig.port, "2222"); // From *.example.com
  assertEquals(webConfig.connecttimeout, "10"); // First match takes precedence

  // api.example.com should only match *.example.com and *
  const apiConfig = parser.getConfigForHost("api.example.com");
  assertEquals(apiConfig.user, "deploy");
  assertEquals(apiConfig.port, "2222");
  assertEquals(apiConfig.connecttimeout, "10");

  // other.com should only match *
  const otherConfig = parser.getConfigForHost("other.com");
  assertEquals(otherConfig.user, "root");
  assertEquals(otherConfig.port, "22");
});

Deno.test("SSHConfigParser - Case insensitive options", () => {
  const parser = new SSHConfigParser();
  const config = `
Host example.com
    USER myuser
    PORT 2222
    IdentityFile ~/.ssh/key
    ProxyJump proxy.com
    ProxyCommand ssh -W %h:%p proxy.com
`;

  parser.parseContent(config);
  const hosts = parser.getHosts();

  assertEquals(hosts[0].config.user, "myuser");
  assertEquals(hosts[0].config.port, "2222");
  assertEquals(hosts[0].config.identityfile, "~/.ssh/key");
  assertEquals(hosts[0].config.proxyjump, "proxy.com");
  assertEquals(hosts[0].config.proxycommand, "ssh -W %h:%p proxy.com");
});

Deno.test("SSHConfigParser - Comments and empty lines", () => {
  const parser = new SSHConfigParser();
  const config = `
# Global comment

Host example.com
    # This is a comment
    User myuser

    # Another comment
    Port 2222

# Host commented.com
#     User test

Host other.com
    User other
`;

  parser.parseContent(config);
  const hosts = parser.getHosts();

  assertEquals(hosts.length, 2);
  assertEquals(hosts[0].pattern, "example.com");
  assertEquals(hosts[0].config.user, "myuser");
  assertEquals(hosts[0].config.port, "2222");

  assertEquals(hosts[1].pattern, "other.com");
  assertEquals(hosts[1].config.user, "other");
});

Deno.test("SSHConfigParser - Jiji relevant config extraction", () => {
  const parser = new SSHConfigParser();
  const config = `
Host example.com
    Hostname 192.168.1.100
    User deploy
    Port 2222
    IdentityFile ~/.ssh/deploy_key
    ProxyJump bastion.example.com
    ProxyCommand ssh -W %h:%p bastion
    ConnectTimeout 45
    ServerAliveInterval 60
    ServerAliveCountMax 3
    Compression yes
    ForwardAgent yes
    StrictHostKeyChecking no
    UnknownOption value
`;

  parser.parseContent(config);
  const jijiConfig = parser.getJijiRelevantConfig("example.com");

  assertEquals(jijiConfig.hostname, "192.168.1.100");
  assertEquals(jijiConfig.user, "deploy");
  assertEquals(jijiConfig.port, 2222);
  assertEquals(jijiConfig.identityFile, "~/.ssh/deploy_key");
  assertEquals(jijiConfig.proxyJump, "bastion.example.com");
  assertEquals(jijiConfig.proxyCommand, "ssh -W %h:%p bastion");
  assertEquals(jijiConfig.connectTimeout, 45);
  assertEquals(jijiConfig.serverAliveInterval, 60);
  assertEquals(jijiConfig.serverAliveCountMax, 3);
  assertEquals(jijiConfig.compression, true);
  assertEquals(jijiConfig.forwardAgent, true);
  assertEquals(jijiConfig.strictHostKeyChecking, false);

  // Unknown options should not be included
  assertEquals(
    (jijiConfig as Record<string, unknown>).unknownoption,
    undefined,
  );
});

Deno.test("SSHConfigParser - Boolean option parsing", () => {
  const parser = new SSHConfigParser();
  const config = `
Host test1.com
    Compression yes
    ForwardAgent true
    StrictHostKeyChecking on

Host test2.com
    Compression no
    ForwardAgent false
    StrictHostKeyChecking off
`;

  parser.parseContent(config);

  const config1 = parser.getJijiRelevantConfig("test1.com");
  assertEquals(config1.compression, true);
  assertEquals(config1.forwardAgent, true);
  assertEquals(config1.strictHostKeyChecking, true);

  const config2 = parser.getJijiRelevantConfig("test2.com");
  assertEquals(config2.compression, false);
  assertEquals(config2.forwardAgent, false);
  assertEquals(config2.strictHostKeyChecking, false);
});

Deno.test("SSHConfigParser - Empty and malformed configs", () => {
  const parser = new SSHConfigParser();

  // Empty config
  parser.parseContent("");
  assertEquals(parser.getHosts().length, 0);
  assertEquals(Object.keys(parser.getConfigForHost("any.com")).length, 0);

  // Only comments
  parser.clear();
  parser.parseContent(`
# Only comments
# No actual config
`);
  assertEquals(parser.getHosts().length, 0);

  // Malformed entries (missing values)
  parser.clear();
  parser.parseContent(`
Host example.com
    User
    Port
    ValidOption value
`);

  const hosts = parser.getHosts();
  assertEquals(hosts.length, 1);
  assertEquals(hosts[0].config.user, undefined);
  assertEquals(hosts[0].config.port, undefined);
  assertEquals(hosts[0].config.validoption, "value");
});

Deno.test("SSHConfigParser - Configuration without Host", () => {
  const parser = new SSHConfigParser();
  const config = `
# Configuration without Host directive should be ignored
User globaluser
Port 2222

Host example.com
    User localuser
`;

  parser.parseContent(config);
  const hosts = parser.getHosts();

  assertEquals(hosts.length, 1);
  assertEquals(hosts[0].pattern, "example.com");
  assertEquals(hosts[0].config.user, "localuser");

  // Global config should not affect host config
  const hostConfig = parser.getConfigForHost("example.com");
  assertEquals(hostConfig.user, "localuser");
  assertEquals(hostConfig.port, undefined);
});

Deno.test("SSHConfigParser - Multiple config files parsing", async () => {
  const parser = new SSHConfigParser();

  // Create temporary config files
  const config1 = `
Host web.example.com
    User webuser
    Port 8080
`;

  const config2 = `
Host db.example.com
    User dbuser
    Port 5432

Host web.example.com
    ConnectTimeout 60
`;

  const tempDir = await Deno.makeTempDir();
  const file1 = `${tempDir}/config1`;
  const file2 = `${tempDir}/config2`;

  await Deno.writeTextFile(file1, config1);
  await Deno.writeTextFile(file2, config2);

  try {
    await parser.parseFiles([file1, file2]);

    // Should have both hosts
    const hosts = parser.getHosts();
    assertEquals(hosts.length, 3);

    // web.example.com should have config from both files
    const webConfig = parser.getConfigForHost("web.example.com");
    assertEquals(webConfig.user, "webuser");
    assertEquals(webConfig.port, "8080");
    assertEquals(webConfig.connecttimeout, "60");

    // db.example.com should only have config from file2
    const dbConfig = parser.getConfigForHost("db.example.com");
    assertEquals(dbConfig.user, "dbuser");
    assertEquals(dbConfig.port, "5432");
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test("SSHConfigParser - File not found handling", async () => {
  const parser = new SSHConfigParser();

  // Should not throw when file doesn't exist
  await parser.parseFile("/nonexistent/config/file");
  assertEquals(parser.getHosts().length, 0);
});
