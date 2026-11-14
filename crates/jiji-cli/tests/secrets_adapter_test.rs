//! `secrets: { adapter: doppler }` against a fake `doppler` executable first on PATH that logs
//! its argv and prints canned output.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Project {
    dir: tempfile::TempDir,
    bin: PathBuf,
}

impl Project {
    fn new(secrets_block: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join(".jiji")).expect("create .jiji");
        std::fs::write(
            dir.path().join(".jiji/deploy.yml"),
            format!(
                r#"
project: demo
builder: {{ engine: docker }}
servers:
  app: {{ host: 192.0.2.10 }}
services:
  web:
    image: example/web:latest
    servers: [app]
    environment:
      secrets: [API_TOKEN, DATABASE_URL]
ssh: {{ user: root }}
{secrets_block}
"#
            ),
        )
        .expect("write config");
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).expect("create bin");
        Project { dir, bin }
    }

    fn log_path(&self) -> PathBuf {
        self.dir.path().join("doppler.log")
    }

    fn invocations(&self) -> Vec<String> {
        std::fs::read_to_string(self.log_path())
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn write_fake_doppler(&self, stdout: &str, stderr: &str, exit_code: i32) {
        let stdout_path = self.dir.path().join("doppler.stdout");
        std::fs::write(&stdout_path, stdout).expect("write canned stdout");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{log}'\ncat '{stdout}'\nprintf '%s' '{stderr}' >&2\nexit {exit_code}\n",
            log = self.log_path().display(),
            stdout = stdout_path.display(),
        );
        let path = self.bin.join("doppler");
        std::fs::write(&path, script).expect("write fake doppler");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake doppler");
    }

    fn run(&self, args: &[&str], path: Option<&Path>) -> Output {
        let search_path = match path {
            Some(path) => path.as_os_str().to_owned(),
            None => {
                let current = std::env::var_os("PATH").unwrap_or_default();
                let mut paths = vec![self.bin.clone()];
                paths.extend(std::env::split_paths(&current));
                std::env::join_paths(paths).expect("join PATH")
            }
        };
        Command::new(env!("CARGO_BIN_EXE_jiji"))
            .args(args)
            .current_dir(self.dir.path())
            .env("PATH", search_path)
            .env_remove("DOPPLER_TOKEN")
            .output()
            .expect("run jiji")
    }
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

const SCOPED: &str = "secrets:\n  adapter: doppler\n  project: my-app\n  config: prd\n";

#[test]
fn secrets_print_passes_project_and_config_and_reports_status() {
    let project = Project::new(SCOPED);
    project.write_fake_doppler(r#"{"API_TOKEN":"tok-from-doppler"}"#, "", 0);
    std::fs::write(
        project.dir.path().join(".env"),
        "DATABASE_URL=from-dotenv\n",
    )
    .expect("write .env");

    let output = project.run(&["secrets", "print"], None);
    let text = combined(&output);

    assert!(output.status.success(), "{text}");
    assert_eq!(
        project.invocations(),
        ["secrets download --no-file --format json --project my-app --config prd"]
    );
    assert!(
        text.contains("Secrets adapter: doppler (project my-app, config prd)"),
        "{text}"
    );
    assert!(!text.contains("Secrets file:"), "{text}");
    let token_line = text
        .lines()
        .find(|line| line.contains("API_TOKEN"))
        .unwrap();
    assert!(token_line.contains("[SET]"), "{text}");
    // .env is not read when an adapter is configured.
    let database_line = text
        .lines()
        .find(|line| line.contains("DATABASE_URL"))
        .unwrap();
    assert!(database_line.contains("[MISSING]"), "{text}");
    assert!(!text.contains("tok-from-doppler"), "{text}");
}

#[test]
fn secrets_print_show_values_reveals_adapter_values() {
    let project = Project::new(SCOPED);
    project.write_fake_doppler(
        r#"{"API_TOKEN":"tok-from-doppler","DATABASE_URL":"postgres://db"}"#,
        "",
        0,
    );

    let output = project.run(&["secrets", "print", "--show-values"], None);
    let text = combined(&output);

    assert!(output.status.success(), "{text}");
    assert!(text.contains("tok-from-doppler"), "{text}");
    assert!(
        text.contains("All secrets are configured correctly"),
        "{text}"
    );
}

#[test]
fn omitted_project_and_config_are_not_passed() {
    let project = Project::new("secrets:\n  adapter: doppler\n");
    project.write_fake_doppler(r#"{"API_TOKEN":"a","DATABASE_URL":"b"}"#, "", 0);

    let output = project.run(&["secrets", "print"], None);

    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(
        project.invocations(),
        ["secrets download --no-file --format json"]
    );
}

#[test]
fn non_zero_exit_reports_stderr_and_next_step_but_never_stdout() {
    let project = Project::new(SCOPED);
    project.write_fake_doppler(
        "leaked-stdout-value",
        "Doppler Error: you must be logged in",
        1,
    );

    let output = project.run(&["secrets", "print"], None);
    let text = combined(&output);

    assert!(!output.status.success());
    assert!(text.contains("you must be logged in"), "{text}");
    assert!(text.contains("doppler login"), "{text}");
    assert!(text.contains("secrets.project"), "{text}");
    assert!(!text.contains("leaked-stdout-value"), "{text}");
}

#[test]
fn missing_binary_says_to_install_the_cli() {
    let project = Project::new(SCOPED);
    let empty = project.dir.path().join("empty-bin");
    std::fs::create_dir(&empty).expect("create empty bin");

    let output = project.run(&["secrets", "print"], Some(&empty));
    let text = combined(&output);

    assert!(!output.status.success());
    assert!(text.contains("was not found on PATH"), "{text}");
    assert!(text.contains("Install it"), "{text}");
}

#[test]
fn invalid_json_is_actionable_and_hides_output() {
    let project = Project::new(SCOPED);
    project.write_fake_doppler("not-json secret-value", "", 0);

    let output = project.run(&["secrets", "print"], None);
    let text = combined(&output);

    assert!(!output.status.success());
    assert!(text.contains("not valid JSON"), "{text}");
    assert!(!text.contains("secret-value"), "{text}");
}

#[test]
fn non_string_value_names_the_key() {
    let project = Project::new(SCOPED);
    project.write_fake_doppler(r#"{"API_TOKEN":"a","DATABASE_URL":12345}"#, "", 0);

    let output = project.run(&["secrets", "print"], None);
    let text = combined(&output);

    assert!(!output.status.success());
    assert!(text.contains("'DATABASE_URL' is not a string"), "{text}");
    assert!(!text.contains("12345"), "{text}");
}
