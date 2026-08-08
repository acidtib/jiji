use jiji_config::ContainerEngine;
use jiji_ssh::SshSession;

const DOCKER_MIN_VERSION: (u64, u64, u64) = (29, 3, 0);
const PODMAN_MIN_VERSION: (u64, u64, u64) = (5, 8, 4);
const PODMAN_STATIC_VERSION: &str = "5.8.4";
const PODMAN_STATIC_AMD64_SHA256: &str =
    "a58765fe8be6ab3fb79f892f1a027b4ce4a7e8eb589df1ef960c167cbde08d69";
const PODMAN_STATIC_ARM64_SHA256: &str =
    "a2f6b73cc0f7018e2e8518338a4ec27db70148e1af86e16719235605aefd1df3";

pub enum EngineStatus {
    AlreadyInstalled(String),
    Installed(String),
    Upgraded { from: String, to: String },
}

struct OsInfo {
    id: String,
    version_id: String,
    version_codename: String,
}

/// Checks whether `engine` is installed on the remote host and, if not, installs it. Podman is
/// upgraded in place when its distro package is too old because supported Debian and Ubuntu
/// releases do not package a version new enough for current CDI specifications. Docker still
/// requires an operator-managed upgrade when an installed version is too old. Used both for a
/// `jiji server setup`-managed deployment host and a `builder.remote` host (see
/// `remote_build.rs::preflight`) -- jiji provisions the engine on either, but never anything else
/// on a builder host (no network/proxy setup, no Buildx/`podman manifest` installation).
pub async fn ensure_engine(
    session: &SshSession,
    engine: ContainerEngine,
) -> anyhow::Result<EngineStatus> {
    let name = engine.to_string();
    let mut previous_version = None;

    if is_installed(session, &name).await? {
        let version = version_of(session, &name).await?;
        if version_is_supported(&version, min_version(engine)) {
            if engine == ContainerEngine::Podman {
                reconcile_managed_podman_static_configuration(session).await?;
            }
            return Ok(EngineStatus::AlreadyInstalled(version));
        }
        if engine != ContainerEngine::Podman {
            check_min_version(&name, &version, min_version(engine))?;
        }
        previous_version = Some(version);
    }

    install(session, engine).await?;

    let version = version_of(session, &name).await?;
    check_min_version(&name, &version, min_version(engine))?;
    Ok(match previous_version {
        Some(from) => EngineStatus::Upgraded { from, to: version },
        None => EngineStatus::Installed(version),
    })
}

async fn reconcile_managed_podman_static_configuration(session: &SshSession) -> anyhow::Result<()> {
    let command = format!(
        "if test -f /etc/containers/containers.conf.d/99-jiji-static.conf; then {}; {}; systemctl daemon-reload; fi",
        podman_static_configuration_command(),
        podman_static_storage_configuration_command()
    );
    let result = session.execute(&command).await?;
    if !result.success {
        anyhow::bail!(
            "Could not reconcile Jiji's Podman configuration on {}: {}. Check the host's systemd/Podman state and retry.",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(())
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
            "Could not read {engine_name} version on {}: {}. Confirm {engine_name} is installed and on PATH for the SSH user, then retry.",
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
            "{engine_name} {}.{}.{} is installed, but jiji requires at least {}.{}.{}. Upgrade {engine_name} on this host and retry.",
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

fn version_is_supported(version_output: &str, min: (u64, u64, u64)) -> bool {
    parse_version(version_output).is_none_or(|found| found >= min)
}

pub(crate) fn parse_version(output: &str) -> Option<(u64, u64, u64)> {
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
                "Command `{command}` failed on {} (exit {:?}): {}. Fix the reported error on that host and retry.",
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
            "Could not read /etc/os-release on {}: {}. Confirm the host is a supported Linux distribution and retry.",
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
            "Could not determine the OS distribution on {} from /etc/os-release. Confirm the host is a supported Linux distribution and retry.",
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
            "Don't know how to install docker on unsupported OS '{other}' (version {}). Install docker manually and retry.",
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
            "apt-get install -y -qq ca-certificates curl git iptables tar uidmap".to_string(),
            podman_static_install_command(),
            policy_cmd,
        ]),
        "fedora" | "centos" | "rhel" => Ok(vec![
            "dnf install -y podman curl git".to_string(),
            policy_cmd,
        ]),
        other => anyhow::bail!(
            "Don't know how to install podman on unsupported OS '{other}' (version {}). Install podman manually and retry.",
            os.version_id
        ),
    }
}

fn podman_static_install_command() -> String {
    format!(
        "set -eu; \
arch=$(dpkg --print-architecture); \
case \"$arch\" in \
amd64) checksum={PODMAN_STATIC_AMD64_SHA256} ;; \
arm64) checksum={PODMAN_STATIC_ARM64_SHA256} ;; \
*) echo \"Unsupported architecture '$arch' for the pinned Podman static bundle. Install Podman {PODMAN_STATIC_VERSION} or newer manually.\" >&2; exit 1 ;; \
esac; \
asset=podman-linux-$arch.tar.gz; \
tmp=$(mktemp -d); \
trap 'rm -rf \"$tmp\"' EXIT; \
curl -fsSL \"https://github.com/mgoltzsche/podman-static/releases/download/v{PODMAN_STATIC_VERSION}/$asset\" -o \"$tmp/$asset\"; \
echo \"$checksum  $tmp/$asset\" | sha256sum -c -; \
tar -xzf \"$tmp/$asset\" -C \"$tmp\"; \
cp -a --remove-destination \"$tmp/podman-linux-$arch/usr/local/.\" /usr/local/; \
mkdir -p /etc/containers/containers.conf.d; \
test -e /etc/containers/seccomp.json || install -m 0644 \"$tmp/podman-linux-$arch/etc/containers/seccomp.json\" /etc/containers/seccomp.json; \
{}; \
{}; \
if test -f /etc/apparmor.d/podman; then \
sed -Ei 's!^profile podman /usr/bin/podman !profile podman /usr/{{bin,local/bin}}/podman !' /etc/apparmor.d/podman; \
command -v apparmor_parser >/dev/null 2>&1 && apparmor_parser -r /etc/apparmor.d/podman || true; \
fi; \
systemctl daemon-reload",
        podman_static_configuration_command(),
        podman_static_storage_configuration_command()
    )
}

