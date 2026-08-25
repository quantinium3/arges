use aes_gcm::{
    Aes256Gcm,
    aead::{Aead, KeyInit, Nonce, Payload},
};
use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::fs::read;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const KEY_ID_DOMAIN: &[u8] = b"arges:master-key-id:v1";
const DATA_AAD_DOMAIN: &[u8] = b"arges:secret:data:v1";
const DEK_AAD_DOMAIN: &[u8] = b"arges:secret:dek:v1";

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct MasterKey {
    key: [u8; 32],
    id: String,
}

pub struct EncryptedValue {
    pub key_id: String,
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
    pub wrapped_dek: Vec<u8>,
    pub dek_nonce: Vec<u8>,
}

fn nonce_from_bytes(bytes: &[u8]) -> Result<Nonce<Aes256Gcm>> {
    bytes
        .try_into()
        .map_err(|_| anyhow!("invalid nonce length"))
}

fn random_nonce() -> Result<Nonce<Aes256Gcm>> {
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).context("failed to generate nonce")?;
    let nonce: Nonce<Aes256Gcm> = bytes
        .as_slice()
        .try_into()
        .expect("nonce is exactly NonceSize bytes");
    Ok(nonce)
}

fn key_id(key: &[u8; 32]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(KEY_ID_DOMAIN);
    hasher.update(key);
    hex::encode(&hasher.finalize()[..8])
}

fn aad(domain: &[u8], name: &str, version: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(domain.len() + name.len() + 16);
    out.extend_from_slice(domain);
    out.extend_from_slice(&(name.len() as u64).to_le_bytes());
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&version.to_le_bytes());
    out
}

#[cfg(unix)]
async fn ensure_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("failed to stat master key at {}", path.display()))?
        .permissions()
        .mode();

    if mode & 0o077 != 0 {
        bail!(
            "master key at {} is readable by group or others (mode {:04o}); run chmod 600 on it",
            path.display(),
            mode & 0o7777
        );
    }

    Ok(())
}

#[cfg(not(unix))]
async fn ensure_private(_path: &Path) -> Result<()> {
    Ok(())
}

impl MasterKey {
    pub async fn load(path: &Path) -> Result<Self> {
        ensure_private(path).await?;

        let bytes = Zeroizing::new(
            read(path)
                .await
                .with_context(|| format!("failed to read master key from {}", path.display()))?,
        );
        let key: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("master key must be 32 bytes, got {}", bytes.len()))?;
        let id = key_id(&key);

        Ok(Self { key, id })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    fn cipher(&self) -> Aes256Gcm {
        Aes256Gcm::new_from_slice(&self.key).expect("master key is exactly 32 bytes")
    }

