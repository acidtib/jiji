import { assertEquals, assertThrows } from "@std/assert";
import { ServiceConfiguration } from "../service.ts";
import { ConfigurationError } from "../base.ts";

// Test data for files and directories
Deno.test("ServiceConfiguration - files as string array", () => {
  const serviceData = {
    image: "nginx:latest",
    hosts: ["localhost"],
    files: [
      "nginx.conf:/etc/nginx/nginx.conf:ro",
      "config.yml:/app/config.yml",
    ],
  };

  const service = new ServiceConfiguration("web", serviceData, "myproject");

  assertEquals(service.files.length, 2);
  assertEquals(service.files[0], "nginx.conf:/etc/nginx/nginx.conf:ro");
  assertEquals(service.files[1], "config.yml:/app/config.yml");
});

Deno.test("ServiceConfiguration - files as hash array", () => {
  const serviceData = {
    image: "nginx:latest",
    hosts: ["localhost"],
    files: [
      {
        local: "nginx.conf",
        remote: "/etc/nginx/nginx.conf",
        mode: "0644",
        owner: "nginx:nginx",
        options: "ro",
      },
    ],
  };

  const service = new ServiceConfiguration("web", serviceData, "myproject");

  assertEquals(service.files.length, 1);
  const file = service.files[0] as {
    local: string;
    remote: string;
    mode: string;
    owner: string;
    options: string;
  };
  assertEquals(file.local, "nginx.conf");
  assertEquals(file.remote, "/etc/nginx/nginx.conf");
  assertEquals(file.mode, "0644");
  assertEquals(file.owner, "nginx:nginx");
  assertEquals(file.options, "ro");
});

Deno.test("ServiceConfiguration - directories as string array", () => {
  const serviceData = {
    image: "nginx:latest",
    hosts: ["localhost"],
    directories: [
      "html:/usr/share/nginx/html:ro",
      "uploads:/var/uploads:z",
    ],
  };

  const service = new ServiceConfiguration("web", serviceData, "myproject");

  assertEquals(service.directories.length, 2);
  assertEquals(service.directories[0], "html:/usr/share/nginx/html:ro");
  assertEquals(service.directories[1], "uploads:/var/uploads:z");
});

Deno.test("ServiceConfiguration - directories as hash array", () => {
  const serviceData = {
    image: "mysql:latest",
    hosts: ["localhost"],
    directories: [
      {
        local: "mysql-data",
        remote: "/var/lib/mysql",
        mode: "0750",
        owner: "mysql:mysql",
        options: "z",
      },
    ],
  };

  const service = new ServiceConfiguration("db", serviceData, "myproject");

  assertEquals(service.directories.length, 1);
  const dir = service.directories[0] as {
    local: string;
    remote: string;
    mode: string;
    owner: string;
    options: string;
  };
  assertEquals(dir.local, "mysql-data");
  assertEquals(dir.remote, "/var/lib/mysql");
  assertEquals(dir.mode, "0750");
  assertEquals(dir.owner, "mysql:mysql");
  assertEquals(dir.options, "z");
});

Deno.test("ServiceConfiguration - validation fails with invalid file format", () => {
  const serviceData = {
    image: "nginx:latest",
    hosts: ["localhost"],
    files: ["invalid"], // Missing colon separator
  };

  const service = new ServiceConfiguration("web", serviceData, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Invalid file mount",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid directory format", () => {
  const serviceData = {
    image: "nginx:latest",
    hosts: ["localhost"],
    directories: ["dir:path:invalid:too:many:colons"],
  };

  const service = new ServiceConfiguration("web", serviceData, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Invalid directory mount",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid file hash format", () => {
  const serviceData = {
    image: "nginx:latest",
    hosts: ["localhost"],
    files: [
      {
        local: "config.yml",
        // Missing 'remote' field
      },
    ],
  };

  const service = new ServiceConfiguration("web", serviceData, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Invalid file mount",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid mode format", () => {
  const serviceData = {
    image: "nginx:latest",
    hosts: ["localhost"],
    files: [
      {
        local: "config.yml",
        remote: "/app/config.yml",
        mode: "999", // Invalid octal mode
      },
    ],
  };

  const service = new ServiceConfiguration("web", serviceData, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Invalid file mount",
  );
});

Deno.test("ServiceConfiguration - validation fails with invalid owner format", () => {
  const serviceData = {
    image: "nginx:latest",
    hosts: ["localhost"],
    directories: [
      {
        local: "data",
        remote: "/var/data",
        owner: "invalid-owner", // Missing colon separator
      },
    ],
  };

  const service = new ServiceConfiguration("web", serviceData, "myproject");

  assertThrows(
    () => service.validate(),
    ConfigurationError,
    "Invalid directory mount",
  );
});

Deno.test("ServiceConfiguration - toObject includes files and directories", () => {
  const serviceData = {
    image: "nginx:latest",
    hosts: ["localhost"],
    files: ["nginx.conf:/etc/nginx/nginx.conf:ro"],
    directories: ["html:/usr/share/nginx/html"],
  };

  const service = new ServiceConfiguration("web", serviceData, "myproject");
  const obj = service.toObject();

  assertEquals(obj.files, ["nginx.conf:/etc/nginx/nginx.conf:ro"]);
  assertEquals(obj.directories, ["html:/usr/share/nginx/html"]);
});

Deno.test("ServiceConfiguration - toObject excludes empty files and directories", () => {
  const serviceData = {
    image: "nginx:latest",
    hosts: ["localhost"],
  };

  const service = new ServiceConfiguration("web", serviceData, "myproject");
  const obj = service.toObject();

  assertEquals("files" in obj, false);
  assertEquals("directories" in obj, false);
});

Deno.test("ServiceConfiguration - validates files with valid SELinux options", () => {
  const serviceData = {
    image: "nginx:latest",
    hosts: ["localhost"],
    files: [
      "config.yml:/app/config.yml:z",
      "secret.key:/app/secret.key:Z",
    ],
  };

  const service = new ServiceConfiguration("web", serviceData, "myproject");

  // Should not throw
  service.validate();
});

Deno.test("ServiceConfiguration - validates directories with valid permissions", () => {
  const serviceData = {
    image: "mysql:latest",
    hosts: ["localhost"],
    directories: [
      {
        local: "data",
        remote: "/var/lib/mysql",
        mode: "0755",
        owner: "1000:1000",
      },
    ],
  };

  const service = new ServiceConfiguration("db", serviceData, "myproject");

  // Should not throw
  service.validate();
});
