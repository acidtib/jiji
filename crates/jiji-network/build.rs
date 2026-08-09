//! `jiji-proxy` is distributed as a container image and versioned/released
//! independently of both `jiji-cli` and `jiji-agent` (see
//! `proxy_script::PROXY_VERSION`/`image()`). Both binaries pull it via this
//! shared crate, so the version reference has to live here rather than in
//! either binary crate. Reading it from the workspace's own `Cargo.toml` at
//! compile time keeps it accurate to what's actually in the repo without
//! any manual upkeep.

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../lib/build-support/sibling_crate_version.rs"
));

fn main() {
    let proxy_version = sibling_crate_version(env!("CARGO_MANIFEST_DIR"), "jiji-proxy");
    println!("cargo:rustc-env=JIJI_PROXY_BUILD_VERSION={proxy_version}");
}
