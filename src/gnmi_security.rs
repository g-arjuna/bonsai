//! Explicit gNMI transport-security contract (M5.1).
//!
//! One authoritative path turns a `TargetConfig` (or discovery inputs)
//! into the material a gNMI client needs: effective security mode, CA
//! PEM, SNI domain and optional mTLS client identity.
//!
//! Contract:
//! - `security = "tls_verify"` — CA-verified TLS + SNI. Requires `ca_cert`
//!   and `tls_domain`; client cert material is rejected as ambiguous.
//! - `security = "mtls"` — `tls_verify` plus `client_cert`/`client_key`
//!   (filesystem paths or `vault:` references via `tls_util`).
//! - `security = "plaintext"` — explicit laboratory cleartext opt-in.
//!   Conflicting TLS material is rejected; the choice is audit-visible
//!   (`plaintext_explicit`) and counted via `bonsai_gnmi_plaintext_dial_total`.
//! - `security` absent — legacy inference preserved for upgrade
//!   compatibility: `ca_cert` present ⇒ TLS verify, absent ⇒ plaintext
//!   (`plaintext_legacy`). Legacy plaintext is never a *selected* secure
//!   mode and never silently downgrades a configured TLS target: any
//!   missing/invalid TLS material is a hard error, not a fallback.

use anyhow::{Context, Result, bail};
use tonic::transport::{Certificate, ClientTlsConfig, Identity};

use crate::config::{GnmiSecurityMode, TargetConfig};
use crate::credentials::CredentialVault;

/// Effective transport security after legacy inference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectiveSecurity {
    /// CA-verified TLS with SNI (explicit `tls_verify` or legacy `ca_cert`).
    TlsVerify,
    /// TLS verify plus client certificate (explicit `mtls` only).
    MutualTls,
    /// Cleartext gRPC. `explicit` distinguishes the audited laboratory
    /// opt-in from legacy inferred plaintext.
    Plaintext { explicit: bool },
}

impl EffectiveSecurity {
    pub fn is_tls(self) -> bool {
        !matches!(self, Self::Plaintext { .. })
    }

    /// Operator/diagnostic label — never confused with gNMI model origins.
    pub fn label(self) -> &'static str {
        match self {
            Self::TlsVerify => "tls_verify",
            Self::MutualTls => "mtls",
            Self::Plaintext { explicit: true } => "plaintext_explicit",
            Self::Plaintext { explicit: false } => "plaintext_legacy",
        }
    }

    /// Whether this transport is authorized for production device
    /// sessions (Subscribe, Set/remediation, collector acquisition).
    ///
    /// Legacy-inferred plaintext (`security` and `ca_cert` both absent)
    /// is *readable configuration* but is NOT *authorized operational
    /// transport*: it must be migrated to an explicit mode. Only the
    /// deliberate laboratory opt-in `security="plaintext"` or a TLS mode
    /// authorizes cleartext-or-better device I/O. Configured credentials
    /// may only be transmitted on an authorized transport.
    pub fn authorized_for_acquisition(self) -> bool {
        !matches!(self, Self::Plaintext { explicit: false })
    }
}

/// Resolved connection material for one gNMI dial (subscribe, discover,
/// set). PEM bytes are loaded once at spawn/resolve time; the struct is
/// `Clone` so conn-info helpers can carry it by value.
#[derive(Clone)]
pub struct ConnSecurity {
    pub effective: EffectiveSecurity,
    pub ca_cert_pem: Option<Vec<u8>>,
    pub tls_domain: String,
    pub client_cert_pem: Option<Vec<u8>>,
    pub client_key_pem: Option<Vec<u8>>,
}

impl ConnSecurity {
    /// Plaintext lab/test constructor — no TLS material at all.
    pub fn plaintext(explicit: bool) -> Self {
        Self {
            effective: EffectiveSecurity::Plaintext { explicit },
            ca_cert_pem: None,
            tls_domain: String::new(),
            client_cert_pem: None,
            client_key_pem: None,
        }
    }

