use std::collections::BTreeMap;
use std::io::{BufReader, Read, Write};
use std::iter;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use age::secrecy::SecretString;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

const VAULT_FILE: &str = "vault.age";
const METADATA_FILE: &str = "metadata.json";
const LAST_USED_WRITE_WINDOW_NS: i64 = 5 * 60 * 1_000_000_000;

#[derive(Clone)]
pub struct CredentialVault {
    root: PathBuf,
    passphrase: Option<Arc<SecretString>>,
    state: Arc<Mutex<VaultState>>,
}

#[derive(Default)]
struct VaultState {
    entries: BTreeMap<String, StoredCredential>,
    metadata: BTreeMap<String, CredentialMetadata>,
    unlocked: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CredentialSummary {
    pub alias: String,
    pub created_at_ns: i64,
    pub updated_at_ns: i64,
    pub last_used_at_ns: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedCredential {
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredCredential {
    username: String,
    password: String,
    created_at_ns: i64,
    updated_at_ns: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct CredentialMetadata {
    created_at_ns: i64,
    updated_at_ns: i64,
    #[serde(default)]
    last_used_at_ns: i64,
}

impl CredentialVault {
    pub fn open(root: impl Into<PathBuf>, passphrase_env: &str) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .with_context(|| format!("failed to create credential vault '{}'", root.display()))?;

        let passphrase = std::env::var(passphrase_env).ok().filter(|v| !v.is_empty());
        let metadata = read_metadata(&root)?;

        let (entries, unlocked, passphrase) = match passphrase {
            Some(passphrase) => {
                let secret = Arc::new(SecretString::new(passphrase.into()));
                let entries = if vault_path(&root).exists() {
                    decrypt_entries(&root, &secret).with_context(|| {
                        format!(
                            "failed to unlock credential vault '{}' using env var '{}'",
                            root.display(),
                            passphrase_env
                        )
                    })?
                } else {
                    BTreeMap::new()
                };
                (entries, true, Some(secret))
            }
            None => (BTreeMap::new(), false, None),
        };

        Ok(Self {
            root,
            passphrase,
            state: Arc::new(Mutex::new(VaultState {
                entries,
                metadata,
                unlocked,
            })),
        })
    }

    pub fn is_unlocked(&self) -> Result<bool> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("credential vault lock poisoned"))?;
        Ok(state.unlocked)
    }

    pub fn list(&self) -> Result<Vec<CredentialSummary>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("credential vault lock poisoned"))?;
        Ok(state
            .metadata
            .iter()
            .map(|(alias, metadata)| CredentialSummary {
                alias: alias.clone(),
                created_at_ns: metadata.created_at_ns,
                updated_at_ns: metadata.updated_at_ns,
                last_used_at_ns: metadata.last_used_at_ns,
            })
            .collect())
    }

    pub fn add(&self, alias: &str, username: &str, password: &str) -> Result<CredentialSummary> {
        let alias = normalize_alias(alias)?;
        let username = normalize_required("username", username)?;
        let password = normalize_required("password", password)?;
        let now = now_ns();

        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("credential vault lock poisoned"))?;
        ensure_unlocked(&state)?;

        if state.entries.contains_key(&alias) {
            bail!("credential alias '{}' already exists; use update() to change it", alias);
        }

        state.entries.insert(
            alias.clone(),
            StoredCredential {
                username,
                password,
                created_at_ns: now,
                updated_at_ns: now,
            },
        );
        state.metadata.insert(
            alias.clone(),
            CredentialMetadata {
                created_at_ns: now,
                updated_at_ns: now,
                last_used_at_ns: 0,
            },
        );
        self.persist_locked(&state)?;

        Ok(CredentialSummary {
            alias,
            created_at_ns: now,
            updated_at_ns: now,
            last_used_at_ns: 0,
        })
    }

    pub fn update(&self, alias: &str, username: &str, password: &str) -> Result<CredentialSummary> {
        let alias = normalize_alias(alias)?;
        let username = normalize_required("username", username)?;
        let password = normalize_required("password", password)?;
        let now = now_ns();

        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("credential vault lock poisoned"))?;
        ensure_unlocked(&state)?;

        let entry = state
            .entries
            .get(&alias)
            .ok_or_else(|| anyhow!("credential alias '{}' not found", alias))?;
        let created_at_ns = entry.created_at_ns;
        let last_used_at_ns = state
            .metadata
            .get(&alias)
            .map(|m| m.last_used_at_ns)
            .unwrap_or_default();

        state.entries.insert(
            alias.clone(),
            StoredCredential {
                username,
                password,
                created_at_ns,
                updated_at_ns: now,
            },
        );
        state.metadata.insert(
            alias.clone(),
            CredentialMetadata {
                created_at_ns,
                updated_at_ns: now,
                last_used_at_ns,
            },
        );
        self.persist_locked(&state)?;

        Ok(CredentialSummary {
            alias,
            created_at_ns,
            updated_at_ns: now,
            last_used_at_ns,
        })
    }

    pub fn remove(&self, alias: &str) -> Result<Option<CredentialSummary>> {
        let alias = normalize_alias(alias)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("credential vault lock poisoned"))?;
        ensure_unlocked(&state)?;

        let metadata = state.metadata.remove(&alias);
        let removed = state.entries.remove(&alias);
        if removed.is_some() {
            self.persist_locked(&state)?;
        }

        Ok(metadata.map(|metadata| CredentialSummary {
            alias,
            created_at_ns: metadata.created_at_ns,
            updated_at_ns: metadata.updated_at_ns,
            last_used_at_ns: metadata.last_used_at_ns,
        }))
    }

    pub fn username_for_alias(&self, alias: &str) -> Result<String> {
        let alias = normalize_alias(alias)?;
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("credential vault lock poisoned"))?;
        ensure_unlocked(&state)?;

        let entry = state
            .entries
            .get(&alias)
            .with_context(|| format!("credential alias '{alias}' not found"))?;
        Ok(entry.username.clone())
    }

    pub fn resolve(&self, alias: &str) -> Result<ResolvedCredential> {
        let alias = normalize_alias(alias)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("credential vault lock poisoned"))?;
        ensure_unlocked(&state)?;

        let entry = state
            .entries
            .get(&alias)
            .cloned()
            .with_context(|| format!("credential alias '{alias}' not found"))?;
        if let Some(metadata) = state.metadata.get_mut(&alias) {
            let now = now_ns();
            if should_persist_last_used(metadata.last_used_at_ns, now) {
                metadata.last_used_at_ns = now;
                write_metadata(&self.root, &state.metadata)?;
            }
        }

        Ok(ResolvedCredential {
            username: entry.username,
            password: entry.password,
        })
    }

    fn persist_locked(&self, state: &VaultState) -> Result<()> {
        let passphrase = self
            .passphrase
            .as_ref()
            .ok_or_else(|| anyhow!("credential vault is locked; set BONSAI_VAULT_PASSPHRASE"))?;
        encrypt_entries(&self.root, passphrase, &state.entries)?;
        write_metadata(&self.root, &state.metadata)?;
        Ok(())
    }
}

