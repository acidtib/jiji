use jiji_config::ContainerEngine;
use jiji_ssh::SshSession;

const DOCKER_MIN_VERSION: (u64, u64, u64) = (29, 3, 0);
const PODMAN_MIN_VERSION: (u64, u64, u64) = (4, 9, 3);

pub enum EngineStatus {
    AlreadyInstalled(String),
    Installed(String),
}

struct OsInfo {
    id: String,
    version_id: String,
    version_codename: String,
}

/// Checks whether `engine` is installed on the remote host and, if not, installs it. Refuses to
/// proceed (rather than silently continuing) if an already-installed engine is below the
/// minimum supported version — matches the POC's behavior of never auto-upgrading in place.
pub async fn ensure_engine(
    session: &SshSession,
    engine: ContainerEngine,
) -> anyhow::Result<EngineStatus> {
    let name = engine.to_string();

    if is_installed(session, &name).await? {
        let version = version_of(session, &name).await?;
        check_min_version(&name, &version, min_version(engine))?;
        return Ok(EngineStatus::AlreadyInstalled(version));
    }

    install(session, engine).await?;

    let version = version_of(session, &name).await?;
    check_min_version(&name, &version, min_version(engine))?;
    Ok(EngineStatus::Installed(version))
}

/// Verifies `engine` is already installed and meets the minimum supported version on `session`,
/// without installing or upgrading anything -- jiji does not provision or upgrade a remote
/// builder host (see `plans/remote-builders.md` non-goals), unlike `ensure_engine`, which is
/// only ever used for a `jiji server setup`-managed deployment host.
pub(crate) async fn ensure_min_version(
    session: &SshSession,
    engine: ContainerEngine,
) -> anyhow::Result<String> {
    let name = engine.to_string();
    if !is_installed(session, &name).await? {
        anyhow::bail!(
            "{name} is not installed on {}. jiji does not provision remote builder hosts; install {name} there and retry.",
            session.host()
        );
    }
    let version = version_of(session, &name).await?;
    check_min_version(&name, &version, min_version(engine))?;
    Ok(version)
}

fn min_version(engine: ContainerEngine) -> (u64, u64, u64) {
    match engine {
        ContainerEngine::Docker => DOCKER_MIN_VERSION,
        ContainerEngine::Podman => PODMAN_MIN_VERSION,
    }
}

async fn is_installed(session: &SshSession, engine_name: &str) -> anyhow::Result<bool> {
    let result = session.execute(&format!("which {engine_name}")).await?;
    Ok(result.success)
}