    pub fn scheme(&self) -> &'static str {
        if self.effective.is_tls() {
            "https"
        } else {
            "http"
        }
    }

    /// Build the tonic client TLS config, or `None` for plaintext.
    /// Fails closed: a TLS effective mode without complete material is an
    /// error, never a downgrade to cleartext.
    pub fn client_tls_config(&self) -> Result<Option<ClientTlsConfig>> {
        if !self.effective.is_tls() {
            return Ok(None);
        }
        let ca_pem = self
            .ca_cert_pem
            .as_ref()
            .context("internal: TLS security resolved without CA certificate PEM")?;
        let mut tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(ca_pem.clone()))
            .domain_name(self.tls_domain.clone());
        if self.effective == EffectiveSecurity::MutualTls {
            let cert = self
                .client_cert_pem
                .as_ref()
                .context("internal: mTLS resolved without client certificate PEM")?;
            let key = self
                .client_key_pem
                .as_ref()
                .context("internal: mTLS resolved without client key PEM")?;
            tls = tls.identity(Identity::from_pem(cert.clone(), key.clone()));
        }
        Ok(Some(tls))
    }
}

/// Pure field-consistency validation — no IO, safe for API boundaries.
/// Returns an operator-visible reason on the first violation.
pub fn validate_security_fields(
    security: Option<GnmiSecurityMode>,
    ca_cert: Option<&str>,
    tls_domain: Option<&str>,
    client_cert: Option<&str>,
    client_key: Option<&str>,
) -> Result<(), String> {
    let has_ca = ca_cert.map(|s| !s.trim().is_empty()).unwrap_or(false);
    let has_domain = tls_domain.map(|s| !s.trim().is_empty()).unwrap_or(false);
    let has_cert = client_cert.map(|s| !s.trim().is_empty()).unwrap_or(false);
    let has_key = client_key.map(|s| !s.trim().is_empty()).unwrap_or(false);

    if has_cert != has_key {
        return Err("client_cert and client_key must be configured together for mTLS".to_string());
    }
    if (has_cert || has_key) && security != Some(GnmiSecurityMode::Mtls) {
        return Err(
            "client_cert/client_key require security=\"mtls\"; client identity is never \
             sent under tls_verify or plaintext"
                .to_string(),
        );
    }

    match security {
        Some(GnmiSecurityMode::Mtls) => {
            if !has_ca {
                return Err("security=\"mtls\" requires ca_cert".to_string());
            }
            if !has_domain {
                return Err("security=\"mtls\" requires tls_domain (SNI)".to_string());
            }
            if !has_cert {
                return Err(
                    "security=\"mtls\" requires both client_cert and client_key".to_string()
                );
            }
        }
        Some(GnmiSecurityMode::TlsVerify) => {
            if !has_ca {
                return Err("security=\"tls_verify\" requires ca_cert".to_string());
            }
            if !has_domain {
                return Err("security=\"tls_verify\" requires tls_domain (SNI)".to_string());
            }
        }
        Some(GnmiSecurityMode::Plaintext) => {
            if has_ca {
                return Err(
                    "security=\"plaintext\" conflicts with ca_cert; remove the CA or choose a \
                     TLS mode — ambiguous transport security is rejected"
                        .to_string(),
                );
            }
            if has_domain {
                return Err(
                    "security=\"plaintext\" conflicts with tls_domain; SNI is meaningless \
                     on a cleartext channel"
                        .to_string(),
                );
            }
        }
        None => {
            // Legacy inference: no explicit constraints. client_cert/key
            // were already rejected above unless security=mtls.
        }
    }
    Ok(())
}

/// Effective mode for a target, applying documented legacy inference.
pub fn effective_mode(target: &TargetConfig) -> EffectiveSecurity {
    infer_effective_mode(
        target.security,
        target
            .ca_cert
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false),
    )
}

/// Shared inference used by every resolution path: explicit modes win;
/// absent security falls back to the documented legacy rule.
pub fn infer_effective_mode(
    security: Option<GnmiSecurityMode>,
    has_ca_cert: bool,
) -> EffectiveSecurity {
    match security {
        Some(GnmiSecurityMode::TlsVerify) => EffectiveSecurity::TlsVerify,
        Some(GnmiSecurityMode::Mtls) => EffectiveSecurity::MutualTls,
        Some(GnmiSecurityMode::Plaintext) => EffectiveSecurity::Plaintext { explicit: true },
        None => {
            if has_ca_cert {
                EffectiveSecurity::TlsVerify
            } else {
                EffectiveSecurity::Plaintext { explicit: false }
            }
        }
    }
}

/// Convenience wrapper for `TargetConfig`.
pub fn validate_target_security(target: &TargetConfig) -> Result<(), String> {
    validate_security_fields(
        target.security,
        target.ca_cert.as_deref(),
        target.tls_domain.as_deref(),
        target.client_cert.as_deref(),
        target.client_key.as_deref(),
    )
}

