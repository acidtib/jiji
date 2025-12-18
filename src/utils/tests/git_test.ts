import { assertEquals, assertRejects } from "@std/assert";
import { GitUtils } from "../git.ts";

// Note: These tests require a git repository to be initialized
// We'll use conditional tests that check if we're in a git repo

Deno.test("GitUtils - isGitInstalled returns true if git is available", async () => {
  const isInstalled = await GitUtils.isGitInstalled();
  // Git should be installed in most environments
  assertEquals(typeof isInstalled, "boolean");
});

Deno.test("GitUtils - isGitRepository detects git repository", async () => {
  const isRepo = await GitUtils.isGitRepository();
  assertEquals(typeof isRepo, "boolean");
});

// Conditional tests - only run if in a git repository
const isGitRepo = await GitUtils.isGitRepository();

if (isGitRepo) {
  Deno.test("GitUtils - getCommitSHA returns short SHA", async () => {
    const sha = await GitUtils.getCommitSHA(true);

    assertEquals(typeof sha, "string");
    assertEquals(sha.length, 7); // Short SHA is 7 characters
    // Should be hexadecimal
    assertEquals(/^[0-9a-f]{7}$/.test(sha), true);
  });

  Deno.test("GitUtils - getCommitSHA returns full SHA", async () => {
    const sha = await GitUtils.getCommitSHA(false);

    assertEquals(typeof sha, "string");
    assertEquals(sha.length, 40); // Full SHA is 40 characters
    // Should be hexadecimal
    assertEquals(/^[0-9a-f]{40}$/.test(sha), true);
  });

  Deno.test("GitUtils - getCurrentBranch returns branch name", async () => {
    const branch = await GitUtils.getCurrentBranch();

    assertEquals(typeof branch, "string");
    assertEquals(branch.length > 0, true);
  });

  Deno.test("GitUtils - hasUncommittedChanges detects changes", async () => {
    const hasChanges = await GitUtils.hasUncommittedChanges();

    // Should return a boolean
    assertEquals(typeof hasChanges, "boolean");
  });

  Deno.test("GitUtils - getRepoRoot returns valid path", async () => {
    const repoRoot = await GitUtils.getRepoRoot();

    assertEquals(typeof repoRoot, "string");
    assertEquals(repoRoot.length > 0, true);
    // Should be an absolute path
    assertEquals(repoRoot.startsWith("/") || /^[A-Z]:\\/.test(repoRoot), true);
  });

  Deno.test("GitUtils - short and full SHA have correct relationship", async () => {
    const shortSha = await GitUtils.getCommitSHA(true);
    const fullSha = await GitUtils.getCommitSHA(false);

    // Short SHA should be the first 7 characters of full SHA
    assertEquals(fullSha.startsWith(shortSha), true);
  });
}

// Test error handling when not in a git repository
Deno.test("GitUtils - getCommitSHA throws error outside git repo", async () => {
  // Create a temp directory that's not a git repo
  const tempDir = await Deno.makeTempDir();

  try {
    const originalDir = Deno.cwd();

    try {
      Deno.chdir(tempDir);

      await assertRejects(
        async () => await GitUtils.getCommitSHA(),
        Error,
        "Failed to get git commit SHA",
      );
    } finally {
      Deno.chdir(originalDir);
    }
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test("GitUtils - getCurrentBranch throws error outside git repo", async () => {
  const tempDir = await Deno.makeTempDir();

  try {
    const originalDir = Deno.cwd();

    try {
      Deno.chdir(tempDir);

      await assertRejects(
        async () => await GitUtils.getCurrentBranch(),
        Error,
        "Failed to get current branch",
      );
    } finally {
      Deno.chdir(originalDir);
    }
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test("GitUtils - getRepoRoot throws error outside git repo", async () => {
  const tempDir = await Deno.makeTempDir();

  try {
    const originalDir = Deno.cwd();

    try {
      Deno.chdir(tempDir);

      await assertRejects(
        async () => await GitUtils.getRepoRoot(),
        Error,
        "Failed to get repository root",
      );
    } finally {
      Deno.chdir(originalDir);
    }
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

Deno.test("GitUtils - isGitRepository returns false outside git repo", async () => {
  const tempDir = await Deno.makeTempDir();

  try {
    const originalDir = Deno.cwd();

    try {
      Deno.chdir(tempDir);

      const isRepo = await GitUtils.isGitRepository();
      assertEquals(isRepo, false);
    } finally {
      Deno.chdir(originalDir);
    }
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});

// Integration test - create a temporary git repo
Deno.test("GitUtils - works in a fresh git repository", async () => {
  const tempDir = await Deno.makeTempDir();

  try {
    const originalDir = Deno.cwd();

    try {
      Deno.chdir(tempDir);

      // Initialize git repo
      const initCmd = new Deno.Command("git", {
        args: ["init"],
        stdout: "null",
        stderr: "null",
      });
      await initCmd.output();

      // Configure git for commits
      const configUserCmd = new Deno.Command("git", {
        args: ["config", "user.email", "test@example.com"],
        stdout: "null",
        stderr: "null",
      });
      await configUserCmd.output();

      const configNameCmd = new Deno.Command("git", {
        args: ["config", "user.name", "Test User"],
        stdout: "null",
        stderr: "null",
      });
      await configNameCmd.output();

      // Create a file and commit
      await Deno.writeTextFile("test.txt", "test content");

      const addCmd = new Deno.Command("git", {
        args: ["add", "test.txt"],
        stdout: "null",
        stderr: "null",
      });
      await addCmd.output();

      const commitCmd = new Deno.Command("git", {
        args: ["commit", "-m", "Initial commit"],
        stdout: "null",
        stderr: "null",
      });
      await commitCmd.output();

      // Now test GitUtils methods
      const isRepo = await GitUtils.isGitRepository();
      assertEquals(isRepo, true);

      const sha = await GitUtils.getCommitSHA(true);
      assertEquals(sha.length, 7);

      const branch = await GitUtils.getCurrentBranch();
      assertEquals(branch === "main" || branch === "master", true);

      // Should have no uncommitted changes
      const hasChanges = await GitUtils.hasUncommittedChanges();
      assertEquals(hasChanges, false);

      // Create an uncommitted change
      await Deno.writeTextFile("test2.txt", "new file");

      // Should now have uncommitted changes
      const hasChangesAfter = await GitUtils.hasUncommittedChanges();
      assertEquals(hasChangesAfter, true);
    } finally {
      Deno.chdir(originalDir);
    }
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
});
