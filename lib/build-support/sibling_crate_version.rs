// Shared by `crates/jiji-cli/build.rs` and `crates/jiji-network/build.rs`:
// both need to read a sibling crate's own version at compile time (since
// `jiji-agent`/`jiji-proxy` are versioned independently, see
// `version_requirements::AGENT_BUILD_VERSION` and
// `proxy_script::PROXY_VERSION`). A plain `include!`-ed file rather than a
// separate workspace crate: the latter would need its own
// `[package].version` and release-please tracking for ~15 shared lines.
//
// Include this file with an absolute path derived from `CARGO_MANIFEST_DIR`.
// This form works in rustc and rust-analyzer.

/// Reads `<manifest_dir>/../<sibling_crate_dir>/Cargo.toml`'s
/// `[package].version`, registering it for `cargo:rerun-if-changed`.
fn sibling_crate_version(manifest_dir: &str, sibling_crate_dir: &str) -> String {
    let manifest = std::path::Path::new(manifest_dir)
        .join("..")
        .join(sibling_crate_dir)
        .join("Cargo.toml");
    println!("cargo:rerun-if-changed={}", manifest.display());

    let contents = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", manifest.display()));
    let parsed: toml::Value = contents
        .parse()
        .unwrap_or_else(|err| panic!("could not parse {}: {err}", manifest.display()));
    parsed
        .get("package")
        .and_then(|package| package.get("version"))
        .and_then(|version| version.as_str())
        .unwrap_or_else(|| panic!("{} has no [package].version", manifest.display()))
        .to_string()
}
