use std::path::{Component, Path};

use anyhow::Context;
use jiji_config::{MountConfig, Service};
use jiji_ssh::{CommandResult, SshSession};

use crate::container_runtime::render_volumes;

pub struct ParsedMount {
    pub local: String,
    pub remote: String,
    pub mode: Option<String>,
    pub owner: Option<String>,
    pub options: Option<String>,
}

pub fn parse_mount(mount: &MountConfig) -> anyhow::Result<ParsedMount> {
    match mount {
        MountConfig::Str(value) => {
            let parts: Vec<&str> = value.splitn(3, ':').collect();
            if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
                anyhow::bail!(
                    "Mount '{value}' must be in 'local:remote' or 'local:remote:options' format"
                );
            }
            Ok(ParsedMount {
                local: parts[0].to_string(),
                remote: parts[1].to_string(),
                mode: None,
                owner: None,
                options: parts.get(2).map(|options| options.to_string()),
            })
        }
        MountConfig::Detailed {
            local,
            remote,
            mode,
            owner,
            options,
        } => Ok(ParsedMount {
            local: local.clone(),
            remote: remote.clone(),
            mode: mode.clone(),
            owner: owner.clone(),
            options: options.clone(),
        }),
    }
}

pub fn remote_mount_base(project: &str, kind: &str, service: &str) -> String {
    format!(".jiji/{project}/{kind}/{service}")
}

pub fn reject_path_traversal(local: &str) -> anyhow::Result<()> {
    if Path::new(local)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        anyhow::bail!("Mount local path '{local}' may not contain '..' path segments");
    }
    Ok(())
}

pub fn build_mount_args(
    mounts: &[MountConfig],
    project: &str,
    kind: &str,
    service: &str,
) -> anyhow::Result<Vec<String>> {
    let base = remote_mount_base(project, kind, service);
    let mut args = Vec::new();
    for mount in mounts {
        let parsed = parse_mount(mount)?;
        reject_path_traversal(&parsed.local)?;
        let mut mount_arg = format!("{base}/{}:{}", parsed.local, parsed.remote);
        if let Some(options) = &parsed.options {
            mount_arg.push(':');
            mount_arg.push_str(options);
        }
        args.push("-v".to_string());
        args.push(mount_arg);
    }
    Ok(args)
}

/// Files, then directories, then named/bind volumes -- a single ordered list of `-v` flags.
pub fn build_all_mount_args(
    service: &Service,
    project: &str,
    service_name: &str,
) -> anyhow::Result<Vec<String>> {
    let mut args = build_mount_args(&service.files, project, "files", service_name)?;
    args.extend(build_mount_args(
        &service.directories,
        project,
        "directories",
        service_name,
    )?);
    args.extend(render_volumes(&service.volumes, service_name));
    Ok(args)
}

pub async fn upload_file(
    session: &SshSession,
    local_path: &Path,
    remote_path: &str,
    mode: Option<&str>,
    owner: Option<&str>,
) -> anyhow::Result<()> {
    let content = tokio::fs::read(local_path)
        .await
        .with_context(|| format!("Could not read local file {}", local_path.display()))?;
    let mode = mode.unwrap_or("0644");
    let temp = format!("{remote_path}.jiji-new");
    let command =
        format!("set -eu; install -D -m {mode} /dev/stdin {temp}; mv {temp} {remote_path}");
    let result = session.execute_with_input(&command, &content).await?;
    ensure_success(session, &command, &result)?;

    if let Some(owner) = owner {
        let command = format!("chown {owner} {remote_path}");
        let result = session.execute(&command).await?;
        ensure_success(session, &command, &result)?;
    }
    Ok(())
}

/// A missing local directory uploads nothing and just creates an empty remote directory (matches
/// the original tool's fallback). An existing directory is archived in memory with `tar` and
/// piped over stdin -- there is no SFTP support in `jiji-ssh` yet, and this handles nested
/// subdirectories/permissions correctly, unlike a flat-files-only restriction. A size cap guards
/// against a mis-scoped `directories:` entry streaming an entire filesystem through one SSH exec.
pub async fn upload_directory(
    session: &SshSession,
    local_dir: &Path,
    remote_dir: &str,
    mode: Option<&str>,
    owner: Option<&str>,
    max_bytes: u64,
) -> anyhow::Result<()> {
    if !local_dir.exists() {
        let command = format!("mkdir -p {remote_dir}");
        let result = session.execute(&command).await?;
        return ensure_success(session, &command, &result);
    }

    let owned_dir = local_dir.to_path_buf();
    let bytes = tokio::task::spawn_blocking(move || build_tar_archive(&owned_dir, max_bytes))
        .await
        .context("tar archive task panicked")??;

    let command = format!("set -eu; mkdir -p {remote_dir}; tar -C {remote_dir} -xf -");
    let result = session.execute_with_input(&command, &bytes).await?;
    ensure_success(session, &command, &result)?;

    if let Some(mode) = mode {
        let command = format!("chmod -R {mode} {remote_dir}");
        let result = session.execute(&command).await?;
        ensure_success(session, &command, &result)?;
    }
    if let Some(owner) = owner {
        let command = format!("chown -R {owner} {remote_dir}");
        let result = session.execute(&command).await?;
        ensure_success(session, &command, &result)?;
    }
    Ok(())
}

