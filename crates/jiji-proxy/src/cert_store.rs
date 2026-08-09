//! Dynamic, SNI-based TLS certificate resolution. Replaces phase 1's single
//! static cert-file listener: every terminated host gets its own entry here,
//! looked up by SNI at handshake time via Pingora's `TlsAccept` hook
//! (`certificate_callback`), populated either by a static file pair an
//! operator drops into `cert_dir` (`{host}.crt`/`{host}.key`, loaded once at
//! startup and never touched again) or by `acme.rs`'s issuance/renewal loop.
//! A static file always wins: `acme.rs` only ever issues/renews hosts whose
//! current entry is missing or itself ACME-sourced. See "TLS certificates"
//! in `docs/architecture-notes.md#private-networking-wireguard-mesh--agent-served-dns`.

use async_trait::async_trait;
use pingora::listeners::TlsAccept;
use pingora::protocols::tls::TlsRef;
use pingora::tls::ext;
use pingora::tls::pkey::{PKey, Private};
use pingora::tls::ssl;
use pingora::tls::x509::X509;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertSource {
    /// Loaded from `cert_dir` at startup; `acme.rs` never issues, renews, or
    /// overwrites this host.
    Static,
    /// Issued/renewed by `acme.rs`; eligible for automatic renewal.
    Acme,
}

pub struct CertEntry {
    pub cert: X509,
    pub key: PKey<Private>,
    pub source: CertSource,
}

#[derive(Clone)]
pub struct CertStore {
    dir: Arc<PathBuf>,
    entries: Arc<RwLock<HashMap<String, Arc<CertEntry>>>>,
}

impl CertStore {
    /// Scans `dir` for `{host}.crt`/`{host}.key` pairs and loads each as a
    /// `Static` entry. `dir` is created if it doesn't exist yet (a fresh
    /// jiji-proxy container has nothing to load until ACME issues its first
    /// certificate).
    pub fn load(dir: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let mut entries = HashMap::new();
        for item in std::fs::read_dir(&dir)? {
            let item = item?;
            let path = item.path();
            let Some(host) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|_| path.extension().is_some_and(|ext| ext == "crt"))
            else {
                continue;
            };
            let key_path = dir.join(format!("{host}.key"));
            if !key_path.exists() {
                tracing::warn!(host, cert = %path.display(), "found a .crt with no matching .key; skipping");
                continue;
            }
            let cert = X509::from_pem(&std::fs::read(&path)?)?;
            let key = PKey::private_key_from_pem(&std::fs::read(&key_path)?)?;
            tracing::info!(host, "loaded static certificate");
            entries.insert(
                host.to_string(),
                Arc::new(CertEntry {
                    cert,
                    key,
                    source: CertSource::Static,
                }),
            );
        }
        Ok(Self {
            dir: Arc::new(dir),
            entries: Arc::new(RwLock::new(entries)),
        })
    }

    /// Exact-match first; if `host` has no entry of its own, falls back to
    /// whatever wildcard-pattern entry would match it (see
    /// `wildcard::parent_wildcard_key`). Only a `Static` entry (a
    /// user-provided PEM pair) can ever be stored under a wildcard key --
    /// `acme.rs` never issues for one, since `RouteManager::tls_hosts`
    /// excludes wildcard hosts from its worklist entirely (HTTP-01 can't
    /// issue a wildcard certificate).
    fn get(&self, host: &str) -> Option<Arc<CertEntry>> {
        let entries = self.entries.read().expect("cert store lock poisoned");
        if let Some(entry) = entries.get(host) {
            return Some(entry.clone());
        }
        let wildcard_key = crate::wildcard::parent_wildcard_key(host)?;
        entries.get(&wildcard_key).cloned()
    }

    /// `true` if `host` has no certificate yet, or has an `Acme`-sourced one
    /// expiring within `renew_before`. Always `false` for a `Static` entry.
    pub fn needs_issuance(&self, host: &str, renew_before: Duration) -> bool {
        match self.get(host) {
            None => true,
            Some(entry) if entry.source == CertSource::Static => false,
            Some(entry) => {
                let days = (renew_before.as_secs() / 86_400).max(1) as u32;
                let threshold = match openssl::asn1::Asn1Time::days_from_now(days) {
                    Ok(threshold) => threshold,
                    Err(error) => {
                        tracing::warn!(%error, host, "could not compute renewal threshold; treating as due");
                        return true;
                    }
                };
                entry.cert.not_after() < threshold
            }
        }
    }

    /// Parses and atomically persists an ACME-issued cert/key pair to
    /// `cert_dir`, then makes it immediately servable.
    pub fn store_acme_cert(&self, host: &str, cert_pem: &str, key_pem: &str) -> anyhow::Result<()> {
        let cert = X509::from_pem(cert_pem.as_bytes())?;
        let key = PKey::private_key_from_pem(key_pem.as_bytes())?;

        write_atomic(&self.dir.join(format!("{host}.crt")), cert_pem.as_bytes())?;
        write_atomic(&self.dir.join(format!("{host}.key")), key_pem.as_bytes())?;

        self.entries
            .write()
            .expect("cert store lock poisoned")
            .insert(
                host.to_string(),
                Arc::new(CertEntry {
                    cert,
                    key,
                    source: CertSource::Acme,
                }),
            );
        Ok(())
    }
}

fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, contents)?;
    std::fs::rename(&tmp_path, path)
}

/// The `TlsAccept` hook Pingora calls mid-handshake; looks up the requested
/// SNI hostname in the shared `CertStore` and, if found, loads it into the
/// connection. A host with no entry yet (e.g. ACME hasn't issued for it) is
/// left with no certificate set, which fails that one handshake rather than
/// the whole listener -- it self-resolves once the next issuance succeeds.
pub struct DynamicCertResolver {
    pub certs: CertStore,
}

#[async_trait]
impl TlsAccept for DynamicCertResolver {
    async fn certificate_callback(&self, ssl_ref: &mut TlsRef) {
        // `servername()` borrows from `ssl_ref`; converting to an owned
        // `String` immediately releases that borrow so `ssl_ref` can be
        // passed mutably to `ext::ssl_use_certificate`/`ssl_use_private_key`
        // below.
        let Some(host) = ssl_ref
            .servername(ssl::NameType::HOST_NAME)
            .map(str::to_string)
        else {
            return;
        };
        let Some(entry) = self.certs.get(&host) else {
            tracing::warn!(host, "TLS handshake for a host with no certificate yet");
            return;
        };
        if let Err(error) = ext::ssl_use_certificate(ssl_ref, &entry.cert) {
            tracing::error!(%error, host, "failed to set certificate for handshake");
        }
        if let Err(error) = ext::ssl_use_private_key(ssl_ref, &entry.key) {
            tracing::error!(%error, host, "failed to set private key for handshake");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A throwaway self-signed cert/key pair, generated once for this test only
    // (`openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1
    // -keyout test.key -out test.crt -days 1 -nodes -subj "/CN=test"`); its
    // subject/expiry are irrelevant since this only exercises CertStore's
    // lookup, never a real handshake.
    const TEST_CERT: &str = "-----BEGIN CERTIFICATE-----\n\
        MIIBczCCARmgAwIBAgIUDxbO7Zq4t/N6ENGdWzIEmodSSMIwCgYIKoZIzj0EAwIw\n\
        DzENMAsGA1UEAwwEdGVzdDAeFw0yNjA4MDYwNTI1NTRaFw0yNjA4MDcwNTI1NTRa\n\
        MA8xDTALBgNVBAMMBHRlc3QwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAAR9Jior\n\
        +rE+VcaFFE5wgMu+O0T3Tb4ILYDLQfyzIzMth4dCLUD/BKk2n8IEtF8W8PArsc+5\n\
        KYRUp18btC2s4zdxo1MwUTAdBgNVHQ4EFgQUSBoJ50S+dIRlJH4tRYQEQpC4Bmww\n\
        HwYDVR0jBBgwFoAUSBoJ50S+dIRlJH4tRYQEQpC4BmwwDwYDVR0TAQH/BAUwAwEB\n\
        /zAKBggqhkjOPQQDAgNIADBFAiA8Yvhj1Lj77Vg0U9Zo8zir8WdHhE8sjQoA0LKo\n\
        iNDJFQIhAKdvuF5aWw9NJ5LYe2ISpUOXgHnk+p4qpH8Cur5RV7LX\n\
        -----END CERTIFICATE-----\n";
    const TEST_KEY: &str = "-----BEGIN PRIVATE KEY-----\n\
        MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgKHDm/RibUjD+CTW9\n\
        0x60pCpdD6ugoTOe273i3IM2MMOhRANCAAR9Jior+rE+VcaFFE5wgMu+O0T3Tb4I\n\
        LYDLQfyzIzMth4dCLUD/BKk2n8IEtF8W8PArsc+5KYRUp18btC2s4zdx\n\
        -----END PRIVATE KEY-----\n";

    #[test]
    fn a_static_wildcard_cert_is_found_for_a_matching_subdomain() {
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("*.example.com.crt"), TEST_CERT).unwrap();
        std::fs::write(dir.path().join("*.example.com.key"), TEST_KEY).unwrap();

        let store = CertStore::load(dir.path().to_path_buf()).expect("load cert store");
        assert!(store.get("foo.example.com").is_some());
        assert!(store.get("bar.example.com").is_some());
    }

    #[test]
    fn a_static_wildcard_cert_does_not_match_a_nested_subdomain() {
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("*.example.com.crt"), TEST_CERT).unwrap();
        std::fs::write(dir.path().join("*.example.com.key"), TEST_KEY).unwrap();

        let store = CertStore::load(dir.path().to_path_buf()).expect("load cert store");
        assert!(store.get("deep.foo.example.com").is_none());
    }

    #[test]
    fn an_exact_cert_takes_precedence_over_a_matching_wildcard() {
        let dir = tempfile::tempdir().expect("create temp dir");
        std::fs::write(dir.path().join("*.example.com.crt"), TEST_CERT).unwrap();
        std::fs::write(dir.path().join("*.example.com.key"), TEST_KEY).unwrap();
        std::fs::write(dir.path().join("api.example.com.crt"), TEST_CERT).unwrap();
        std::fs::write(dir.path().join("api.example.com.key"), TEST_KEY).unwrap();

        let store = CertStore::load(dir.path().to_path_buf()).expect("load cert store");
        assert!(store.get("api.example.com").is_some());
    }
}
