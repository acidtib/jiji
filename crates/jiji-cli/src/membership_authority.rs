//! Local project membership authority.
//!
//! The private key never leaves the operator machine. Hosts receive only the
//! verifying key and authority-signed operations.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use ed25519_dalek::SigningKey;

pub struct ProjectAuthority {
    pub id: String,
    pub signing_key: SigningKey,
}

pub struct AuthorityRotation {
    pub current: ProjectAuthority,
    pub next: ProjectAuthority,
    current_path: std::path::PathBuf,
    next_path: std::path::PathBuf,
    previous_path: std::path::PathBuf,
}

impl ProjectAuthority {
    pub fn load_or_create(config_path: &Path) -> anyhow::Result<Self> {
        let config_dir = config_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("configuration path has no parent directory"))?;
        let directory = config_dir.join("control-plane");
        let path = directory.join("membership-authority.key");
        fs::create_dir_all(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        let bytes = if path.exists() {
            fs::read(&path)?
        } else {
            let mut bytes = [0_u8; 32];
            OpenOptions::new()
                .read(true)
                .open("/dev/urandom")?
                .read_exact(&mut bytes)?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            bytes.to_vec()
        };
        let key_bytes: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("{} must contain exactly 32 bytes", path.display()))?;
        let mode = fs::metadata(&path)?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            anyhow::bail!(
                "{} permissions are {:o}; membership authority keys must not be accessible by group or others",
                path.display(),
                mode
            );
        }
        Ok(Self {
            id: "project-root".into(),
            signing_key: SigningKey::from_bytes(&key_bytes),
        })
    }

    pub fn load_or_create_node_key(
        config_path: &Path,
        node_id: &str,
    ) -> anyhow::Result<SigningKey> {
        let config_dir = config_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("configuration path has no parent directory"))?;
        let directory = config_dir.join("control-plane/nodes");
        fs::create_dir_all(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        let safe_name = jiji_network::systemd_unit_slug(node_id);
        let path = directory.join(format!("{safe_name}.key"));
        let bytes = load_or_create_secret(&path)?;
        Ok(SigningKey::from_bytes(&bytes))
    }

    pub fn stage_rotation(config_path: &Path) -> anyhow::Result<AuthorityRotation> {
        let current = Self::load_or_create(config_path)?;
        let directory = authority_directory(config_path)?;
        let current_path = directory.join("membership-authority.key");
        let next_path = directory.join("membership-authority.next.key");
        let previous_path = directory.join("membership-authority.previous.key");
        let next = ProjectAuthority {
            id: "project-root".into(),
            signing_key: SigningKey::from_bytes(&load_or_create_secret(&next_path)?),
        };
        Ok(AuthorityRotation {
            current,
            next,
            current_path,
            next_path,
            previous_path,
        })
    }

    pub fn previous(config_path: &Path) -> anyhow::Result<Option<ProjectAuthority>> {
        let path = authority_directory(config_path)?.join("membership-authority.previous.key");
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(ProjectAuthority {
            id: "project-root".into(),
            signing_key: SigningKey::from_bytes(&load_or_create_secret(&path)?),
        }))
    }
}

impl AuthorityRotation {
    pub fn commit(self) -> anyhow::Result<()> {
        if self.previous_path.exists() {
            anyhow::bail!(
                "a previous authority is still retained; finalize that rotation before starting another"
            );
        }
        fs::rename(&self.current_path, &self.previous_path)?;
        fs::rename(&self.next_path, &self.current_path)?;
        Ok(())
    }
}

pub fn finalize_rotation(config_path: &Path) -> anyhow::Result<()> {
    let path = authority_directory(config_path)?.join("membership-authority.previous.key");
    if !path.exists() {
        anyhow::bail!("no previous membership authority is awaiting retirement");
    }
    fs::remove_file(path)?;
    Ok(())
}

fn authority_directory(config_path: &Path) -> anyhow::Result<std::path::PathBuf> {
    Ok(config_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("configuration path has no parent directory"))?
        .join("control-plane"))
}

pub fn recovery_epoch(config_path: &Path) -> anyhow::Result<u64> {
    let path = authority_directory(config_path)?.join("recovery-epoch");
    if !path.exists() {
        return Ok(1);
    }
    let value = fs::read_to_string(&path)?;
    let epoch = value.trim().parse::<u64>()?;
    if epoch == 0 {
        anyhow::bail!("{} must contain a positive recovery epoch", path.display());
    }
    Ok(epoch)
}

pub fn write_recovery_epoch(config_path: &Path, epoch: u64) -> anyhow::Result<()> {
    if epoch == 0 {
        anyhow::bail!("recovery epoch must be positive");
    }
    let directory = authority_directory(config_path)?;
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let path = directory.join("recovery-epoch");
    let staged = directory.join("recovery-epoch.new");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&staged)?;
    writeln!(file, "{epoch}")?;
    file.sync_all()?;
    fs::rename(staged, path)?;
    Ok(())
}

