//! Integration tests for `jiji update`, run as a real subprocess against a real, in-process
//! HTTP/1.1 server standing in for GitHub -- the SSH suite's "roll a real local server, no
//! mock-object framework" convention applied to plain HTTP. `self_update`'s
//! `JIJI_RELEASE_BASE_URL` / `JIJI_RELEASE_API_BASE_URL` env overrides point both the version
//! lookup and the asset download at the same mock.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

#[derive(Clone)]
struct CannedResponse {
    status: u16,
    body: Vec<u8>,
}

fn ok(body: impl Into<Vec<u8>>) -> CannedResponse {
    CannedResponse {
        status: 200,
        body: body.into(),
    }
}

fn not_found() -> CannedResponse {
    CannedResponse {
        status: 404,
        body: b"not found".to_vec(),
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn handle(
    stream: &mut TcpStream,
    routes: &HashMap<String, CannedResponse>,
    received: &Mutex<Vec<String>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();
    // Drain headers until the blank line; every request this mock serves is a bodyless GET.
    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }
    received.lock().unwrap().push(path.clone());

    let response = routes.get(&path).cloned().unwrap_or_else(not_found);
    let status_text = match response.status {
        200 => "200 OK",
        404 => "404 Not Found",
        _ => "500 Internal Server Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status_text}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.body.len()
    )?;
    stream.write_all(&response.body)?;
    stream.flush()?;
    Ok(())
}

/// Spawns a background thread serving `routes` over plain HTTP until the test process exits
/// (each test is a short-lived subprocess run; the listener thread is intentionally never
/// joined). Returns the base URL and the log of every request path received, in order.
fn spawn_mock_server(routes: HashMap<String, CannedResponse>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let addr = listener.local_addr().expect("local addr");
    let routes = Arc::new(routes);
    let received = Arc::new(Mutex::new(Vec::new()));
    let received_for_thread = Arc::clone(&received);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let routes = Arc::clone(&routes);
            let received = Arc::clone(&received_for_thread);
            std::thread::spawn(move || {
                let _ = handle(&mut stream, &routes, &received);
            });
        }
    });
    (format!("http://{addr}"), received)
}

fn expected_asset_name() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "jiji-linux-x86_64",
        ("linux", "aarch64") => "jiji-linux-arm64",
        ("macos", "x86_64") => "jiji-macos-x86_64",
        ("macos", "aarch64") => "jiji-macos-arm64",
        (os, arch) => panic!("update_test.rs needs an asset mapping for {os}/{arch}"),
    }
}

/// A fixture "installed" jiji binary: some placeholder content at a known executable mode, so
/// tests can assert both content and permissions after a run.
fn target_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("jiji");
    std::fs::write(&target, b"placeholder installed binary").expect("write target");
    let mut permissions = std::fs::metadata(&target).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&target, permissions).expect("chmod");
    (dir, target)
}

fn run(base_url: &str, target: &std::path::Path, extra: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jiji"));
    command
        .arg("update")
        .args(extra)
        .env("JIJI_RELEASE_BASE_URL", base_url)
        .env("JIJI_RELEASE_API_BASE_URL", base_url)
        .env("JIJI_UPDATE_TARGET_PATH", target);
    command.output().expect("run jiji update")
}

fn latest_json(tag: &str) -> CannedResponse {
    ok(format!(r#"{{"tag_name":"{tag}"}}"#))
}

#[test]
fn current_version_exits_cleanly_without_downloading_anything() {
    let installed = format!("v{}", env!("CARGO_PKG_VERSION"));
    let routes = HashMap::from([("/releases/latest".to_string(), latest_json(&installed))]);
    let (base_url, received) = spawn_mock_server(routes);
    let (_dir, target) = target_fixture();
    let before = std::fs::read(&target).unwrap();

    let output = run(&base_url, &target, &[]);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("already up to date"));
    assert_eq!(std::fs::read(&target).unwrap(), before);

    let received = received.lock().unwrap();
    assert!(!received
        .iter()
        .any(|path| path.contains("/releases/download/")));
}

#[test]
fn a_newer_release_is_downloaded_verified_and_installed() {
    let asset = expected_asset_name();
    let tag = "v9.9.9";
    let bytes = b"new jiji binary contents".to_vec();
    let checksum = hex_sha256(&bytes);
    let routes = HashMap::from([
        ("/releases/latest".to_string(), latest_json(tag)),
        (
            format!("/releases/download/{tag}/{asset}"),
            ok(bytes.clone()),
        ),
        (
            format!("/releases/download/{tag}/{asset}.sha256"),
            ok(checksum),
        ),
    ]);
    let (base_url, _received) = spawn_mock_server(routes);
    let (_dir, target) = target_fixture();

    let output = run(&base_url, &target, &[]);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(&target).unwrap(), bytes);
    let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755);
}

