//! Packages a service's local build context into an in-memory tar archive for upload to a
//! remote builder. Local builds hand the engine a bare path and let it read the filesystem (and
//! apply its own ignore-file semantics) directly; a remote build can only stage what it
//! packages, so this module reimplements `.dockerignore`/`.containerignore` filtering,
//! executable-bit and symlink preservation, and archive-traversal rejection.

use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};

use anyhow::Context;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use jiji_config::ContainerEngine;

use crate::build_engine::ResolvedBuildConfig;

#[derive(Debug)]
pub struct ContextPackage {
    pub archive: Vec<u8>,
    /// POSIX-relative path of the Dockerfile within the archive (i.e. within the context root).
    pub dockerfile_rel: String,
}

/// Resolves and validates `context`/`dockerfile` against `project_root`, requiring the
/// Dockerfile to live inside the context. Local builds may reference a Dockerfile outside the
/// context (the local engine reads the filesystem directly); a remote build can only stage what
/// it packages, so that combination is rejected here instead of silently omitting the file.
fn resolve_remote_context(
    project_root: &Path,
    build: &ResolvedBuildConfig,
) -> anyhow::Result<(PathBuf, String)> {
    let context_root = fs::canonicalize(project_root.join(&build.context)).with_context(|| {
        format!(
            "Build context '{}' does not exist under {}",
            build.context,
            project_root.display()
        )
    })?;
    let dockerfile_path =
        fs::canonicalize(project_root.join(&build.dockerfile)).with_context(|| {
            format!(
                "Dockerfile '{}' does not exist under {}",
                build.dockerfile,
                project_root.display()
            )
        })?;
    let dockerfile_rel = dockerfile_path.strip_prefix(&context_root).map_err(|_| {
        anyhow::anyhow!(
            "Dockerfile '{}' is outside the build context '{}'. Remote builds can only stage the configured context; move the Dockerfile inside it or point `context:` at a directory that contains it.",
            build.dockerfile,
            build.context
        )
    })?;
    Ok((context_root, to_posix(dockerfile_rel)))
}

/// Docker always uses `.dockerignore`. Podman prefers `.containerignore` when present, falling
/// back to `.dockerignore` -- this is precedence, not a union of both files, matching real
/// Podman behavior.
fn load_ignore_rules(context_root: &Path, engine: ContainerEngine) -> anyhow::Result<Gitignore> {
    let dockerignore = context_root.join(".dockerignore");
    let containerignore = context_root.join(".containerignore");
    let ignore_file = match engine {
        ContainerEngine::Podman if containerignore.is_file() => Some(containerignore),
        _ if dockerignore.is_file() => Some(dockerignore),
        _ => None,
    };

    let mut builder = GitignoreBuilder::new(context_root);
    if let Some(path) = &ignore_file {
        if let Some(error) = builder.add(path) {
            return Err(error)
                .with_context(|| format!("Could not parse ignore file {}", path.display()));
        }
    }
    builder.build().with_context(|| {
        format!(
            "Could not build ignore matcher for {}",
            context_root.display()
        )
    })
}

/// Synchronous; the caller wraps this in `spawn_blocking` (mirrors
/// `mounts.rs::build_tar_archive`).
pub fn package_context(
    project_root: &Path,
    build: &ResolvedBuildConfig,
    engine: ContainerEngine,
    max_bytes: u64,
) -> anyhow::Result<ContextPackage> {
    let (context_root, dockerfile_rel) = resolve_remote_context(project_root, build)?;
    let matcher = load_ignore_rules(&context_root, engine)?;

    let mut builder = tar::Builder::new(Vec::new());
    builder.follow_symlinks(false);
    walk_dir(
        &context_root,
        &context_root,
        &dockerfile_rel,
        &matcher,
        &mut builder,
    )?;
    let archive = builder
        .into_inner()
        .context("Could not finalize build context archive")?;

    if archive.len() as u64 > max_bytes {
        anyhow::bail!(
            "Build context '{}' archives to {} bytes, exceeding the {max_bytes}-byte upload limit. Reduce its contents (see `.dockerignore`) or raise the configured limit.",
            build.context,
            archive.len()
        );
    }

    Ok(ContextPackage {
        archive,
        dockerfile_rel,
    })
}