/// Installs the operator authority recovered from an authenticated backup and advances the epoch
/// last. Existing node signing material is archived (not deleted), ensuring replacement setup
/// generates fresh node keys. Repeating a completed recovery is refused by the caller's epoch
/// validation rather than silently advancing twice.
pub fn install_recovered_authority(
    config_path: &Path,
    signing_key: &[u8; 32],
    previous_epoch: u64,
    next_epoch: u64,
) -> anyhow::Result<()> {
    if next_epoch <= previous_epoch {
        anyhow::bail!("recovery must advance the project epoch");
    }
    let directory = authority_directory(config_path)?;
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;

    let nodes = directory.join("nodes");
    let archived_nodes = directory.join(format!("nodes.pre-recovery-epoch-{previous_epoch}"));
    if nodes.exists() {
        if archived_nodes.exists() {
            anyhow::bail!(
                "{} already exists; inspect the interrupted recovery before retrying",
                archived_nodes.display()
            );
        }
        fs::rename(&nodes, &archived_nodes)?;
    }

    let current_key = directory.join("membership-authority.key");
    let archived_key = directory.join(format!(
        "membership-authority.pre-recovery-epoch-{previous_epoch}.key"
    ));
    if current_key.exists() && !archived_key.exists() {
        fs::rename(&current_key, &archived_key)?;
    }
    let staged_key = directory.join("membership-authority.recovery.new");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&staged_key)?;
    file.write_all(signing_key)?;
    file.sync_all()?;
    fs::rename(&staged_key, &current_key)?;
    write_recovery_epoch(config_path, next_epoch)
}

fn load_or_create_secret(path: &Path) -> anyhow::Result<[u8; 32]> {
    let bytes = if path.exists() {
        fs::read(path)?
    } else {
        let mut bytes = [0_u8; 32];
        OpenOptions::new()
            .read(true)
            .open("/dev/urandom")?
            .read_exact(&mut bytes)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        bytes.to_vec()
    };
    let key_bytes: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("{} must contain exactly 32 bytes", path.display()))?;
    let mode = fs::metadata(path)?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        anyhow::bail!(
            "{} permissions are {:o}; signing keys must not be accessible by group or others",
            path.display(),
            mode
        );
    }
    Ok(key_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn authority_is_created_private_and_stable() {
        let dir = tempdir().unwrap();
        let config = dir.path().join(".jiji/deploy.yml");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        let first = ProjectAuthority::load_or_create(&config).unwrap();
        let second = ProjectAuthority::load_or_create(&config).unwrap();
        assert_eq!(first.signing_key.to_bytes(), second.signing_key.to_bytes());
        let key_path = config
            .parent()
            .unwrap()
            .join("control-plane/membership-authority.key");
        assert_eq!(
            fs::metadata(key_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn rotation_keeps_overlap_until_explicit_finalize() {
        let dir = tempdir().unwrap();
        let config = dir.path().join(".jiji/deploy.yml");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        let original = ProjectAuthority::load_or_create(&config)
            .unwrap()
            .signing_key
            .to_bytes();
        let rotation = ProjectAuthority::stage_rotation(&config).unwrap();
        let next = rotation.next.signing_key.to_bytes();
        assert_ne!(original, next);
        rotation.commit().unwrap();
        assert_eq!(
            ProjectAuthority::load_or_create(&config)
                .unwrap()
                .signing_key
                .to_bytes(),
            next
        );
        assert_eq!(
            ProjectAuthority::previous(&config)
                .unwrap()
                .unwrap()
                .signing_key
                .to_bytes(),
            original
        );
        finalize_rotation(&config).unwrap();
        assert!(ProjectAuthority::previous(&config).unwrap().is_none());
    }

    #[test]
    fn disaster_recovery_archives_node_material_and_commits_epoch_last() {
        let dir = tempdir().unwrap();
        let config = dir.path().join(".jiji/deploy.yml");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        ProjectAuthority::load_or_create(&config).unwrap();
        ProjectAuthority::load_or_create_node_key(&config, "node-a").unwrap();
        let recovered = [55_u8; 32];
        install_recovered_authority(&config, &recovered, 1, 2).unwrap();

        assert_eq!(recovery_epoch(&config).unwrap(), 2);
        assert_eq!(
            ProjectAuthority::load_or_create(&config)
                .unwrap()
                .signing_key
                .to_bytes(),
            recovered
        );
        let control = config.parent().unwrap().join("control-plane");
        assert!(control.join("nodes.pre-recovery-epoch-1").is_dir());
        assert!(control
            .join("membership-authority.pre-recovery-epoch-1.key")
            .is_file());
        assert!(!control.join("nodes").exists());
    }
}
