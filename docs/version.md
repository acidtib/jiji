# Version Script

The `mise run version` task (`.mise/tasks/version` in the repo root) is a
utility for showing or bumping the workspace version. It reads and writes
`[workspace.package].version` in the root `Cargo.toml`, which every crate in
the workspace (`jiji-core`, `jiji-tui`, `jiji-config`, `jiji-network`,
`jiji-ssh`, `jiji-cli`) inherits via `version.workspace = true`.

## Usage

### Show Current Version

To display the current version:

```bash
mise run version
```

This reads the version from the `[workspace.package]` block in `./Cargo.toml`
and prints it to the console.

### Update Version

To update the version, you have two options:

#### Auto-increment Patch Version

To automatically increment the patch version (e.g., 0.1.6 -> 0.1.7):

```bash
mise run version -- --bump
```

#### Set Specific Version

To set a specific version:

```bash
mise run version -- --bump <new-version>
```

For example:

```bash
mise run version -- --bump 1.2.3
```

Both commands will:

1. Update `version` in `[workspace.package]` in `./Cargo.toml`.
2. Run `cargo update --workspace` so `Cargo.lock` picks up the new version for
   every workspace crate.

The auto increment option is useful for regular development releases where you
just need to bump the patch version.

## What It Does

The script manages exactly one source of truth: the `version` field under
`[workspace.package]` in the root `Cargo.toml`. Every workspace crate's own
`Cargo.toml` declares `version.workspace = true` rather than a literal
version, so bumping the workspace version updates all of them atomically.
`Cargo.lock` is then refreshed via `cargo update --workspace` so the lock
file's recorded versions stay consistent with the manifest.

There is no separate runtime-visible version constant to keep in sync; `jiji
version` (`crates/jiji-cli/src/commands/version.rs`) reads the version Cargo
compiled the binary with via `env!("CARGO_PKG_VERSION")`.

## Requirements

The task is a plain bash script run through `mise` (see `.mise/tasks/version`)
and needs only `sed`, `awk`, and `cargo` on `PATH` — no separate runtime or
permission model to configure.
