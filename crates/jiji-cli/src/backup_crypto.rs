//! Authenticated, passphrase-encrypted control-plane backup envelope.
//!
//! The passphrase is supplied as bytes by the caller (normally read from a mode-0600 file), never
//! as a command-line argument. Scrypt derives an independent 256-bit key for every random salt;
//! AES-256-GCM authenticates both the payload and the fixed format marker.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use scrypt::{scrypt, Params};

const MAGIC: &[u8; 8] = b"JIJIBK01";
const SALT_BYTES: usize = 16;
const NONCE_BYTES: usize = 12;

pub fn encrypt(plaintext: &[u8], passphrase: &[u8]) -> anyhow::Result<Vec<u8>> {
    if passphrase.is_empty() {
        anyhow::bail!("backup passphrase must not be empty");
    }
    let mut salt = [0_u8; SALT_BYTES];
    let mut nonce = [0_u8; NONCE_BYTES];
    getrandom::fill(&mut salt)?;
    getrandom::fill(&mut nonce)?;
    let key = derive_key(passphrase, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| anyhow::anyhow!("could not initialize backup encryption"))?;
    let nonce = Nonce::try_from(nonce.as_slice())
        .map_err(|_| anyhow::anyhow!("could not initialize backup nonce"))?;
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: MAGIC,
            },
        )
        .map_err(|_| anyhow::anyhow!("could not encrypt control-plane backup"))?;
    let mut envelope =
        Vec::with_capacity(MAGIC.len() + SALT_BYTES + NONCE_BYTES + ciphertext.len());
    envelope.extend_from_slice(MAGIC);
    envelope.extend_from_slice(&salt);
    envelope.extend_from_slice(&nonce);
    envelope.extend_from_slice(&ciphertext);
    Ok(envelope)
}

pub fn decrypt(envelope: &[u8], passphrase: &[u8]) -> anyhow::Result<Vec<u8>> {
    if passphrase.is_empty() {
        anyhow::bail!("backup passphrase must not be empty");
    }
    let header = MAGIC.len() + SALT_BYTES + NONCE_BYTES;
    if envelope.len() <= header || &envelope[..MAGIC.len()] != MAGIC {
        anyhow::bail!("file is not a supported Jiji control-plane backup");
    }
    let salt_start = MAGIC.len();
    let nonce_start = salt_start + SALT_BYTES;
    let payload_start = nonce_start + NONCE_BYTES;
    let key = derive_key(passphrase, &envelope[salt_start..nonce_start])?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| anyhow::anyhow!("could not initialize backup decryption"))?;
    let nonce = Nonce::try_from(&envelope[nonce_start..payload_start])
        .map_err(|_| anyhow::anyhow!("backup nonce is malformed"))?;
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: &envelope[payload_start..],
                aad: MAGIC,
            },
        )
        .map_err(|_| {
            anyhow::anyhow!(
                "backup authentication failed; the passphrase is wrong or the file was modified"
            )
        })
}

fn derive_key(passphrase: &[u8], salt: &[u8]) -> anyhow::Result<[u8; 32]> {
    let mut key = [0_u8; 32];
    scrypt(passphrase, salt, &Params::RECOMMENDED, &mut key)
        .map_err(|error| anyhow::anyhow!("backup key derivation failed: {error}"))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_uses_randomized_authenticated_envelopes() {
        let first = encrypt(b"catalog", b"correct horse").unwrap();
        let second = encrypt(b"catalog", b"correct horse").unwrap();
        assert_ne!(first, second);
        assert_eq!(decrypt(&first, b"correct horse").unwrap(), b"catalog");
        assert!(decrypt(&first, b"wrong").is_err());
    }

    #[test]
    fn tampering_and_truncation_are_rejected() {
        let mut encrypted = encrypt(b"important", b"secret").unwrap();
        let last = encrypted.len() - 1;
        encrypted[last] ^= 1;
        assert!(decrypt(&encrypted, b"secret").is_err());
        assert!(decrypt(b"short", b"secret").is_err());
    }
}