fn should_persist_last_used(last_used_at_ns: i64, now_ns: i64) -> bool {
    last_used_at_ns <= 0 || now_ns.saturating_sub(last_used_at_ns) >= LAST_USED_WRITE_WINDOW_NS
}

fn ensure_unlocked(state: &VaultState) -> Result<()> {
    if state.unlocked {
        Ok(())
    } else {
        bail!("credential vault is locked; set BONSAI_VAULT_PASSPHRASE before starting bonsai")
    }
}

fn vault_path(root: &Path) -> PathBuf {
    root.join(VAULT_FILE)
}

fn metadata_path(root: &Path) -> PathBuf {
    root.join(METADATA_FILE)
}

fn decrypt_entries(
    root: &Path,
    passphrase: &SecretString,
) -> Result<BTreeMap<String, StoredCredential>> {
    let encrypted = std::fs::read(vault_path(root))
        .with_context(|| format!("failed to read credential vault '{}'", root.display()))?;
    let decryptor = age::Decryptor::new_buffered(BufReader::new(&encrypted[..]))
        .context("invalid age vault file")?;
    if !decryptor.is_scrypt() {
        bail!("credential vault is not passphrase-encrypted");
    }

    let identity = age::scrypt::Identity::new(passphrase.clone());
    let mut reader = decryptor
        .decrypt(iter::once(&identity as _))
        .context("credential vault passphrase rejected")?;
    let mut plaintext = String::new();
    reader
        .read_to_string(&mut plaintext)
        .context("failed to read decrypted credential vault")?;
    serde_json::from_str(&plaintext).context("failed to parse decrypted credential vault")
}

fn encrypt_entries(
    root: &Path,
    passphrase: &SecretString,
    entries: &BTreeMap<String, StoredCredential>,
) -> Result<()> {
    let plaintext =
        serde_json::to_vec_pretty(entries).context("failed to serialize credential vault")?;
    let encryptor = age::Encryptor::with_user_passphrase(passphrase.clone());
    let mut encrypted = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut encrypted)
        .context("failed to start credential vault encryption")?;
    writer
        .write_all(&plaintext)
        .context("failed to encrypt credential vault")?;
    writer
        .finish()
        .context("failed to finish credential vault encryption")?;
    std::fs::write(vault_path(root), encrypted)
        .with_context(|| format!("failed to write credential vault '{}'", root.display()))?;
    Ok(())
}

fn read_metadata(root: &Path) -> Result<BTreeMap<String, CredentialMetadata>> {
    let path = metadata_path(root);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read credential metadata '{}'", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse credential metadata '{}'", path.display()))
}

