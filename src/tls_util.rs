//! Vault-aware TLS certificate resolution.
//!
//! All cert path fields (TargetConfig.ca_cert, RuntimeTlsConfig.ca_cert, etc.) may be either:
//!   - A filesystem path  (e.g. "lab/fast-iteration/ca.pem")
//!   - A vault reference  (e.g. "vault:srl-lab-ca")
//!
//! Call `read_cert_pem(path, vault)` instead of `tokio::fs::read(path)` wherever cert bytes
//! are loaded. This is the single integration point between the Certs UI and runtime TLS.

use anyhow::{Context, Result};
use crate::credentials::{CredentialVault, ResolvePurpose};

const VAULT_PREFIX: &str = "vault:";

/// Read PEM bytes from either a vault reference or a filesystem path.
///
/// # Vault reference
/// If `path` starts with `"vault:"`, the remainder is treated as a cert name and the PEM
/// is retrieved from the vault alias `cert-{name}`. The vault must be unlocked.
///
/// # Filesystem path
/// Otherwise, `path` is treated as a normal filesystem path and read with `tokio::fs::read`.
pub async fn read_cert_pem(path: &str, vault: &CredentialVault) -> Result<Vec<u8>> {
    if let Some(name) = path.strip_prefix(VAULT_PREFIX) {
        let alias = format!("cert-{}", name.trim().to_lowercase().replace(' ', "-"));
        let cred = vault
            .resolve(&alias, ResolvePurpose::Internal)
            .with_context(|| format!("cert '{}' not found in vault (alias: {})", name, alias))?;
        let pem = cred.password_string();
        if !pem.contains("-----BEGIN") {
            anyhow::bail!(
                "vault alias '{}' does not contain a valid PEM block (cert name: '{}')",
                alias,
                name
            );
        }
        Ok(pem.into_bytes())
    } else {
        tokio::fs::read(path)
            .await
            .with_context(|| format!("could not read CA cert from '{path}'"))
    }
}

/// Synchronous variant for contexts where async is not available (e.g. ingest.rs startup).
pub fn read_cert_pem_sync(path: &str, vault: &CredentialVault) -> Result<Vec<u8>> {
    if let Some(name) = path.strip_prefix(VAULT_PREFIX) {
        let alias = format!("cert-{}", name.trim().to_lowercase().replace(' ', "-"));
        let cred = vault
            .resolve(&alias, ResolvePurpose::Internal)
            .with_context(|| format!("cert '{}' not found in vault (alias: {})", name, alias))?;
        let pem = cred.password_string();
        if !pem.contains("-----BEGIN") {
            anyhow::bail!(
                "vault alias '{}' does not contain a valid PEM block (cert name: '{}')",
                alias,
                name
            );
        }
        Ok(pem.into_bytes())
    } else {
        std::fs::read(path)
            .with_context(|| format!("could not read cert from '{path}'"))
    }
}

/// Returns true if this path string is a vault reference.
pub fn is_vault_ref(path: &str) -> bool {
    path.starts_with(VAULT_PREFIX)
}

/// Check that a cert path is reachable — either the vault alias exists or the file exists.
/// Returns `Ok(source)` where source is "vault" or "file", or `Err(reason)`.
pub async fn verify_cert_path(path: &str, vault: &CredentialVault) -> Result<&'static str> {
    if let Some(name) = path.strip_prefix(VAULT_PREFIX) {
        let alias = format!("cert-{}", name.trim().to_lowercase().replace(' ', "-"));
        vault
            .resolve(&alias, ResolvePurpose::Internal)
            .with_context(|| format!("vault cert '{}' not found (alias: {})", name, alias))?;
        Ok("vault")
    } else {
        tokio::fs::metadata(path)
            .await
            .with_context(|| format!("cert file not found at '{path}'"))?;
        Ok("file")
    }
}
