//! The project's recovery-epoch fencing counter.
//!
//! Unrelated to key material: this is a plain integer, advanced only when rebuilding a lost
//! control plane (`jiji network recover`) so surviving or stale hosts can't silently rejoin with
//! pre-loss state. Stored locally under the project's `.jiji` directory, independent of the config
//! file itself so any command with the config loaded can read/advance it.

use std::path::{Path, PathBuf};

pub(crate) fn directory(config_path: &Path) -> anyhow::Result<PathBuf> {
    Ok(config_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("configuration path has no parent directory"))?
        .join("recovery"))
}

pub fn read(config_path: &Path) -> anyhow::Result<u64> {
    let path = directory(config_path)?.join("epoch");
    if !path.exists() {
        return Ok(1);
    }
    let value = std::fs::read_to_string(&path)?;
    let epoch = value.trim().parse::<u64>()?;
    if epoch == 0 {
        anyhow::bail!("{} must contain a positive recovery epoch", path.display());
    }
    Ok(epoch)
}

pub fn write(config_path: &Path, epoch: u64) -> anyhow::Result<()> {
    if epoch == 0 {
        anyhow::bail!("recovery epoch must be positive");
    }
    let directory = directory(config_path)?;
    std::fs::create_dir_all(&directory)?;
    let path = directory.join("epoch");
    let staged = directory.join("epoch.new");
    std::fs::write(&staged, format!("{epoch}\n"))?;
    std::fs::rename(staged, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn defaults_to_one_and_round_trips() {
        let dir = tempdir().unwrap();
        let config = dir.path().join(".jiji/deploy.yml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        assert_eq!(read(&config).unwrap(), 1);
        write(&config, 3).unwrap();
        assert_eq!(read(&config).unwrap(), 3);
    }

    #[test]
    fn zero_is_rejected() {
        let dir = tempdir().unwrap();
        let config = dir.path().join(".jiji/deploy.yml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        assert!(write(&config, 0).is_err());
    }
}