fn build_tar_archive(local_dir: &Path, max_bytes: u64) -> anyhow::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    builder
        .append_dir_all(".", local_dir)
        .with_context(|| format!("Could not archive directory {}", local_dir.display()))?;
    let bytes = builder
        .into_inner()
        .context("Could not finalize tar archive")?;
    if bytes.len() as u64 > max_bytes {
        anyhow::bail!(
            "Directory '{}' archives to {} bytes, exceeding the {max_bytes}-byte upload limit. Reduce its contents or raise the configured limit.",
            local_dir.display(),
            bytes.len()
        );
    }
    Ok(bytes)
}

/// Uploads every `files`/`directories` entry for `service`, then returns the complete ordered
/// mount-flag list (files, directories, volumes) for the container run command.
pub async fn prepare_mounts(
    session: &SshSession,
    service: &Service,
    service_name: &str,
    project: &str,
    project_root: &Path,
    max_dir_bytes: u64,
) -> anyhow::Result<Vec<String>> {
    for mount in &service.files {
        let parsed = parse_mount(mount)?;
        reject_path_traversal(&parsed.local)?;
        let local_path = project_root.join(&parsed.local);
        let remote_path = format!(
            "{}/{}",
            remote_mount_base(project, "files", service_name),
            parsed.local
        );
        upload_file(
            session,
            &local_path,
            &remote_path,
            parsed.mode.as_deref(),
            parsed.owner.as_deref(),
        )
        .await?;
    }

    for mount in &service.directories {
        let parsed = parse_mount(mount)?;
        reject_path_traversal(&parsed.local)?;
        let local_path = project_root.join(&parsed.local);
        let remote_path = format!(
            "{}/{}",
            remote_mount_base(project, "directories", service_name),
            parsed.local
        );
        upload_directory(
            session,
            &local_path,
            &remote_path,
            parsed.mode.as_deref(),
            parsed.owner.as_deref(),
            max_dir_bytes,
        )
        .await?;
    }

    build_all_mount_args(service, project, service_name)
}

fn ensure_success(
    session: &SshSession,
    command: &str,
    result: &CommandResult,
) -> anyhow::Result<()> {
    if result.success {
        return Ok(());
    }
    anyhow::bail!(
        "Command `{command}` failed on {} (exit {:?}): {}",
        session.host(),
        result.code,
        result.stderr.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_form_mount_parses_local_remote_and_options() {
        let mount = MountConfig::Str("nginx.conf:/etc/nginx/nginx.conf:ro".to_string());
        let parsed = parse_mount(&mount).unwrap();
        assert_eq!(parsed.local, "nginx.conf");
        assert_eq!(parsed.remote, "/etc/nginx/nginx.conf");
        assert_eq!(parsed.options.as_deref(), Some("ro"));
        assert_eq!(parsed.mode, None);
    }

    #[test]
    fn detailed_form_mount_keeps_mode_and_owner() {
        let mount = MountConfig::Detailed {
            local: "secret.key".to_string(),
            remote: "/etc/app/secret.key".to_string(),
            mode: Some("0600".to_string()),
            owner: Some("nginx:nginx".to_string()),
            options: Some("ro".to_string()),
        };
        let parsed = parse_mount(&mount).unwrap();
        assert_eq!(parsed.mode.as_deref(), Some("0600"));
        assert_eq!(parsed.owner.as_deref(), Some("nginx:nginx"));
    }

    #[test]
    fn path_traversal_is_rejected() {
        assert!(reject_path_traversal("../etc/passwd").is_err());
        assert!(reject_path_traversal("nested/../../etc").is_err());
        assert!(reject_path_traversal("nested/ok").is_ok());
    }

    #[test]
    fn mount_args_land_under_the_project_service_scoped_base() {
        let mounts = vec![MountConfig::Str(
            "nginx.conf:/etc/nginx/nginx.conf:ro".to_string(),
        )];
        let args = build_mount_args(&mounts, "demo", "files", "web").unwrap();
        assert_eq!(
            args,
            vec![
                "-v".to_string(),
                ".jiji/demo/files/web/nginx.conf:/etc/nginx/nginx.conf:ro".to_string()
            ]
        );
    }

    #[test]
    fn all_mount_args_order_files_then_directories_then_volumes() {
        let service: Service = serde_yaml::from_str(
            r#"
image: example/web
servers: [app]
files: ["a.conf:/a.conf"]
directories: ["confd:/etc/confd"]
volumes: ["data:/data"]
"#,
        )
        .unwrap();
        let args = build_all_mount_args(&service, "demo", "web").unwrap();
        let joined = args.join(" ");
        let files_pos = joined.find("files/web/a.conf").unwrap();
        let dirs_pos = joined.find("directories/web/confd").unwrap();
        let volume_pos = joined.find("web-data:/data").unwrap();
        assert!(files_pos < dirs_pos);
        assert!(dirs_pos < volume_pos);
    }
}
