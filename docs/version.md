# Version Script

The `./bin/version` script is a utility for managing the version of the jiji project. It handles reading and updating version information in both the source code and the Deno configuration.

## Usage

### Show Current Version

To display the current version:

```bash
./bin/version
```

This will read the version from `./src/version.ts` and print it to the console.

### Update Version

To update the version:

```bash
./bin/version --bump <new-version>
```

For example:

```bash
./bin/version --bump 1.2.3
```

This command will:
1. Update the version in `./src/version.ts`
2. Update the version in `./deno.json`

## What It Does

The script manages version information in two places:

1. **`./src/version.ts`** - Contains the TypeScript constant that represents the version in the source code
2. **`./deno.json`** - Contains the version field in the Deno configuration file

When updating the version, the script ensures both files are updated atomically, so they always contain the same version information.

## Permissions

The script requires the following Deno permissions:
- `--allow-read` to read the version file and Deno configuration
- `--allow-write` to update the version file and Deno configuration