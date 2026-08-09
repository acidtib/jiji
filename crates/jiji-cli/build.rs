use std::process::Command;

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../lib/build-support/sibling_crate_version.rs"
));

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=JIJI_GIT_SHA={sha}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");

    // `jiji-agent` is versioned independently of `jiji-cli` (see
    // `version_requirements.rs`), so a fresh `jiji server setup` needs to
    // know which agent build this CLI was actually released alongside,
    // without assuming it shares the CLI's own version.
    let agent_version = sibling_crate_version(env!("CARGO_MANIFEST_DIR"), "jiji-agent");
    println!("cargo:rustc-env=JIJI_AGENT_BUILD_VERSION={agent_version}");
}