async fn read_pem(path: &str, vault: Option<&CredentialVault>) -> Result<Vec<u8>> {
    if crate::tls_util::is_vault_ref(path) {
        // Vault references are core-side material. A collector resolves
        // trust material from its own filesystem only — a `vault:` ref
        // that happens to be valid on core is NOT assumed available
        // here. Fail closed with an explicit unsupported error rather
        // than a confusing filesystem miss.
        let vault = vault.context(
            "vault: certificate references are resolved on core only; \
             collector-local trust material must use filesystem paths",
        )?;
        return crate::tls_util::read_cert_pem(path, vault).await;
    }
    tokio::fs::read(path)
        .await
        .with_context(|| format!("could not read PEM material from '{path}'"))
}

/// Private-key files resolved from the collector/core filesystem must
/// not be accessible to other users. Vault-held keys are encrypted at
/// rest and exempt. Group-read is permitted but logged.
#[cfg(unix)]
async fn check_private_key_file_permissions(path: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("could not stat client key '{path}'"))?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o007 != 0 {
        anyhow::bail!(
            "client key '{path}' is accessible to other users (mode {mode:04o}); \
             restrict permissions (e.g. chmod 600) — refusing to load private key material"
        );
    }
    if mode & 0o040 != 0 {
        tracing::warn!(
            path,
            mode = format!("{mode:04o}"),
            "client key file is group-readable"
        );
    }
    Ok(())
}

#[cfg(not(unix))]
async fn check_private_key_file_permissions(_path: &str) -> Result<()> {
    Ok(())
}

/// Read client-key PEM, enforcing filesystem permission policy when the
/// reference is a plain path (vault-held keys are exempt).
async fn read_client_key_pem(path: &str, vault: Option<&CredentialVault>) -> Result<Vec<u8>> {
    let pem = read_pem(path, vault).await?;
    if !crate::tls_util::is_vault_ref(path) {
        check_private_key_file_permissions(path).await?;
    }
    Ok(pem)
}

/// Resolve the full connection security for a managed target.
/// Vault-aware when `vault` is provided (core mode); filesystem-only for
/// collector mode. Any inconsistency or unreadable material is a hard
/// error — never a silent downgrade.
pub async fn resolve_conn_security(
    target: &TargetConfig,
    vault: Option<&CredentialVault>,
) -> Result<ConnSecurity> {
    validate_target_security(target).map_err(|e| anyhow::anyhow!(e))?;
    let effective = effective_mode(target);
    if !effective.is_tls() {
        return Ok(ConnSecurity::plaintext(matches!(
            effective,
            EffectiveSecurity::Plaintext { explicit: true }
        )));
    }

    let ca_cert_pem = match &target.ca_cert {
        Some(path) if !path.trim().is_empty() => Some(read_pem(path, vault).await?),
        _ => None,
    };
    // TLS modes require the CA — validation guarantees presence for
    // explicit modes; legacy inference keys off ca_cert.is_some().
    if ca_cert_pem.is_none() {
        bail!("TLS security selected but no ca_cert is configured");
    }
    let tls_domain = target
        .tls_domain
        .clone()
        .unwrap_or_default()
        .trim()
        .to_string();
    if tls_domain.is_empty() {
        bail!("tls_domain is required for TLS gNMI connections (SNI)");
    }

    let (client_cert_pem, client_key_pem) = if effective == EffectiveSecurity::MutualTls {
        let cert = target
            .client_cert
            .as_deref()
            .context("internal: mTLS without client_cert")?;
        let key = target
            .client_key
            .as_deref()
            .context("internal: mTLS without client_key")?;
        (
            Some(read_pem(cert, vault).await?),
            Some(read_client_key_pem(key, vault).await?),
        )
    } else {
        (None, None)
    };

    Ok(ConnSecurity {
        effective,
        ca_cert_pem,
        tls_domain,
        client_cert_pem,
        client_key_pem,
    })
}

/// Resolve connection security for a production device session
/// (Subscribe, Set/remediation, collector acquisition). Identical to
/// [`resolve_conn_security`] except that legacy-inferred plaintext is
/// rejected: readable legacy configuration must be explicitly migrated
/// (`security="plaintext"` lab opt-in or a TLS mode) before it can carry
/// operational device I/O. The error is `migration_required` — never a
/// silent downgrade and never a plaintext retry after TLS failure.
pub async fn resolve_conn_security_for_acquisition(
    target: &TargetConfig,
    vault: Option<&CredentialVault>,
) -> Result<ConnSecurity> {
    let conn = resolve_conn_security(target, vault).await?;
    if !conn.effective.authorized_for_acquisition() {
        anyhow::bail!(
            "migration_required: legacy plaintext gNMI transport is readable \
             configuration but not authorized for device sessions; set \
             security=\"plaintext\" for an isolated laboratory or configure \
             security=\"tls_verify\"/\"mtls\""
        );
    }
    Ok(conn)
}

