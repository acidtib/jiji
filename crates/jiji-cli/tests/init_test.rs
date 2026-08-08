use std::fs;
use std::process::Command;

#[test]
fn init_writes_config_and_exits_successfully() {
    let dir = tempfile::tempdir().expect("create temp dir");

    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("init")
        .current_dir(dir.path())
        .output()
        .expect("run jiji init");

    assert!(
        output.status.success(),
        "jiji init should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let written = dir.path().join(".jiji/deploy.yml");
    assert!(written.exists(), "expected .jiji/deploy.yml to be created");

    let contents = fs::read_to_string(&written).expect("read written config");
    assert!(contents.contains("project: myproject"));
    let config: serde_yaml::Value =
        serde_yaml::from_str(&contents).expect("generated config should be valid YAML");
    let network = config
        .get("network")
        .expect("generated config should persist project-specific network ranges");
    let management_cidr = network["management_cidr"]
        .as_str()
        .expect("management_cidr should be a string");
    let container_cidr = network["container_cidr"]
        .as_str()
        .expect("container_cidr should be a string");
    assert!(management_cidr.starts_with("198.18."));
    assert!(management_cidr.ends_with(".0/24"));
    assert!(container_cidr.starts_with("100."));
    assert!(container_cidr.ends_with(".0.0/16"));

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Configuration is valid"));
    assert!(stdout.contains("Configuration file created"));
    assert!(
        !stdout.contains('\u{1b}'),
        "captured output should not contain ANSI styling: {stdout:?}"
    );
}

// The overwrite-confirmation path (`init` on an existing config) is not covered here:
// `dialoguer::Confirm` requires a real TTY, and piping stdin in a test harness surfaces as
// "not a terminal" rather than exercising the prompt. Verified manually instead (see plan's
// verification section) — `jiji init` twice in a scratch dir and confirm the prompt appears.
