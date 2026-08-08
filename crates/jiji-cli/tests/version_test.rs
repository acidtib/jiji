use std::process::Command;

#[test]
fn version_prints_cargo_package_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_jiji"))
        .arg("version")
        .output()
        .expect("run jiji version");

    assert!(
        output.status.success(),
        "jiji version should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&format!("Jiji v{}", env!("CARGO_PKG_VERSION"))));
    assert!(stdout.contains(env!("JIJI_GIT_SHA")));
    assert!(
        !stdout.contains('\u{1b}'),
        "captured output should not contain ANSI styling: {stdout:?}"
    );
}