fn walk_dir(
    context_root: &Path,
    dir: &Path,
    dockerfile_rel: &str,
    matcher: &Gitignore,
    builder: &mut tar::Builder<Vec<u8>>,
) -> anyhow::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("Could not read directory {}", dir.display()))?
        .collect::<Result<_, _>>()
        .with_context(|| format!("Could not read directory {}", dir.display()))?;
    entries.sort_by_key(fs::DirEntry::file_name);

    for entry in entries {
        let path = entry.path();
        let rel = path
            .strip_prefix(context_root)
            .expect("walk only ever descends under context_root")
            .to_path_buf();
        let rel_str = to_posix(&rel);
        let is_dockerfile = rel_str == dockerfile_rel;
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("Could not read metadata for {}", path.display()))?;

        if metadata.file_type().is_symlink() {
            if !is_dockerfile && matcher.matched_path_or_any_parents(&rel, false).is_ignore() {
                continue;
            }
            add_symlink_entry(builder, &path, &rel)?;
            continue;
        }

        if metadata.is_dir() {
            let ignored = matcher.matched_path_or_any_parents(&rel, true).is_ignore();
            let dockerfile_inside = dockerfile_rel.starts_with(&format!("{rel_str}/"));
            if ignored && !dockerfile_inside {
                continue;
            }
            walk_dir(context_root, &path, dockerfile_rel, matcher, builder)?;
            continue;
        }

        if !is_dockerfile && matcher.matched_path_or_any_parents(&rel, false).is_ignore() {
            continue;
        }
        let mut file =
            File::open(&path).with_context(|| format!("Could not read file {}", path.display()))?;
        builder
            .append_file(&rel, &mut file)
            .with_context(|| format!("Could not archive file {}", path.display()))?;
    }
    Ok(())
}

fn add_symlink_entry(
    builder: &mut tar::Builder<Vec<u8>>,
    path: &Path,
    rel: &Path,
) -> anyhow::Result<()> {
    let target = fs::read_link(path)
        .with_context(|| format!("Could not read symlink {}", path.display()))?;
    if target.is_absolute() {
        anyhow::bail!(
            "Symlink '{}' points to an absolute path ('{}'), which cannot be safely staged for a remote build. Use a relative symlink inside the context or exclude it via `.dockerignore`.",
            rel.display(),
            target.display()
        );
    }
    let parent = rel.parent().unwrap_or_else(|| Path::new(""));
    let resolved = lexically_normalize(&parent.join(&target));
    if matches!(resolved.components().next(), Some(Component::ParentDir)) {
        anyhow::bail!(
            "Symlink '{}' points outside the build context ('{}'). Remote builds cannot follow links leaving the context; remove it or exclude it via `.dockerignore`.",
            rel.display(),
            target.display()
        );
    }

    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_mode(0o777);
    builder
        .append_link(&mut header, rel, &target)
        .with_context(|| format!("Could not archive symlink {}", rel.display()))
}

/// Resolves `.`/`..` components without touching the filesystem (the target of a symlink may not
/// exist, e.g. a broken link or one pointing at a file created later in the build). A leading
/// `..` in the result means the path escapes whatever root it was joined against.
fn lexically_normalize(path: &Path) -> PathBuf {
    let mut parts: Vec<OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::ParentDir => match parts.last() {
                Some(last) if last != ".." => {
                    parts.pop();
                }
                _ => parts.push("..".into()),
            },
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::Normal(part) => parts.push(part.to_os_string()),
        }
    }
    parts.into_iter().collect()
}

