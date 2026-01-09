import { assertEquals } from "@std/assert";
import {
  buildAllMountArgs,
  buildDirectoryMountArgs,
  buildFileMountArgs,
  parseMountConfig,
} from "../mount_manager.ts";

Deno.test("MountManager - parse string format simple", () => {
  const result = parseMountConfig("local:remote");

  assertEquals(result.local, "local");
  assertEquals(result.remote, "remote");
  assertEquals(result.mode, undefined);
  assertEquals(result.owner, undefined);
  assertEquals(result.options, undefined);
});

Deno.test("MountManager - parse string format with options", () => {
  const result = parseMountConfig("config.yml:/app/config.yml:ro");

  assertEquals(result.local, "config.yml");
  assertEquals(result.remote, "/app/config.yml");
  assertEquals(result.options, "ro");
});

Deno.test("MountManager - parse hash format", () => {
  const result = parseMountConfig({
    local: "nginx.conf",
    remote: "/etc/nginx/nginx.conf",
    mode: "0644",
    owner: "nginx:nginx",
    options: "ro",
  });

  assertEquals(result.local, "nginx.conf");
  assertEquals(result.remote, "/etc/nginx/nginx.conf");
  assertEquals(result.mode, "0644");
  assertEquals(result.owner, "nginx:nginx");
  assertEquals(result.options, "ro");
});

Deno.test("MountManager - buildFileMountArgs with string format", () => {
  const files = [
    "nginx.conf:/etc/nginx/nginx.conf:ro",
    "config.yml:/app/config.yml",
  ];

  const args = buildFileMountArgs(files, "myproject", "web");

  assertEquals(args.length, 2);
  assertEquals(
    args[0],
    "-v .jiji/myproject/files/web/nginx.conf:/etc/nginx/nginx.conf:ro",
  );
  assertEquals(
    args[1],
    "-v .jiji/myproject/files/web/config.yml:/app/config.yml",
  );
});

Deno.test("MountManager - buildFileMountArgs with hash format", () => {
  const files = [
    {
      local: "secret.key",
      remote: "/etc/app/secret.key",
      mode: "0600",
      owner: "app:app",
      options: "ro",
    },
  ];

  const args = buildFileMountArgs(files, "myproject", "api");

  assertEquals(args.length, 1);
  assertEquals(
    args[0],
    "-v .jiji/myproject/files/api/secret.key:/etc/app/secret.key:ro",
  );
});

Deno.test("MountManager - buildDirectoryMountArgs with string format", () => {
  const directories = [
    "html:/usr/share/nginx/html:ro",
    "uploads:/var/uploads:z",
  ];

  const args = buildDirectoryMountArgs(directories, "myproject", "web");

  assertEquals(args.length, 2);
  assertEquals(
    args[0],
    "-v .jiji/myproject/directories/web/html:/usr/share/nginx/html:ro",
  );
  assertEquals(
    args[1],
    "-v .jiji/myproject/directories/web/uploads:/var/uploads:z",
  );
});

Deno.test("MountManager - buildDirectoryMountArgs with hash format", () => {
  const directories = [
    {
      local: "mysql-data",
      remote: "/var/lib/mysql",
      mode: "0750",
      owner: "mysql:mysql",
      options: "Z",
    },
  ];

  const args = buildDirectoryMountArgs(directories, "myproject", "database");

  assertEquals(args.length, 1);
  assertEquals(
    args[0],
    "-v .jiji/myproject/directories/database/mysql-data:/var/lib/mysql:Z",
  );
});

Deno.test("MountManager - buildAllMountArgs combines all mount types", () => {
  const files = ["config.yml:/app/config.yml:ro"];
  const directories = ["data:/var/data:z"];
  const volumes = ["/host/path:/container/path"];

  const args = buildAllMountArgs(
    files,
    directories,
    volumes,
    "myproject",
    "api",
  );

  assertEquals(
    args,
    "-v .jiji/myproject/files/api/config.yml:/app/config.yml:ro -v .jiji/myproject/directories/api/data:/var/data:z -v /host/path:/container/path",
  );
});

Deno.test("MountManager - buildAllMountArgs with empty arrays", () => {
  const args = buildAllMountArgs([], [], [], "myproject", "web");

  assertEquals(args, "");
});

Deno.test("MountManager - buildFileMountArgs without options", () => {
  const files = ["config.yml:/app/config.yml"];

  const args = buildFileMountArgs(files, "myproject", "app");

  assertEquals(args.length, 1);
  assertEquals(
    args[0],
    "-v .jiji/myproject/files/app/config.yml:/app/config.yml",
  );
});

Deno.test("MountManager - buildDirectoryMountArgs without options", () => {
  const directories = [
    {
      local: "data",
      remote: "/var/data",
      mode: "0755",
    },
  ];

  const args = buildDirectoryMountArgs(directories, "myproject", "worker");

  assertEquals(args.length, 1);
  assertEquals(
    args[0],
    "-v .jiji/myproject/directories/worker/data:/var/data",
  );
});

Deno.test("MountManager - parse hash format minimal", () => {
  const result = parseMountConfig({
    local: "file.txt",
    remote: "/app/file.txt",
  });

  assertEquals(result.local, "file.txt");
  assertEquals(result.remote, "/app/file.txt");
  assertEquals(result.mode, undefined);
  assertEquals(result.owner, undefined);
  assertEquals(result.options, undefined);
});
