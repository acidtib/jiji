use std::path::Path;

use russh_sftp::client::SftpSession;
use tokio::io::AsyncWriteExt;

use crate::error::SshError;
use crate::session::SshSession;

impl SshSession {
    /// Uploads a local file's full contents to `remote_path`, creating it if missing and
    /// truncating it if present -- ordinary "upload" semantics, not the atomic
    /// write-to-temp-then-rename behavior `jiji-cli`'s `mounts.rs::upload_file` uses. This is a
    /// minimal primitive with no current caller in this codebase; see
    /// docs/ssh-deferred-features-plan.md for why the existing `mounts.rs`/`env_resolution.rs`
    /// upload paths are deliberately not migrated to it.
    pub async fn sftp_put(&self, local_path: &Path, remote_path: &str) -> Result<(), SshError> {
        let data = tokio::fs::read(local_path).await?;
        let sftp = self.open_sftp().await?;
        let mut file = sftp
            .create(remote_path)
            .await
            .map_err(|source| self.sftp_error(remote_path, source.to_string()))?;
        file.write_all(&data)
            .await
            .map_err(|source| self.sftp_error(remote_path, source.to_string()))?;
        file.shutdown()
            .await
            .map_err(|source| self.sftp_error(remote_path, source.to_string()))?;
        Ok(())
    }

    /// Downloads a remote file's full contents to `local_path`, creating/truncating it locally.
    pub async fn sftp_get(&self, remote_path: &str, local_path: &Path) -> Result<(), SshError> {
        let sftp = self.open_sftp().await?;
        let data = sftp
            .read(remote_path)
            .await
            .map_err(|source| self.sftp_error(remote_path, source.to_string()))?;
        tokio::fs::write(local_path, data).await?;
        Ok(())
    }

    async fn open_sftp(&self) -> Result<SftpSession, SshError> {
        let stream = self.open_sftp_stream().await?;
        SftpSession::new(stream)
            .await
            .map_err(|source| self.sftp_error("<sftp init>", source.to_string()))
    }

    fn sftp_error(&self, path: &str, reason: String) -> SshError {
        SshError::Sftp {
            host: self.host().to_string(),
            path: path.to_string(),
            reason,
        }
    }
}
