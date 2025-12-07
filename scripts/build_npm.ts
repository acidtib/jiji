import { build, emptyDir } from "@deno/dnt";

await emptyDir("./build/npm");

await build({
  entryPoints: [{
    kind: "bin",
    name: "jiji",
    path: "./src/main.ts",
  }],
  outDir: "./build/npm",
  shims: {
    deno: true,
  },
  scriptModule: false, // Disable CommonJS/UMD for CLI with top level await
  package: {
    name: "jiji",
    version: Deno.args[0] || "0.0.1",
    description: "Jiji - Infrastructure management tool",
    license: "MIT",
    repository: {
      type: "git",
      url: "git+https://github.com/acidtib/jiji.git",
    },
    bugs: {
      url: "https://github.com/acidtib/jiji/issues",
    },
    keywords: ["cli", "infrastructure", "management", "server", "bootstrap"],
    engines: {
      node: ">=18.0.0",
    },
    bin: {
      jiji: "./esm/main.js",
    },
  },
  postBuild() {
    // Copy additional files
    try {
      Deno.copyFileSync("README.md", "build/npm/README.md");
    } catch {
      // Create a basic README if it doesn't exist
      Deno.writeTextFileSync(
        "build/npm/README.md",
        `# Jiji

Infrastructure management tool

## Installation

\`\`\`bash
npm install -g jiji
\`\`\`

## Usage

\`\`\`bash
jiji --help
\`\`\`
`,
      );
    }

    try {
      Deno.copyFileSync("LICENSE", "build/npm/LICENSE");
    } catch {
      // Create a basic MIT license if it doesn't exist
      Deno.writeTextFileSync(
        "build/npm/LICENSE",
        `MIT License

Copyright (c) ${new Date().getFullYear()} Jiji

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.`,
      );
    }
  },
});

console.log("\nBuild complete!");
console.log("npm package created in ./build/npm/");
console.log("\nTo publish:");
console.log("  cd build/npm");
console.log("  npm publish");
