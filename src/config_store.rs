use std::io::{BufReader, Read, Write};
use std::path::PathBuf;

use age::secrecy::SecretString;
use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub struct ConfigStore {
    root: PathBuf,
    passphrase_env: String,
}

#[derive(Clone, Debug)]
pub struct StoredConfigSnapshot {
    pub relative_path: String,
    pub bytes_len: usize,
    pub sha256: String,
}

impl ConfigStore {
    pub fn open(root: impl Into<PathBuf>, passphrase_env: impl Into<String>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .with_context(|| format!("failed to create config store '{}'", root.display()))?;
        Ok(Self {
            root,
            passphrase_env: passphrase_env.into(),
        })
    }

    pub fn store_snapshot(
        &self,
        device_address: &str,
        snapshot_id: &str,
        plaintext: &str,
    ) -> Result<StoredConfigSnapshot> {
        let secret = self.passphrase()?;
        let device_dir = self.root.join(sanitize(device_address));
        std::fs::create_dir_all(&device_dir).with_context(|| {
            format!(
                "failed to create config snapshot directory '{}'",
                device_dir.display()
            )
        })?;

        let file_name = format!("{snapshot_id}.age");
        let full_path = device_dir.join(&file_name);
        let encryptor = age::Encryptor::with_user_passphrase(secret.clone());
        let mut encrypted = Vec::new();
        let mut writer = encryptor
            .wrap_output(&mut encrypted)
            .context("failed to wrap config snapshot encryptor")?;
        writer
            .write_all(plaintext.as_bytes())
            .context("failed to write encrypted config snapshot")?;
        writer
            .finish()
            .context("failed to finish config snapshot")?;
        std::fs::write(&full_path, encrypted)
            .with_context(|| format!("failed to persist snapshot '{}'", full_path.display()))?;

        let relative_path = format!("{}/{}", sanitize(device_address), file_name);
        Ok(StoredConfigSnapshot {
            relative_path,
            bytes_len: plaintext.len(),
            sha256: sha256_hex(plaintext),
        })
    }

    pub fn read_snapshot(&self, relative_path: &str) -> Result<String> {
        let secret = self.passphrase()?;
        let full_path = self.root.join(relative_path);
        let encrypted = std::fs::read(&full_path)
            .with_context(|| format!("failed to read snapshot '{}'", full_path.display()))?;
        let decryptor = age::Decryptor::new_buffered(BufReader::new(&encrypted[..]))
            .context("failed to read encrypted config snapshot")?;
        if !decryptor.is_scrypt() {
            bail!("config snapshot is not passphrase-encrypted");
        }
        let identity = age::scrypt::Identity::new(secret.clone());
        let mut reader = decryptor
            .decrypt(std::iter::once(&identity as _))
            .context("failed to decrypt config snapshot")?;
        let mut plaintext = String::new();
        reader
            .read_to_string(&mut plaintext)
            .context("failed to decode config snapshot")?;
        Ok(plaintext)
    }

    fn passphrase(&self) -> Result<SecretString> {
        let value = std::env::var(&self.passphrase_env)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "config store is locked: set {} to capture encrypted snapshots",
                    self.passphrase_env
                )
            })?;
        Ok(SecretString::new(value.into()))
    }
}

pub fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

fn sanitize(value: &str) -> String {
    value.replace(['/', '\\', ':'], "_")
}

pub fn summarize_diff(previous: &str, current: &str) -> (i64, i64, String) {
    let previous_lines: std::collections::BTreeSet<&str> = previous.lines().collect();
    let current_lines: std::collections::BTreeSet<&str> = current.lines().collect();

    let added = current_lines.difference(&previous_lines).count() as i64;
    let removed = previous_lines.difference(&current_lines).count() as i64;
    let summary = if added == 0 && removed == 0 {
        "unchanged".to_string()
    } else {
        format!("{added} line(s) added, {removed} line(s) removed")
    };
    (added, removed, summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_diff_counts_added_and_removed_lines() {
        let (added, removed, summary) = summarize_diff("a\nb\n", "b\nc\n");
        assert_eq!(added, 1);
        assert_eq!(removed, 1);
        assert!(summary.contains("1 line(s) added"));
    }

    #[test]
    fn sha256_hex_is_stable() {
        assert_eq!(
            sha256_hex("bonsai"),
            "7dd7122ad9bf240f04fdf988a0df4a2552098ad8ed8df429bed1056ebdb64387"
        );
    }
}