    pub fn encrypt(&self, name: &str, version: u64, plaintext: &str) -> Result<EncryptedValue> {
        let mut dek = Zeroizing::new([0u8; 32]);
        getrandom::fill(dek.as_mut_slice()).context("failed to generate dek")?;

        let data_cipher =
            Aes256Gcm::new_from_slice(dek.as_slice()).map_err(|_| anyhow!("invalid dek length"))?;
        let nonce = random_nonce()?;
        let ciphertext = data_cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: &aad(DATA_AAD_DOMAIN, name, version),
                },
            )
            .map_err(|_| anyhow!("failed to encrypt value"))?;

        let dek_nonce = random_nonce()?;
        let wrapped_dek = self
            .cipher()
            .encrypt(
                &dek_nonce,
                Payload {
                    msg: dek.as_slice(),
                    aad: &aad(DEK_AAD_DOMAIN, name, version),
                },
            )
            .map_err(|_| anyhow!("failed to wrap dek"))?;

        Ok(EncryptedValue {
            key_id: self.id.clone(),
            ciphertext,
            nonce: nonce.to_vec(),
            wrapped_dek,
            dek_nonce: dek_nonce.to_vec(),
        })
    }

    pub fn decrypt(
        &self,
        name: &str,
        version: u64,
        enc: &EncryptedValue,
    ) -> Result<Zeroizing<String>> {
        if enc.key_id != self.id {
            bail!(
                "value was wrapped by master key {}, but master key {} is loaded",
                enc.key_id,
                self.id
            );
        }

        let dek_nonce = nonce_from_bytes(&enc.dek_nonce)?;
        let dek = Zeroizing::new(
            self.cipher()
                .decrypt(
                    &dek_nonce,
                    Payload {
                        msg: enc.wrapped_dek.as_slice(),
                        aad: &aad(DEK_AAD_DOMAIN, name, version),
                    },
                )
                .map_err(|_| anyhow!("failed to unwrap dek"))?,
        );

        let data_cipher =
            Aes256Gcm::new_from_slice(dek.as_slice()).map_err(|_| anyhow!("invalid dek length"))?;
        let nonce = nonce_from_bytes(&enc.nonce)?;
        let plaintext_bytes = Zeroizing::new(
            data_cipher
                .decrypt(
                    &nonce,
                    Payload {
                        msg: enc.ciphertext.as_slice(),
                        aad: &aad(DATA_AAD_DOMAIN, name, version),
                    },
                )
                .map_err(|_| anyhow!("failed to decrypt value"))?,
        );

        let plaintext = std::str::from_utf8(&plaintext_bytes)
            .map_err(|_| anyhow!("decrypted value was not valid utf-8"))?;

        Ok(Zeroizing::new(plaintext.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master_key() -> MasterKey {
        let mut k = [0u8; 32];
        getrandom::fill(&mut k).unwrap();
        let id = key_id(&k);
        MasterKey { key: k, id }
    }

    #[test]
    fn roundtrip() {
        let mk = master_key();
        let enc = mk.encrypt("db/password", 1, "hunter2").unwrap();
        assert_eq!(enc.key_id, mk.id());
        assert_eq!(enc.nonce.len(), 12);
        assert_eq!(enc.wrapped_dek.len(), 48);
        assert_eq!(&*mk.decrypt("db/password", 1, &enc).unwrap(), "hunter2");
    }

    #[test]
    fn swapping_ciphertext_between_names_is_rejected() {
        let mk = master_key();
        let api = mk.encrypt("api/key", 1, "api-key").unwrap();
        let swapped = EncryptedValue {
            key_id: api.key_id.clone(),
            ciphertext: api.ciphertext.clone(),
            nonce: api.nonce.clone(),
            wrapped_dek: api.wrapped_dek.clone(),
            dek_nonce: api.dek_nonce.clone(),
        };
        let err = mk.decrypt("db/password", 1, &swapped).unwrap_err();
        assert!(err.to_string().contains("unwrap dek"), "{err}");
    }

    #[test]
    fn restoring_an_older_version_is_rejected() {
        let mk = master_key();
        let old = mk.encrypt("db/password", 1, "old-secret").unwrap();
        assert!(mk.decrypt("db/password", 2, &old).is_err());
        assert_eq!(&*mk.decrypt("db/password", 1, &old).unwrap(), "old-secret");
    }

    #[test]
    fn aad_layers_are_domain_separated() {
        let mk = master_key();
        let enc = mk.encrypt("n", 1, "v").unwrap();
        let crossed = EncryptedValue {
            key_id: enc.key_id.clone(),
            ciphertext: enc.wrapped_dek.clone(),
            nonce: enc.dek_nonce.clone(),
            wrapped_dek: enc.wrapped_dek.clone(),
            dek_nonce: enc.dek_nonce.clone(),
        };
        assert!(mk.decrypt("n", 1, &crossed).is_err());
    }

    #[test]
    fn name_boundary_is_unambiguous() {
        let mk = master_key();
        let enc = mk.encrypt("ab", 1, "v").unwrap();
        assert!(mk.decrypt("a", 1, &enc).is_err());
        assert!(mk.decrypt("abc", 1, &enc).is_err());
    }

    #[test]
    fn foreign_key_id_rejected_before_crypto() {
        let mk = master_key();
        let mut enc = mk.encrypt("n", 1, "v").unwrap();
        enc.key_id = "0011223344556677".to_string();
        let err = mk.decrypt("n", 1, &enc).unwrap_err();
        assert!(err.to_string().contains("wrapped by master key"), "{err}");
    }

    #[test]
    fn key_id_is_stable_and_distinct() {
        let a = master_key();
        let b = master_key();
        assert_eq!(a.id(), key_id(&a.key));
        assert_ne!(a.id(), b.id());
        assert_eq!(a.id().len(), 16);
    }

    #[test]
    fn wrong_master_key_and_tampering_are_rejected() {
        let mk = master_key();
        let mut enc = mk.encrypt("n", 1, "v").unwrap();
        let other = master_key();
        let mut for_other = EncryptedValue {
            key_id: other.id().to_string(),
            ciphertext: enc.ciphertext.clone(),
            nonce: enc.nonce.clone(),
            wrapped_dek: enc.wrapped_dek.clone(),
            dek_nonce: enc.dek_nonce.clone(),
        };
        assert!(other.decrypt("n", 1, &for_other).is_err());
        for_other.key_id = enc.key_id.clone();
        enc.ciphertext[0] ^= 1;
        assert!(mk.decrypt("n", 1, &enc).is_err());
    }

    #[tokio::test]
    async fn load_checks_permissions_and_length() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("arges-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("master.key");
        std::fs::write(&path, [7u8; 32]).unwrap();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let err = match MasterKey::load(&path).await {
            Ok(_) => panic!("expected failure"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("chmod 600"), "{err}");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mk = MasterKey::load(&path).await.unwrap();
        assert_eq!(mk.id(), key_id(&[7u8; 32]));

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
        let mk = MasterKey::load(&path).await.unwrap();
        assert_eq!(mk.id(), key_id(&[7u8; 32]));

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        std::fs::write(&path, [7u8; 31]).unwrap();
        let err = match MasterKey::load(&path).await {
            Ok(_) => panic!("expected failure"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("32 bytes"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
