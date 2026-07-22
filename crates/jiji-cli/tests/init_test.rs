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

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Configuration is valid"));
    assert!(stdout.contains("Configuration file created"));
}

// The overwrite-confirmation path (`init` on an existing config) is not covered here:
// `dialoguer::Confirm` requires a real TTY, and piping stdin in a test harness surfaces as
// "not a terminal" rather than exercising the prompt. Verified manually instead (see plan's
// verification section) — `jiji init` twice in a scratch dir and confirm the prompt appears.
