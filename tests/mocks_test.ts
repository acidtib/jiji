import { assertEquals } from "@std/assert";
import { MockSSHManager } from "./mocks.ts";

Deno.test("MockSSHManager - tracks executed commands", async () => {
  const mock = new MockSSHManager();
  await mock.executeCommand("ls -la");
  await mock.executeCommand("pwd");

  assertEquals(mock.getAllCommands().length, 2);
  assertEquals(mock.getLastCommand(), "pwd");
});

Deno.test("MockSSHManager - simulates success", async () => {
  const mock = new MockSSHManager(true);
  const result = await mock.executeCommand("test");

  assertEquals(result.success, true);
  assertEquals(result.stdout, "success");
  assertEquals(result.stderr, "");
});

Deno.test("MockSSHManager - simulates failure", async () => {
  const mock = new MockSSHManager(false);
  const result = await mock.executeCommand("test");

  assertEquals(result.success, false);
  assertEquals(result.stderr, "error");
  assertEquals(result.stdout, "");
});

Deno.test("MockSSHManager - clears commands", async () => {
  const mock = new MockSSHManager();
  await mock.executeCommand("test");
  mock.clearCommands();

  assertEquals(mock.getAllCommands().length, 0);
  assertEquals(mock.getLastCommand(), "");
});

Deno.test("MockSSHManager - getHost returns test-host", () => {
  const mock = new MockSSHManager();
  assertEquals(mock.getHost(), "test-host");
});

Deno.test("MockSSHManager - getAllCommands returns copy of array", async () => {
  const mock = new MockSSHManager();
  await mock.executeCommand("command1");

  const commands1 = mock.getAllCommands();
  const commands2 = mock.getAllCommands();

  // Should return different array instances
  assertEquals(commands1 === commands2, false);
  // But with same content
  assertEquals(commands1, commands2);
});

Deno.test("MockSSHManager - tracks multiple commands in order", async () => {
  const mock = new MockSSHManager();
  await mock.executeCommand("first");
  await mock.executeCommand("second");
  await mock.executeCommand("third");

  const commands = mock.getAllCommands();
  assertEquals(commands[0], "first");
  assertEquals(commands[1], "second");
  assertEquals(commands[2], "third");
});

Deno.test("MockSSHManager - clearCommands resets state", async () => {
  const mock = new MockSSHManager();
  await mock.executeCommand("before clear");
  mock.clearCommands();
  await mock.executeCommand("after clear");

  const commands = mock.getAllCommands();
  assertEquals(commands.length, 1);
  assertEquals(commands[0], "after clear");
  assertEquals(mock.getLastCommand(), "after clear");
});