/// Field-level variant for discovery/probe inputs that are not a full
/// `TargetConfig`.
pub async fn resolve_conn_security_parts(
    security: Option<GnmiSecurityMode>,
    ca_cert: Option<&str>,
    tls_domain: Option<&str>,
    client_cert: Option<&str>,
    client_key: Option<&str>,
    vault: Option<&CredentialVault>,
) -> Result<ConnSecurity> {
    validate_security_fields(security, ca_cert, tls_domain, client_cert, client_key)
        .map_err(|e| anyhow::anyhow!(e))?;
    let effective = infer_effective_mode(
        security,
        ca_cert.map(|s| !s.trim().is_empty()).unwrap_or(false),
    );
    if !effective.is_tls() {
        return Ok(ConnSecurity::plaintext(matches!(
            effective,
            EffectiveSecurity::Plaintext { explicit: true }
        )));
    }
    let ca_path = ca_cert.context("internal: TLS without ca_cert path")?;
    let ca_cert_pem = Some(read_pem(ca_path, vault).await?);
    let domain = tls_domain.unwrap_or_default().trim().to_string();
    if domain.is_empty() {
        bail!("tls_domain is required for TLS gNMI connections (SNI)");
    }
    let (client_cert_pem, client_key_pem) = if effective == EffectiveSecurity::MutualTls {
        (
            Some(
                read_pem(
                    client_cert.context("internal: mTLS without client_cert")?,
                    vault,
                )
                .await?,
            ),
            Some(
                read_client_key_pem(
                    client_key.context("internal: mTLS without client_key")?,
                    vault,
                )
                .await?,
            ),
        )
    } else {
        (None, None)
    };
    Ok(ConnSecurity {
        effective,
        ca_cert_pem,
        tls_domain: domain,
        client_cert_pem,
        client_key_pem,
    })
}