#[test]
fn a_missing_release_asset_reports_an_actionable_error_and_writes_nothing() {
    let tag = "v9.9.9";
    // No download routes registered at all: both the `.sha256` and the asset itself 404.
    let routes = HashMap::from([("/releases/latest".to_string(), latest_json(tag))]);
    let (base_url, _received) = spawn_mock_server(routes);
    let (_dir, target) = target_fixture();
    let before = std::fs::read(&target).unwrap();

    let output = run(&base_url, &target, &[]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("was not found"), "stderr: {stderr}");
    assert_eq!(std::fs::read(&target).unwrap(), before);
}

#[test]
fn invalid_latest_release_json_reports_a_clear_error_not_a_panic() {
    let routes = HashMap::from([("/releases/latest".to_string(), ok(b"not json".to_vec()))]);
    let (base_url, _received) = spawn_mock_server(routes);
    let (_dir, target) = target_fixture();
    let before = std::fs::read(&target).unwrap();

    let output = run(&base_url, &target, &[]);

    assert!(!output.status.success());
    assert_ne!(
        output.status.code(),
        None,
        "process should exit cleanly, not signal/panic"
    );
    assert_eq!(std::fs::read(&target).unwrap(), before);
}

#[test]
fn a_checksum_mismatch_is_rejected_and_leaves_no_trace() {
    let asset = expected_asset_name();
    let tag = "v9.9.9";
    let bytes = b"new jiji binary contents".to_vec();
    let routes = HashMap::from([
        ("/releases/latest".to_string(), latest_json(tag)),
        (
            format!("/releases/download/{tag}/{asset}"),
            ok(bytes.clone()),
        ),
        (
            format!("/releases/download/{tag}/{asset}.sha256"),
            ok("0000000000000000000000000000000000000000000000000000000000000000".to_string()),
        ),
    ]);
    let (base_url, _received) = spawn_mock_server(routes);
    let (dir, target) = target_fixture();
    let before = std::fs::read(&target).unwrap();

    let output = run(&base_url, &target, &[]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("checksum verification"), "stderr: {stderr}");
    assert_eq!(std::fs::read(&target).unwrap(), before);

    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, vec![std::ffi::OsString::from("jiji")]);
}

#[test]
fn check_flag_never_writes_even_when_a_newer_release_is_available() {
    let asset = expected_asset_name();
    let tag = "v9.9.9";
    let bytes = b"new jiji binary contents".to_vec();
    let checksum = hex_sha256(&bytes);
    let routes = HashMap::from([
        ("/releases/latest".to_string(), latest_json(tag)),
        (
            format!("/releases/download/{tag}/{asset}"),
            ok(bytes.clone()),
        ),
        (
            format!("/releases/download/{tag}/{asset}.sha256"),
            ok(checksum),
        ),
    ]);
    let (base_url, received) = spawn_mock_server(routes);
    let (_dir, target) = target_fixture();
    let before = std::fs::read(&target).unwrap();

    let output = run(&base_url, &target, &["--check"]);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(tag));
    assert_eq!(std::fs::read(&target).unwrap(), before);

    let received = received.lock().unwrap();
    assert!(!received
        .iter()
        .any(|path| path.contains("/releases/download/")));
}

#[test]
fn an_explicit_release_always_proceeds_even_when_older_than_installed() {
    let asset = expected_asset_name();
    let tag = "v0.0.1";
    let bytes = b"rolled back jiji binary".to_vec();
    let checksum = hex_sha256(&bytes);
    let routes = HashMap::from([
        (
            format!("/releases/download/{tag}/{asset}"),
            ok(bytes.clone()),
        ),
        (
            format!("/releases/download/{tag}/{asset}.sha256"),
            ok(checksum),
        ),
    ]);
    let (base_url, received) = spawn_mock_server(routes);
    let (_dir, target) = target_fixture();

    let output = run(&base_url, &target, &["--release", tag]);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(&target).unwrap(), bytes);

    // An explicit --release never calls the "latest" API endpoint at all.
    let received = received.lock().unwrap();
    assert!(!received.iter().any(|path| path == "/releases/latest"));
}