fn podman_static_configuration_command() -> &'static str {
    "mkdir -p /etc/containers/containers.conf.d; printf '%s\\n' '[containers]' 'log_driver = \"k8s-file\"' '' '[engine]' 'runtime = \"/usr/local/bin/crun\"' 'cgroup_manager = \"cgroupfs\"' 'events_logger = \"file\"' > /etc/containers/containers.conf.d/99-jiji-static.conf"
}

/// Overrides the `mgoltzsche/podman-static` bundle's own `storage.conf`, which pins
/// `mount_program = fuse-overlayfs` unconditionally -- a reasonable default for a
/// distro-independent static binary that can't assume the host kernel supports native overlay, but
/// wrong for jiji's own hosts: every server jiji provisions runs rootful Podman as root on a modern
/// kernel, where native in-kernel overlayfs is both supported and correct. Confirmed live: with
/// fuse-overlayfs forced, `podman rm -f` on an otherwise healthy container repeatedly failed with
/// "removing storage for container ...: replacing mount point ...: directory not empty" -- a
/// FUSE-layer unmount race that native overlay doesn't have. Omitting `mount_program` here (rather
/// than setting it explicitly) is what selects native overlay; Podman only falls back to
/// fuse-overlayfs when either this is set or native overlay genuinely isn't usable. Re-applied by
/// `reconcile_managed_podman_static_configuration` on every `ensure_engine` call, exactly like the
/// `99-jiji-static.conf` containers.conf.d drop-in, so an already-provisioned host self-heals onto
/// native overlay the next time `jiji server setup` runs rather than needing a manual fix.
fn podman_static_storage_configuration_command() -> &'static str {
    "mkdir -p /etc/containers; printf '%s\\n' '[storage]' 'driver = \"overlay\"' 'runroot = \"/var/run/containers/storage\"' 'graphroot = \"/var/lib/containers/storage\"' '' '[storage.options.overlay]' 'ignore_chown_errors = \"true\"' 'mountopt = \"nodev,fsync=0\"' > /etc/containers/storage.conf"
}

#[cfg(test)]
mod tests {
    use super::{
        parse_version, podman_install_commands, version_is_supported, OsInfo, PODMAN_MIN_VERSION,
        PODMAN_STATIC_AMD64_SHA256, PODMAN_STATIC_ARM64_SHA256, PODMAN_STATIC_VERSION,
    };

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

    #[test]
    fn requires_a_recent_podman_version() {
        assert!(!version_is_supported(
            "podman version 4.9.3",
            PODMAN_MIN_VERSION
        ));
        assert!(version_is_supported(
            "podman version 5.8.4",
            PODMAN_MIN_VERSION
        ));
        assert!(version_is_supported(
            "podman version 6.0.2",
            PODMAN_MIN_VERSION
        ));
    }

    #[test]
    fn installs_pinned_static_podman_on_ubuntu() {
        let commands = podman_install_commands(&OsInfo {
            id: "ubuntu".to_string(),
            version_id: "24.04".to_string(),
            version_codename: "noble".to_string(),
        })
        .unwrap();
        let install = commands.join("\n");

        assert!(install.contains(&format!(
            "releases/download/v{PODMAN_STATIC_VERSION}/$asset"
        )));
        assert!(install.contains(PODMAN_STATIC_AMD64_SHA256));
        assert!(install.contains(PODMAN_STATIC_ARM64_SHA256));
        assert!(install.contains("sha256sum -c -"));
        assert!(install.contains("cp -a --remove-destination"));
        assert!(install.contains("test -e /etc/containers/seccomp.json || install"));
        assert!(install.contains("99-jiji-static.conf"));
        assert!(install.contains("runtime = \"/usr/local/bin/crun\""));
        assert!(install.contains("/etc/containers/storage.conf"));
        assert!(!install.contains("mount_program"));
        assert!(!install.contains("apt-get install -y -qq podman"));
    }

    #[test]
    fn managed_static_configuration_pins_the_bundled_runtime() {
        let command = super::podman_static_configuration_command();

        assert!(command.contains("runtime = \"/usr/local/bin/crun\""));
        assert!(command.contains("cgroup_manager = \"cgroupfs\""));
        assert!(command.contains("log_driver = \"k8s-file\""));
    }

    #[test]
    fn managed_storage_configuration_selects_native_overlay_by_omitting_mount_program() {
        let command = super::podman_static_storage_configuration_command();

        assert!(command.contains("driver = \"overlay\""));
        assert!(command.contains("/etc/containers/storage.conf"));
        assert!(!command.contains("mount_program"));
        assert!(!command.contains("fuse-overlayfs"));
    }
}