/// Validate that credentials can be encoded as gRPC metadata before any
/// RPC is initiated. Invalid values fail closed here — they must never
/// be silently dropped from the request headers.
pub fn validate_auth_metadata(username: Option<&str>, password: Option<&str>) -> Result<()> {
    for (label, value) in [("username", username), ("password", password)] {
        if let Some(v) = value
            && tonic::metadata::MetadataValue::try_from(v).is_err()
        {
            anyhow::bail!("gNMI {label} contains characters invalid for gRPC metadata");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> TargetConfig {
        TargetConfig {
            address: "192.0.2.1:443".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn legacy_inference_preserved() {
        let mut t = target();
        assert_eq!(
            effective_mode(&t),
            EffectiveSecurity::Plaintext { explicit: false }
        );
        t.ca_cert = Some("ca.pem".to_string());
        assert_eq!(effective_mode(&t), EffectiveSecurity::TlsVerify);
        t.security = Some(GnmiSecurityMode::Mtls);
        assert_eq!(effective_mode(&t), EffectiveSecurity::MutualTls);
        t.security = Some(GnmiSecurityMode::Plaintext);
        assert_eq!(
            effective_mode(&t),
            EffectiveSecurity::Plaintext { explicit: true }
        );
    }

    #[test]
    fn validation_matrix() {
        // mtls requires the full material set.
        assert!(
            validate_security_fields(Some(GnmiSecurityMode::Mtls), None, None, None, None).is_err()
        );
        assert!(
            validate_security_fields(
                Some(GnmiSecurityMode::Mtls),
                Some("ca"),
                Some("dev"),
                Some("c"),
                None
            )
            .is_err()
        );
        assert!(
            validate_security_fields(
                Some(GnmiSecurityMode::Mtls),
                Some("ca"),
                Some("dev"),
                Some("c"),
                Some("k")
            )
            .is_ok()
        );
        // tls_verify rejects client identity as ambiguous.
        assert!(
            validate_security_fields(
                Some(GnmiSecurityMode::TlsVerify),
                Some("ca"),
                Some("dev"),
                Some("c"),
                Some("k")
            )
            .is_err()
        );
        // plaintext conflicts with TLS material.
        assert!(
            validate_security_fields(
                Some(GnmiSecurityMode::Plaintext),
                Some("ca"),
                None,
                None,
                None
            )
            .is_err()
        );
        assert!(
            validate_security_fields(
                Some(GnmiSecurityMode::Plaintext),
                None,
                Some("dev"),
                None,
                None
            )
            .is_err()
        );
        assert!(
            validate_security_fields(Some(GnmiSecurityMode::Plaintext), None, None, None, None)
                .is_ok()
        );
        // Legacy (None) cannot smuggle client certs.
        assert!(validate_security_fields(None, None, None, Some("c"), Some("k")).is_err());
        // tls_verify requires CA + domain.
        assert!(
            validate_security_fields(
                Some(GnmiSecurityMode::TlsVerify),
                None,
                Some("d"),
                None,
                None
            )
            .is_err()
        );
        assert!(
            validate_security_fields(
                Some(GnmiSecurityMode::TlsVerify),
                Some("ca"),
                None,
                None,
                None
            )
            .is_err()
        );
        assert!(
            validate_security_fields(
                Some(GnmiSecurityMode::TlsVerify),
                Some("ca"),
                Some("d"),
                None,
                None
            )
            .is_ok()
        );
    }

    #[test]
    fn security_mode_parse_is_strict() {
        assert_eq!(
            GnmiSecurityMode::parse("mtls"),
            Some(GnmiSecurityMode::Mtls)
        );
        assert_eq!(
            GnmiSecurityMode::parse("TLS_VERIFY"),
            Some(GnmiSecurityMode::TlsVerify)
        );
        assert_eq!(GnmiSecurityMode::parse("bogus"), None);
        assert_eq!(GnmiSecurityMode::parse(""), None);
    }

    #[test]
    fn conn_security_tls_config_fails_closed_without_material() {
        let conn = ConnSecurity {
            effective: EffectiveSecurity::TlsVerify,
            ca_cert_pem: None,
            tls_domain: "dev".to_string(),
            client_cert_pem: None,
            client_key_pem: None,
        };
        assert!(conn.client_tls_config().is_err());
        let plain = ConnSecurity::plaintext(true);
        assert!(plain.client_tls_config().unwrap().is_none());
        assert_eq!(plain.scheme(), "http");
    }

    #[test]
    fn auth_metadata_validation_rejects_unencodable_values() {
        assert!(validate_auth_metadata(Some("user"), Some("pass")).is_ok());
        assert!(validate_auth_metadata(None, None).is_ok());
        // gRPC metadata forbids non-visible-ASCII — must fail BEFORE the
        // RPC, never be silently dropped from headers.
        assert!(validate_auth_metadata(Some("bad\nvalue"), None).is_err());
        assert!(validate_auth_metadata(None, Some("bad\r\nvalue")).is_err());
    }

    #[tokio::test]
    async fn legacy_plaintext_is_migration_required_for_acquisition() {
        let mut t = target();
        // Readable legacy configuration (no security, no ca_cert) is NOT
        // authorized operational transport.
        let err = match resolve_conn_security_for_acquisition(&t, None).await {
            Ok(_) => panic!("legacy plaintext must be migration_required"),
            Err(e) => e,
        };
        assert!(
            format!("{err:#}").contains("migration_required"),
            "unexpected error: {err:#}"
        );
        // Deliberate laboratory opt-in authorizes cleartext.
        t.security = Some(GnmiSecurityMode::Plaintext);
        let conn = resolve_conn_security_for_acquisition(&t, None)
            .await
            .unwrap();
        assert!(conn.effective.authorized_for_acquisition());
        assert_eq!(conn.scheme(), "http");
        // Readable resolution still succeeds for status/migration views.
        let mut legacy = target();
        legacy.ca_cert = None;
        let readable = resolve_conn_security(&legacy, None).await.unwrap();
        assert!(!readable.effective.authorized_for_acquisition());
    }

    #[tokio::test]
    async fn tls_modes_never_downgrade_to_unauthorized_plaintext() {
        let mut t = target();
        t.security = Some(GnmiSecurityMode::TlsVerify);
        t.tls_domain = Some("device.example".to_string());
        // Missing CA material: hard error, never plaintext.
        assert!(
            resolve_conn_security_for_acquisition(&t, None)
                .await
                .is_err()
        );
        t.security = Some(GnmiSecurityMode::Mtls);
        t.client_cert = Some("client.pem".to_string());
        t.client_key = Some("client.key".to_string());
        // Collector-local material absent → fail closed (A5).
        assert!(
            resolve_conn_security_for_acquisition(&t, None)
                .await
                .is_err()
        );
    }
}
