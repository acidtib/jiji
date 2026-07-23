use std::path::Path;
use std::process::Stdio;

use anyhow::Context;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct LocalCommandResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub code: Option<i32>,
}

pub async fn run_captured(
    program: &str,
    args: &[String],
    input: Option<&[u8]>,
    cwd: Option<&Path>,
) -> anyhow::Result<LocalCommandResult> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    if input.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("Could not start local command '{program}'"))?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .expect("stdin configured above")
            .write_all(input)
            .await
            .with_context(|| format!("Could not write input to '{program}'"))?;
    }
    let output = child
        .wait_with_output()
        .await
        .with_context(|| format!("Could not wait for '{program}'"))?;
    Ok(LocalCommandResult {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: output.status.code(),
    })
}

pub async fn run_streaming(
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
) -> anyhow::Result<bool> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    Ok(command
        .status()
        .await
        .with_context(|| format!("Could not start local command '{program}'"))?
        .success())
}

pub async fn command_exists(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captures_success_failure_and_stdin() {
        let empty = Vec::new();
        assert!(
            run_captured("true", &empty, None, None)
                .await
                .unwrap()
                .success
        );
        assert!(
            !run_captured("false", &empty, None, None)
                .await
                .unwrap()
                .success
        );
        let result = run_captured("cat", &empty, Some(b"hello"), None)
            .await
            .unwrap();
        assert_eq!(result.stdout, "hello");
    }

    #[tokio::test]
    async fn detects_existing_and_missing_commands() {
        assert!(command_exists("true").await);
        assert!(!command_exists("jiji-command-that-does-not-exist").await);
    }
}