fn to_posix(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn build(context: &str, dockerfile: &str) -> ResolvedBuildConfig {
        ResolvedBuildConfig {
            context: context.to_string(),
            dockerfile: dockerfile.to_string(),
            args: BTreeMap::new(),
            target: None,
            secrets: Vec::new(),
        }
    }

    fn archive_entries(archive: &[u8]) -> Vec<(String, tar::EntryType)> {
        let mut ar = tar::Archive::new(archive);
        ar.entries()
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.path().unwrap().to_string_lossy().to_string(),
                    entry.header().entry_type(),
                )
            })
            .collect()
    }

    #[test]
    fn packages_a_plain_context() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(dir.path().join("app.txt"), "hi").unwrap();

        let package = package_context(
            dir.path(),
            &build(".", "Dockerfile"),
            ContainerEngine::Docker,
            1024 * 1024,
        )
        .unwrap();
        assert_eq!(package.dockerfile_rel, "Dockerfile");
        let entries = archive_entries(&package.archive);
        assert!(entries.iter().any(|(p, _)| p == "Dockerfile"));
        assert!(entries.iter().any(|(p, _)| p == "app.txt"));
    }

    #[test]
    fn packages_a_subdirectory_context_with_a_bare_dockerfile_name() {
        // Mirrors resolve_build_config's context-relative resolution: `context: ./api` +
        // `dockerfile: Dockerfile` (no `api/` prefix needed) must still find api/Dockerfile.
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("api")).unwrap();
        fs::write(dir.path().join("api/Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(dir.path().join("api/app.txt"), "hi").unwrap();

        let build = crate::build_engine::resolve_build_config(&jiji_config::BuildValue::Detailed(
            jiji_config::BuildConfig {
                context: "./api".into(),
                dockerfile: Some("Dockerfile".into()),
                args: None,
                target: None,
                secrets: None,
            },
        ));
        assert_eq!(build.dockerfile, "api/Dockerfile");

        let package =
            package_context(dir.path(), &build, ContainerEngine::Docker, 1024 * 1024).unwrap();
        assert_eq!(package.dockerfile_rel, "Dockerfile");
        let entries = archive_entries(&package.archive);
        assert!(entries.iter().any(|(p, _)| p == "Dockerfile"));
        assert!(entries.iter().any(|(p, _)| p == "app.txt"));
    }

    #[test]
    fn packages_a_project_root_context_defaulted_from_config() {
        // Proves the schema-level default (BuildConfig.context omitted -> ".") reaches the
        // remote packaging path, not just a hand-constructed "." in a test fixture.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(dir.path().join("app.txt"), "hi").unwrap();

        let yaml = r#"
project: demo
builder:
  engine: podman
servers:
  web:
    host: 10.0.0.1
services:
  app:
    image: nginx:latest
    servers: [web]
    build:
      dockerfile: Dockerfile
"#;
        let config: jiji_config::Config =
            serde_yaml::from_str(yaml).expect("config with context omitted should parse");
        let build_value = config.services["app"]
            .build
            .as_ref()
            .expect("build config present");
        let build = crate::build_engine::resolve_build_config(build_value);
        assert_eq!(build.context, ".");

        let package =
            package_context(dir.path(), &build, ContainerEngine::Docker, 1024 * 1024).unwrap();
        assert_eq!(package.dockerfile_rel, "Dockerfile");
        let entries = archive_entries(&package.archive);
        assert!(entries.iter().any(|(p, _)| p == "Dockerfile"));
        assert!(entries.iter().any(|(p, _)| p == "app.txt"));
    }

    #[test]
    fn dockerignore_excludes_matching_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(dir.path().join("secret.env"), "SECRET=1").unwrap();
        fs::write(dir.path().join(".dockerignore"), "secret.env\n").unwrap();

        let package = package_context(
            dir.path(),
            &build(".", "Dockerfile"),
            ContainerEngine::Docker,
            1024 * 1024,
        )
        .unwrap();
        let entries = archive_entries(&package.archive);
        assert!(!entries.iter().any(|(p, _)| p == "secret.env"));
        assert!(entries.iter().any(|(p, _)| p == "Dockerfile"));
    }

    #[test]
    fn containerignore_takes_precedence_over_dockerignore_for_podman() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(dir.path().join("only-dockerignore.txt"), "x").unwrap();
        fs::write(dir.path().join("only-containerignore.txt"), "x").unwrap();
        fs::write(dir.path().join(".dockerignore"), "only-dockerignore.txt\n").unwrap();
        fs::write(
            dir.path().join(".containerignore"),
            "only-containerignore.txt\n",
        )
        .unwrap();

        let package = package_context(
            dir.path(),
            &build(".", "Dockerfile"),
            ContainerEngine::Podman,
            1024 * 1024,
        )
        .unwrap();
        let entries = archive_entries(&package.archive);
        // .containerignore wins: the file it excludes is gone, but the file *dockerignore*
        // would have excluded is still present because dockerignore was not consulted at all.
        assert!(!entries.iter().any(|(p, _)| p == "only-containerignore.txt"));
        assert!(entries.iter().any(|(p, _)| p == "only-dockerignore.txt"));
    }

    #[test]
    fn negation_reincludes_a_file_under_an_excluded_pattern() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(dir.path().join("keep.log"), "x").unwrap();
        fs::write(dir.path().join("drop.log"), "x").unwrap();
        fs::write(dir.path().join(".dockerignore"), "*.log\n!keep.log\n").unwrap();

        let package = package_context(
            dir.path(),
            &build(".", "Dockerfile"),
            ContainerEngine::Docker,
            1024 * 1024,
        )
        .unwrap();
        let entries = archive_entries(&package.archive);
        assert!(entries.iter().any(|(p, _)| p == "keep.log"));
        assert!(!entries.iter().any(|(p, _)| p == "drop.log"));
    }

    #[test]
    fn executable_bit_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        let script = dir.path().join("run.sh");
        fs::write(&script, "#!/bin/sh\n").unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();

        let package = package_context(
            dir.path(),
            &build(".", "Dockerfile"),
            ContainerEngine::Docker,
            1024 * 1024,
        )
        .unwrap();
        let mut ar = tar::Archive::new(package.archive.as_slice());
        let entry = ar
            .entries()
            .unwrap()
            .map(Result::unwrap)
            .find(|e| e.path().unwrap().to_string_lossy() == "run.sh")
            .expect("run.sh present");
        assert_eq!(entry.header().mode().unwrap() & 0o111, 0o111);
    }

    #[test]
    fn in_context_symlink_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(dir.path().join("real.txt"), "x").unwrap();
        symlink("real.txt", dir.path().join("link.txt")).unwrap();

        let package = package_context(
            dir.path(),
            &build(".", "Dockerfile"),
            ContainerEngine::Docker,
            1024 * 1024,
        )
        .unwrap();
        let entries = archive_entries(&package.archive);
        assert!(entries
            .iter()
            .any(|(p, ty)| p == "link.txt" && *ty == tar::EntryType::Symlink));
    }

    #[test]
    fn out_of_context_symlink_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        symlink("../outside.txt", dir.path().join("escape.txt")).unwrap();

        let error = package_context(
            dir.path(),
            &build(".", "Dockerfile"),
            ContainerEngine::Docker,
            1024 * 1024,
        )
        .unwrap_err();
        assert!(error.to_string().contains("outside the build context"));
    }

    #[test]
    fn absolute_symlink_target_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        symlink("/etc/passwd", dir.path().join("escape.txt")).unwrap();

        let error = package_context(
            dir.path(),
            &build(".", "Dockerfile"),
            ContainerEngine::Docker,
            1024 * 1024,
        )
        .unwrap_err();
        assert!(error.to_string().contains("absolute path"));
    }

    #[test]
    fn dockerfile_is_included_even_when_ignored() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(dir.path().join(".dockerignore"), "Dockerfile\n").unwrap();

        let package = package_context(
            dir.path(),
            &build(".", "Dockerfile"),
            ContainerEngine::Docker,
            1024 * 1024,
        )
        .unwrap();
        let entries = archive_entries(&package.archive);
        assert!(entries.iter().any(|(p, _)| p == "Dockerfile"));
    }

    #[test]
    fn dockerfile_inside_an_ignored_directory_is_still_included() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("docker")).unwrap();
        fs::write(dir.path().join("docker/Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(dir.path().join("docker/other.txt"), "x").unwrap();
        fs::write(dir.path().join(".dockerignore"), "docker/\n").unwrap();

        let package = package_context(
            dir.path(),
            &build(".", "docker/Dockerfile"),
            ContainerEngine::Docker,
            1024 * 1024,
        )
        .unwrap();
        let entries = archive_entries(&package.archive);
        assert!(entries.iter().any(|(p, _)| p == "docker/Dockerfile"));
        // The forced inclusion only applies to the Dockerfile itself, not its ignored siblings.
        assert!(!entries.iter().any(|(p, _)| p == "docker/other.txt"));
    }

    #[test]
    fn max_bytes_cap_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        fs::write(dir.path().join("big.bin"), vec![0u8; 4096]).unwrap();

        let error = package_context(
            dir.path(),
            &build(".", "Dockerfile"),
            ContainerEngine::Docker,
            128,
        )
        .unwrap_err();
        assert!(error.to_string().contains("exceeding"));
    }

    #[test]
    fn dockerfile_outside_context_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("ctx")).unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();

        let error = package_context(
            dir.path(),
            &build("ctx", "Dockerfile"),
            ContainerEngine::Docker,
            1024 * 1024,
        )
        .unwrap_err();
        assert!(error.to_string().contains("outside the build context"));
    }
}