async fn version_of(session: &SshSession, engine_name: &str) -> anyhow::Result<String> {
    let result = session.execute(&format!("{engine_name} --version")).await?;
    if !result.success {
        anyhow::bail!(
            "Could not read {engine_name} version on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(result.stdout.trim().to_string())
}

fn check_min_version(
    engine_name: &str,
    version_output: &str,
    min: (u64, u64, u64),
) -> anyhow::Result<()> {
    let Some(found) = parse_version(version_output) else {
        // Some distro packages don't print a parseable semver (e.g. patched builds); don't block
        // provisioning over an unparseable version string.
        return Ok(());
    };
    if found < min {
        anyhow::bail!(
            "{engine_name} {}.{}.{} is installed, but jiji requires at least {}.{}.{}. Please upgrade {engine_name} on this host and try again.",
            found.0,
            found.1,
            found.2,
            min.0,
            min.1,
            min.2
        );
    }
    Ok(())
}

fn parse_version(output: &str) -> Option<(u64, u64, u64)> {
    let digits_and_dots: String = output
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = digits_and_dots.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

async fn install(session: &SshSession, engine: ContainerEngine) -> anyhow::Result<()> {
    let os = detect_os(session).await?;
    let commands = match engine {
        ContainerEngine::Docker => docker_install_commands(&os)?,
        ContainerEngine::Podman => podman_install_commands(&os)?,
    };

    for command in &commands {
        let result = session.execute(command).await?;
        if !result.success {
            anyhow::bail!(
                "Command `{command}` failed on {} (exit {:?}): {}",
                session.host(),
                result.code,
                result.stderr.trim()
            );
        }
    }

    Ok(())
}

async fn detect_os(session: &SshSession) -> anyhow::Result<OsInfo> {
    let result = session.execute("cat /etc/os-release").await?;
    if !result.success {
        anyhow::bail!(
            "Could not read /etc/os-release on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }

    let mut id = String::new();
    let mut version_id = String::new();
    let mut version_codename = String::new();

    for line in result.stdout.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_string();
        match key {
            "ID" => id = value,
            "VERSION_ID" => version_id = value,
            "VERSION_CODENAME" => version_codename = value,
            _ => {}
        }
    }

    if id.is_empty() {
        anyhow::bail!(
            "Could not determine the OS distribution on {} from /etc/os-release",
            session.host()
        );
    }

    Ok(OsInfo {
        id,
        version_id,
        version_codename,
    })
}

fn docker_install_commands(os: &OsInfo) -> anyhow::Result<Vec<String>> {
    match os.id.as_str() {
        "ubuntu" | "debian" => {
            let distro = os.id.clone();
            let codename = os.version_codename.clone();
            Ok(vec![
                "export DEBIAN_FRONTEND=noninteractive".to_string(),
                "apt-get update -qq".to_string(),
                "apt-get install -y -qq ca-certificates curl gnupg".to_string(),
                "install -m 0755 -d /etc/apt/keyrings".to_string(),
                format!(
                    "curl -fsSL https://download.docker.com/linux/{distro}/gpg -o /etc/apt/keyrings/docker.asc"
                ),
                "chmod a+r /etc/apt/keyrings/docker.asc".to_string(),
                format!(
                    "echo \"deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/{distro} {codename} stable\" | tee /etc/apt/sources.list.d/docker.list > /dev/null"
                ),
                "apt-get update -qq".to_string(),
                "apt-get install -y -qq docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin curl git".to_string(),
                "systemctl start docker".to_string(),
                "systemctl enable docker".to_string(),
            ])
        }
        "fedora" => Ok(vec![
            "dnf -y install dnf-plugins-core".to_string(),
            "dnf config-manager --add-repo https://download.docker.com/linux/fedora/docker-ce.repo".to_string(),
            "dnf install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin curl git".to_string(),
            "systemctl start docker".to_string(),
            "systemctl enable docker".to_string(),
        ]),
        "centos" | "rhel" => Ok(vec![
            "yum install -y yum-utils".to_string(),
            "yum-config-manager --add-repo https://download.docker.com/linux/centos/docker-ce.repo".to_string(),
            "yum install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin curl git".to_string(),
            "systemctl start docker".to_string(),
            "systemctl enable docker".to_string(),
        ]),
        other => anyhow::bail!(
            "Don't know how to install docker on unsupported OS '{other}' (version {}). Please install docker manually.",
            os.version_id
        ),
    }
}

fn podman_install_commands(os: &OsInfo) -> anyhow::Result<Vec<String>> {
    let policy_cmd = "mkdir -p /etc/containers && test -f /etc/containers/policy.json || echo '{\"default\":[{\"type\":\"insecureAcceptAnything\"}]}' | tee /etc/containers/policy.json > /dev/null".to_string();

    match os.id.as_str() {
        "ubuntu" | "debian" => Ok(vec![
            "export DEBIAN_FRONTEND=noninteractive".to_string(),
            "apt-get update -qq".to_string(),
            "apt-get install -y -qq podman curl git".to_string(),
            policy_cmd,
        ]),
        "fedora" | "centos" | "rhel" => Ok(vec![
            "dnf install -y podman curl git".to_string(),
            policy_cmd,
        ]),
        other => anyhow::bail!(
            "Don't know how to install podman on unsupported OS '{other}' (version {}). Please install podman manually.",
            os.version_id
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_version;

    #[test]
    fn parses_docker_version_output() {
        assert_eq!(
            parse_version("Docker version 28.2.0, build abcdef"),
            Some((28, 2, 0))
        );
    }

    #[test]
    fn parses_podman_version_output() {
        assert_eq!(parse_version("podman version 4.9.3"), Some((4, 9, 3)));
    }

    #[test]
    fn returns_none_for_unparseable_output() {
        assert_eq!(parse_version("not a version"), None);
    }
}
