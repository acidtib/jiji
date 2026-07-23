use crate::local_exec;

pub struct GitStatus {
    pub short_sha: String,
    pub has_uncommitted_changes: bool,
}

pub fn resolve_version_tag(
    explicit: Option<&str>,
    git: Option<&GitStatus>,
    now_epoch_seconds: u64,
) -> (String, Option<String>) {
    if let Some(explicit) = explicit {
        return (explicit.to_string(), None);
    }
    if let Some(git) = git {
        let warning = git.has_uncommitted_changes.then(|| {
            "The working tree has uncommitted changes; the image tag identifies the current commit, not those changes.".to_string()
        });
        return (git.short_sha.clone(), warning);
    }
    (now_epoch_seconds.to_string(), None)
}

pub fn is_valid_docker_tag(tag: &str) -> bool {
    if tag.is_empty() || tag.len() > 128 {
        return false;
    }
    let mut chars = tag.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

pub fn validate_or_bail(tag: &str) -> anyhow::Result<()> {
    if !is_valid_docker_tag(tag) {
        anyhow::bail!(
            "Version '{tag}' is not a valid container image tag. Use 1-128 letters, digits, periods, underscores, or hyphens, starting with a letter, digit, or underscore."
        );
    }
    Ok(())
}

pub async fn gather_git_status() -> Option<GitStatus> {
    let sha_args = vec!["rev-parse".into(), "--short".into(), "HEAD".into()];
    let sha = local_exec::run_captured("git", &sha_args, None, None)
        .await
        .ok()?;
    if !sha.success || sha.stdout.trim().is_empty() {
        return None;
    }
    let status_args = vec!["status".into(), "--porcelain".into()];
    let status = local_exec::run_captured("git", &status_args, None, None)
        .await
        .ok()?;
    Some(GitStatus {
        short_sha: sha.stdout.trim().to_string(),
        has_uncommitted_changes: status.success && !status.stdout.trim().is_empty(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_and_dirty_warning_are_deterministic() {
        let git = GitStatus {
            short_sha: "abc123".into(),
            has_uncommitted_changes: true,
        };
        assert_eq!(
            resolve_version_tag(Some("1.2.3"), Some(&git), 99),
            ("1.2.3".into(), None)
        );
        let (tag, warning) = resolve_version_tag(None, Some(&git), 99);
        assert_eq!(tag, "abc123");
        assert!(warning.is_some());
        assert_eq!(resolve_version_tag(None, None, 99), ("99".into(), None));
    }

    #[test]
    fn docker_tag_validation_accepts_only_the_supported_grammar() {
        for tag in ["v1", "_local", "1.2-rc_1"] {
            assert!(is_valid_docker_tag(tag), "{tag}");
        }
        for tag in ["", ".bad", "-bad", "has/slash", "has:colon", "has space"] {
            assert!(!is_valid_docker_tag(tag), "{tag}");
        }
        assert!(validate_or_bail("bad/tag")
            .unwrap_err()
            .to_string()
            .contains("bad/tag"));
    }
}