fn write_metadata(root: &Path, metadata: &BTreeMap<String, CredentialMetadata>) -> Result<()> {
    let serialized = serde_json::to_string_pretty(metadata)
        .context("failed to serialize credential metadata")?;
    std::fs::write(metadata_path(root), serialized)
        .with_context(|| format!("failed to write credential metadata '{}'", root.display()))?;
    Ok(())
}

fn normalize_alias(alias: &str) -> Result<String> {
    let alias = alias.trim();
    if alias.is_empty() {
        bail!("credential alias is required");
    }
    if alias.len() > 128
        || !alias
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':'))
    {
        bail!("credential alias must use ASCII letters, digits, dash, underscore, dot, or colon");
    }
    Ok(alias.to_string())
}

fn normalize_required(field: &str, value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{field} is required");
    }
    Ok(value.to_string())
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn temp_vault_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos();
        std::env::temp_dir().join(format!("bonsai-{name}-{nanos}"))
    }

    #[test]
    fn vault_adds_lists_resolves_and_removes_credentials() {
        let _guard = env_lock().lock().expect("credential env test lock");
        let dir = temp_vault_dir("vault-round-trip");
        unsafe {
            std::env::set_var(
                "BONSAI_TEST_VAULT_PASSPHRASE",
                "correct horse battery staple",
            )
        };

        let vault = CredentialVault::open(&dir, "BONSAI_TEST_VAULT_PASSPHRASE").unwrap();
        assert!(vault.is_unlocked().unwrap());

        let added = vault.add("srl-lab-admin", "admin", "NokiaSrl1!").unwrap();
        assert_eq!(added.alias, "srl-lab-admin");
        assert_eq!(vault.list().unwrap().len(), 1);

        let resolved = vault.resolve("srl-lab-admin").unwrap();
        assert_eq!(resolved.username, "admin");
        assert_eq!(resolved.password, "NokiaSrl1!");

        let reopened = CredentialVault::open(&dir, "BONSAI_TEST_VAULT_PASSPHRASE").unwrap();
        let resolved = reopened.resolve("srl-lab-admin").unwrap();
        assert_eq!(resolved.username, "admin");
        assert_eq!(resolved.password, "NokiaSrl1!");

        let removed = reopened.remove("srl-lab-admin").unwrap();
        assert!(removed.is_some());
        assert!(reopened.list().unwrap().is_empty());

        unsafe { std::env::remove_var("BONSAI_TEST_VAULT_PASSPHRASE") };
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wrong_passphrase_fails_during_open() {
        let _guard = env_lock().lock().expect("credential env test lock");
        let dir = temp_vault_dir("vault-wrong-passphrase");
        unsafe { std::env::set_var("BONSAI_TEST_VAULT_PASSPHRASE", "right") };
        let vault = CredentialVault::open(&dir, "BONSAI_TEST_VAULT_PASSPHRASE").unwrap();
        vault.add("lab", "admin", "secret").unwrap();

        unsafe { std::env::set_var("BONSAI_TEST_VAULT_PASSPHRASE", "wrong") };
        let error = match CredentialVault::open(&dir, "BONSAI_TEST_VAULT_PASSPHRASE") {
            Ok(_) => panic!("wrong passphrase unexpectedly opened vault"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("failed to unlock credential vault"));

        unsafe { std::env::remove_var("BONSAI_TEST_VAULT_PASSPHRASE") };
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resolve_debounces_last_used_metadata_writes() {
        let _guard = env_lock().lock().expect("credential env test lock");
        let dir = temp_vault_dir("vault-last-used-debounce");
        unsafe { std::env::set_var("BONSAI_TEST_VAULT_PASSPHRASE", "right") };
        let vault = CredentialVault::open(&dir, "BONSAI_TEST_VAULT_PASSPHRASE").unwrap();
        vault.add("lab", "admin", "secret").unwrap();

        for _ in 0..50 {
            let resolved = vault.resolve("lab").unwrap();
            assert_eq!(resolved.username, "admin");
        }
        let after_burst = std::fs::read_to_string(metadata_path(&dir)).unwrap();

        for _ in 0..50 {
            vault.resolve("lab").unwrap();
        }
        let after_second_burst = std::fs::read_to_string(metadata_path(&dir)).unwrap();
        assert_eq!(after_burst, after_second_burst);

        let summaries = vault.list().unwrap();
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].last_used_at_ns > 0);

        unsafe { std::env::remove_var("BONSAI_TEST_VAULT_PASSPHRASE") };
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn last_used_write_window_skips_recent_updates() {
        let now = 10 * LAST_USED_WRITE_WINDOW_NS;
        assert!(should_persist_last_used(0, now));
        assert!(should_persist_last_used(
            now - LAST_USED_WRITE_WINDOW_NS,
            now
        ));
        assert!(!should_persist_last_used(
            now - LAST_USED_WRITE_WINDOW_NS + 1,
            now
        ));
    }
}
