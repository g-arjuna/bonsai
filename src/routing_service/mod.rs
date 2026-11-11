//! B10–B13: Reduced Bonsai routing service, versioned internal API (`/internal/routing/v1/*`),
//! public proxy handlers (`/api/routing/v1/*`), and checkpointed graph summary projector.
//!
//! Runs without Kafka, NATS, or external ML sidecars, using `rbmp-core`, `rbmp-rib`,
//! and `rbmp-store` as the single canonical BMP/BGP protocol and history engine.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::telemetry_identity::{
    DEFAULT_IDENTITY_SCOPE, IdentityResolver, IdentityStatus, IngressAuthMethod,
    TelemetrySourceIdentity, VerifiedIngressPrincipal, identity_from_bmp_observation,
};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use rbmp_core::bgp::types::{AfiSafi, BgpDecodeContext, FamilySupportStatus};
use rbmp_core::bmp::parser::{
    DEFAULT_MAX_FRAME, parse_bmp_message_with_context, parse_peer_header, read_bmp_frame,
};
use rbmp_core::bmp::types::{BmpMessage, BmpPayload, PeerKey, RibType, SpeakerKey};
use rbmp_rib::RibManager;
use rbmp_rib::event::{RibEventPayload, RouteAction};
use rbmp_rib::session::PeerState;
use rbmp_store::{BmpJournal, PrefixHistoryItem, RibEventJournal};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite, BufReader, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;

pub const ROUTING_API_VERSION: &str = "v1";
pub const ROUTING_CHECKPOINT_CONFIG_CLASS: &str = "routing_checkpoint";
pub const ROUTING_CHECKPOINT_ID: &str = "routing-projection-checkpoint-v1";
const ROUTING_PROJECTION_VERSION: u64 = 1;

/// Configuration for the reduced routing process (`bonsai-routing`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingServiceConfig {
    pub bmp_listen_addr: String,
    pub http_listen_addr: String,
    pub bmp_journal_path: Option<PathBuf>,
    pub event_journal_path: Option<PathBuf>,
    /// Optional explicit path to the management service's
    /// `bonsai_config.db` holding trusted exporter bindings (BN-021b).
    /// When unset, the routing service looks for `bonsai_config.db`
    /// beside the configured journals (packaged deployments share the
    /// runtime volume) and uses it only if already present.
    #[serde(default)]
    pub identity_db_path: Option<PathBuf>,
    /// Trusted ingress/collector identity scope this listener resolves
    /// BMP exporters under (BN-021c). `None`/`"default"` resolves against
    /// the default identity namespace only. Set this to a configured
    /// site/ingress scope so site-scoped trusted bindings become
    /// reachable; it is NEVER derived from self-reported BMP sysName.
    #[serde(default)]
    pub identity_scope: Option<String>,
    /// Identity resolution policy for the BMP listener (BN-021f).
    /// Defaults to [`BmpIdentityMode::Verified`] — the secure mode.
    #[serde(default)]
    pub bmp_identity_mode: BmpIdentityMode,
    /// Optional mutual-TLS termination for the BMP listener (BN-021f).
    /// When set, every BMP connection must complete a client-certificate
    /// handshake against `client_ca_path`; the verified credential
    /// fingerprint feeds authoritative identity resolution.
    #[serde(default)]
    pub bmp_mtls: Option<BmpMtlsConfig>,
    pub max_frame_bytes: u32,
    #[serde(skip_serializing)]
    pub internal_service_token: Option<String>,
}

impl Default for RoutingServiceConfig {
    fn default() -> Self {
        Self {
            bmp_listen_addr: std::env::var("BONSAI_ROUTING_BMP_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:11019".to_string()),
            http_listen_addr: std::env::var("BONSAI_ROUTING_HTTP_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:8081".to_string()),
            bmp_journal_path: std::env::var("BONSAI_ROUTING_BMP_JOURNAL")
                .ok()
                .map(PathBuf::from),
            event_journal_path: std::env::var("BONSAI_ROUTING_EVENT_JOURNAL")
                .ok()
                .map(PathBuf::from),
            identity_db_path: std::env::var("BONSAI_ROUTING_IDENTITY_DB")
                .ok()
                .map(PathBuf::from),
            identity_scope: std::env::var("BONSAI_ROUTING_IDENTITY_SCOPE")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            bmp_identity_mode: match std::env::var("BONSAI_ROUTING_BMP_IDENTITY_MODE")
                .ok()
                .as_deref()
            {
                Some("claim_compat") | Some("claim-compatible") => BmpIdentityMode::ClaimCompatible,
                _ => BmpIdentityMode::Verified,
            },
            bmp_mtls: {
                // Empty values (e.g. `${VAR:-}` compose expansion) count
                // as unset — a partially-empty TLS configuration must
                // surface as "no mTLS", never as a broken acceptor.
                let env_path = |name: &str| {
                    std::env::var(name)
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from)
                };
                let cert = env_path("BONSAI_ROUTING_BMP_TLS_CERT");
                let key = env_path("BONSAI_ROUTING_BMP_TLS_KEY");
                let ca = env_path("BONSAI_ROUTING_BMP_TLS_CLIENT_CA");
                let paths = [
                    cert.as_ref().map(PathBuf::from),
                    key.as_ref().map(PathBuf::from),
                    ca.as_ref().map(PathBuf::from),
                ];
                if paths.iter().all(Option::is_some) {
                    Some(BmpMtlsConfig {
                        cert_path: paths[0].clone().unwrap(),
                        key_path: paths[1].clone().unwrap(),
                        client_ca_path: paths[2].clone().unwrap(),
                    })
                } else if paths.iter().all(Option::is_none) {
                    None
                } else {
                    // Partial configuration is a deployment error;
                    // surface it via validate() with empty paths.
                    Some(BmpMtlsConfig {
                        cert_path: paths[0].clone().unwrap_or_default(),
                        key_path: paths[1].clone().unwrap_or_default(),
                        client_ca_path: paths[2].clone().unwrap_or_default(),
                    })
                }
            },
            max_frame_bytes: DEFAULT_MAX_FRAME,
            internal_service_token: std::env::var("BONSAI_ROUTING_SERVICE_TOKEN")
                .ok()
                .filter(|s| !s.trim().is_empty()),
        }
    }
}

/// BMP listener identity-resolution policy (BN-021f).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BmpIdentityMode {
    /// Secure default: canonical `vexp:*` attribution requires a
    /// transport-verified principal (mutually-authenticated TLS client
    /// certificate) mapped to a credential-pinned trusted binding.
    /// Unauthenticated/plaintext connections are always
    /// connection-isolated and never receive device attribution.
    /// Historical claim-based `exp:*` journal records remain readable
    /// but can never be joined by new connections.
    #[default]
    Verified,
    /// Explicitly UNSAFE compatibility mode retaining BN-021c
    /// claim-based resolution: self-reported BMP sysName claims may
    /// promote connections into `exp:*` scopes. Never enable for
    /// production attribution — a name match is not ownership proof.
    ClaimCompatible,
}

/// Mutual-TLS termination settings for the BMP listener (BN-021f).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BmpMtlsConfig {
    /// PEM-encoded server certificate chain.
    pub cert_path: PathBuf,
    /// PEM-encoded server private key.
    pub key_path: PathBuf,
    /// PEM file with the trusted client-CA certificate(s) used to
    /// verify exporter client certificates.
    pub client_ca_path: PathBuf,
}

impl RoutingServiceConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.bmp_listen_addr.trim().is_empty() {
            return Err("bmp_listen_addr must not be empty".to_string());
        }
        if self.http_listen_addr.trim().is_empty() {
            return Err("http_listen_addr must not be empty".to_string());
        }
        if self.max_frame_bytes < 6 || self.max_frame_bytes > 16 * 1024 * 1024 {
            return Err(format!(
                "max_frame_bytes ({}) must be between 6 and 16777216",
                self.max_frame_bytes
            ));
        }
        if let Some(mtls) = &self.bmp_mtls {
            for (name, p) in [
                ("cert_path", &mtls.cert_path),
                ("key_path", &mtls.key_path),
                ("client_ca_path", &mtls.client_ca_path),
            ] {
                if p.as_os_str().is_empty() {
                    return Err(format!(
                        "bmp_mtls.{name} is required when BMP mTLS is configured \
                         (set BONSAI_ROUTING_BMP_TLS_CERT/_KEY/_CLIENT_CA together)"
                    ));
                }
            }
        }
        Ok(())
    }

    /// Validates production deployment settings, enforcing fail-closed
    /// security for binaries and services. Reads the explicit dev
    /// override environment variables once, then delegates to
    /// [`Self::validate_deployment_with`].
    pub fn validate_deployment(&self) -> Result<(), String> {
        self.validate_deployment_with(&DeploymentSecurityOverrides::from_env())
    }

    /// Deployment validation against explicit security overrides.
    /// Split from the environment read so tests can exercise every
    /// policy branch deterministically.
    pub fn validate_deployment_with(
        &self,
        overrides: &DeploymentSecurityOverrides,
    ) -> Result<(), String> {
        self.validate()?;

        let allow_insecure = overrides.allow_insecure_no_token;
        let allow_ephemeral = overrides.allow_ephemeral;

        // Fail closed if internal token is missing
        if self
            .internal_service_token
            .as_ref()
            .is_none_or(|s| s.trim().is_empty())
            && !allow_insecure
        {
            return Err(
                "Missing internal_service_token: BONSAI_ROUTING_SERVICE_TOKEN must be set in production deployment (or explicitly set BONSAI_ROUTING_ALLOW_INSECURE_NO_TOKEN=1 for local development)".to_string(),
            );
        }

        // Fail closed if journals are unpersisted/ephemeral
        if (self.bmp_journal_path.is_none() || self.event_journal_path.is_none())
            && !allow_ephemeral
        {
            return Err(
                "Missing persistent journal paths: BONSAI_ROUTING_BMP_JOURNAL and BONSAI_ROUTING_EVENT_JOURNAL must be configured (or explicitly set BONSAI_ROUTING_ALLOW_EPHEMERAL=1 for ephemeral dev)".to_string(),
            );
        }

        // Parse addresses rather than classifying attacker-controlled hostname prefixes.
        let http_addr: std::net::SocketAddr = self
            .http_listen_addr
            .parse()
            .map_err(|_| "HTTP address must be an IP socket address".to_string())?;
        self.bmp_listen_addr
            .parse::<std::net::SocketAddr>()
            .map_err(|_| "BMP address must be an IP socket address".to_string())?;
        if !http_addr.ip().is_loopback()
            && self
                .internal_service_token
                .as_ref()
                .is_none_or(|s| s.trim().is_empty())
        {
            return Err("Refusing non-loopback HTTP without internal_service_token".into());
        }
        if let Some(token) = &self.internal_service_token {
            if token.is_empty() || !token.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
                return Err(
                    "internal_service_token must contain only visible ASCII without whitespace"
                        .into(),
                );
            }
        }
        if let (Some(bmp), Some(events)) = (&self.bmp_journal_path, &self.event_journal_path) {
            // Resolve existing ancestors too, so symlink and ../ aliases cannot share a file.
            fn resolved(path: &std::path::Path) -> std::io::Result<PathBuf> {
                if path.exists() {
                    return path.canonicalize();
                }
                let parent = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(std::path::Path::new("."));
                Ok(resolved(parent)?.join(
                    path.file_name()
                        .ok_or_else(|| std::io::Error::other("journal path has no file name"))?,
                ))
            }
            if bmp.as_os_str().is_empty()
                || events.as_os_str().is_empty()
                || resolved(bmp).map_err(|e| e.to_string())?
                    == resolved(events).map_err(|e| e.to_string())?
            {
                return Err("BMP and event journals must use distinct nonempty paths".into());
            }
            #[cfg(unix)]
            if bmp.exists() && events.exists() {
                use std::os::unix::fs::MetadataExt;
                let a = std::fs::metadata(bmp).map_err(|e| e.to_string())?;
                let b = std::fs::metadata(events).map_err(|e| e.to_string())?;
                if a.dev() == b.dev() && a.ino() == b.ino() {
                    return Err("BMP and event journals must not be hard links to one file".into());
                }
            }
        }

        // ── BMP ingress production policy (BN-021f hardening) ──
        //
        // Verified mode alone is not sufficient: without mTLS there is
        // no verified principal, so every connection is permanently
        // connection-isolated. That is safe but almost certainly not
        // the intended deployment, so an unauthenticated BMP listener
        // on a non-loopback address must be an explicit choice.
        if self.bmp_identity_mode == BmpIdentityMode::ClaimCompatible
            && !overrides.allow_claim_compatible
        {
            return Err(
                "BONSAI_ROUTING_BMP_IDENTITY_MODE=claim_compat disables verified exporter \
                 attribution (self-reported sysName may claim canonical identity) and is \
                 unsafe for production; set BONSAI_ROUTING_ALLOW_CLAIM_COMPATIBLE=1 or \
                 BONSAI_ALLOW_INSECURE_DEV=1 to acknowledge the risk explicitly"
                    .to_string(),
            );
        }
        match &self.bmp_mtls {
            Some(mtls) => {
                // Fail fast at startup rather than at first connection:
                // the certificate chain, key and client CA must all
                // parse and the verifier must build.
                build_bmp_mtls_acceptor(mtls)
                    .map_err(|e| format!("invalid BMP mutual-TLS configuration: {e}"))?;
            }
            None => {
                let bmp_loopback = self
                    .bmp_listen_addr
                    .parse::<std::net::SocketAddr>()
                    .map(|a| a.ip().is_loopback())
                    .unwrap_or(false);
                if !bmp_loopback && !overrides.allow_bmp_plaintext {
                    return Err(
                        "BMP listener is not loopback-only and has no mutual-TLS ingress \
                         (BONSAI_ROUTING_BMP_TLS_CERT/_KEY/_CLIENT_CA unset): verified \
                         exporter attribution requires authenticated transport. For \
                         plaintext development collection set \
                         BONSAI_ROUTING_ALLOW_BMP_PLAINTEXT=1 or \
                         BONSAI_ALLOW_INSECURE_DEV=1"
                            .to_string(),
                    );
                }
            }
        }

        Ok(())
    }
}

/// Explicit insecure-development opt-ins for
/// [`RoutingServiceConfig::validate_deployment_with`]. Each flag must
/// be set deliberately; they are never implied by each other except
/// that the umbrella `BONSAI_ALLOW_INSECURE_DEV`/`..._INSECURE_DEV`
/// variables enable all of them.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeploymentSecurityOverrides {
    /// `BONSAI_ROUTING_ALLOW_INSECURE_NO_TOKEN` or dev umbrella.
    pub allow_insecure_no_token: bool,
    /// `BONSAI_ROUTING_ALLOW_EPHEMERAL` or dev umbrella.
    pub allow_ephemeral: bool,
    /// `BONSAI_ALLOW_INSECURE_DEV`/`BONSAI_ROUTING_ALLOW_INSECURE_DEV`.
    pub allow_insecure_dev: bool,
    /// `BONSAI_ROUTING_ALLOW_BMP_PLAINTEXT` or dev umbrella: permits a
    /// non-loopback plaintext BMP listener (all connections remain
    /// non-authoritative under Verified mode).
    pub allow_bmp_plaintext: bool,
    /// `BONSAI_ROUTING_ALLOW_CLAIM_COMPATIBLE` or dev umbrella:
    /// permits `BmpIdentityMode::ClaimCompatible`.
    pub allow_claim_compatible: bool,
}

impl DeploymentSecurityOverrides {
    /// Read the override environment once.
    pub fn from_env() -> Self {
        let flag = |names: &[&str]| {
            names.iter().any(|n| {
                std::env::var(n)
                    .map(|v| v == "1" || v == "true")
                    .unwrap_or(false)
            })
        };
        let dev = flag(&[
            "BONSAI_ROUTING_ALLOW_INSECURE_DEV",
            "BONSAI_ALLOW_INSECURE_DEV",
        ]);
        Self {
            allow_insecure_no_token: flag(&["BONSAI_ROUTING_ALLOW_INSECURE_NO_TOKEN"]) || dev,
            allow_ephemeral: flag(&["BONSAI_ROUTING_ALLOW_EPHEMERAL"]) || dev,
            allow_insecure_dev: dev,
            allow_bmp_plaintext: flag(&["BONSAI_ROUTING_ALLOW_BMP_PLAINTEXT"]) || dev,
            allow_claim_compatible: flag(&["BONSAI_ROUTING_ALLOW_CLAIM_COMPATIBLE"]) || dev,
        }
    }
}

/// Transport loss metadata when a BMP TCP connection drops without an explicit BGP withdrawal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransportGapRecord {
    pub speaker_addr: IpAddr,
    /// Logical-exporter scope (`exp:*`/`conn:*`/legacy) the gap is
    /// fenced to; empty = legacy/unspecified.
    #[serde(default)]
    pub exporter_instance: String,
    #[serde(default)]
    pub session_epoch: Option<u64>,
    pub disconnected_at: DateTime<Utc>,
    pub reason: String,
}

/// Result of a durable transport-disconnect record attempt
/// (BN-021c review P0-2). Callers must only perform irreversible
/// retirement when the outcome proves the durable lifecycle record
/// exists (or none is needed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportDisconnectOutcome {
    /// Fenced as stale: a newer session epoch owns the speaker. Nothing
    /// was recorded and live state is untouched.
    Stale,
    /// No live speaker session exists for this scope (e.g. a clean BMP
    /// Termination was already applied). A transport gap would be
    /// meaningless — the session ended deliberately, not by transport
    /// loss — so nothing was journaled or recorded.
    NoActiveSession,
    /// The gap was durably appended to `BmpJournal` and recorded in
    /// memory. Safe to retire connection-scoped state.
    Recorded,
    /// The durable journal append failed. In-memory/RIB state was left
    /// untouched so the live view stays consistent with what replay
    /// will reconstruct; the failure is logged at error level. Callers
    /// must NOT retire connection-scoped state — doing so would create
    /// a live/replay divergence (replay sees the earlier route
    /// observations but no disconnect record).
    PersistenceFailed,
}

/// Logical-exporter attribution applied to one BMP speaker scope at
/// ingest. `exporter_instance` partitions all derived state; the
/// `exporter_id`/`device_id` fields carry trusted-binding resolution.
#[derive(Debug, Clone, Default)]
pub struct ExporterScope {
    /// `exp:{canonical_exporter_id}` (namespace-encoded for non-default
    /// identity scopes) when resolved, `conn:{id}` for isolated
    /// connection-scoped exporters, `""` for legacy/shared.
    pub exporter_instance: String,
    pub exporter_id: Option<String>,
    pub device_id: Option<String>,
}

impl ExporterScope {
    /// Legacy/unspecified scope: all messages for a transport address
    /// share speaker instance 0 (pre-BN-021c behavior).
    pub fn legacy() -> Self {
        Self::default()
    }

    pub fn speaker_key(&self, speaker_addr: IpAddr) -> SpeakerKey {
        SpeakerKey::new(speaker_addr, &self.exporter_instance)
    }
}

/// Percent-encode one component of a composite exporter-instance
/// identifier. Only `[A-Za-z0-9._-]` pass through unchanged, so typical
/// canonical exporter ids and scope names keep their historical
/// human-readable form while `:`/`%`/whitespace can never make two
/// distinct `(scope, canonical)` pairs serialize identically.
fn encode_exporter_component(component: &str) -> String {
    let mut out = String::with_capacity(component.len());
    for b in component.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-' => out.push(b as char),
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// Build the `exp:*` logical-exporter scope for a resolved canonical
/// exporter id. Canonical exporter ids are unique per (scope, protocol),
/// so the speaker partition must carry the identity namespace: the
/// default scope keeps the historical `exp:{id}` form (journal and
/// checkpoint continuity), while any other scope is encoded as
/// `exp:{scope}:{id}` so site-scoped bindings can never collide with
/// each other or with the default namespace behind a shared NAT address.
/// Both components are percent-encoded so separators inside operator-
/// or binding-provided strings cannot produce ambiguous composite ids.
pub fn resolved_exporter_instance(identity_scope: &str, canonical: &str) -> String {
    if identity_scope == DEFAULT_IDENTITY_SCOPE {
        format!("exp:{}", encode_exporter_component(canonical))
    } else {
        format!(
            "exp:{}:{}",
            encode_exporter_component(identity_scope),
            encode_exporter_component(canonical)
        )
    }
}

/// Build the `vexp:*` logical-exporter scope for a VERIFIED-ingress
/// resolution (BN-021f). The distinct `vexp:` namespace is deliberate
/// provenance fencing: `exp:*` records in existing journals were
/// attributed from self-reported claims and must never be joined or
/// contaminated by authenticated state. Verified and claim-based
/// resolutions therefore occupy disjoint speaker namespaces; a
/// deployment enabling verified mode on top of claim-era journals
/// keeps the historical speakers frozen as legacy last-known state
/// (operators should archive/reset journals when cutting over).
pub fn verified_exporter_instance(identity_scope: &str, canonical: &str) -> String {
    if identity_scope == DEFAULT_IDENTITY_SCOPE {
        format!("vexp:{}", encode_exporter_component(canonical))
    } else {
        format!(
            "vexp:{}:{}",
            encode_exporter_component(identity_scope),
            encode_exporter_component(canonical)
        )
    }
}

/// Build the mutual-TLS acceptor for the BMP listener (BN-021f).
/// Client certificates are mandatory: the verifier is built from
/// `client_ca_path` and rustls rejects handshakes without a certificate
/// chaining to it. Shared by the `bonsai-routing` binary and tests.
pub fn build_bmp_mtls_acceptor(cfg: &BmpMtlsConfig) -> anyhow::Result<TlsAcceptor> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    let cert_pem = std::fs::read(&cfg.cert_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to read BMP TLS cert '{}': {e}",
            cfg.cert_path.display()
        )
    })?;
    let key_pem = std::fs::read(&cfg.key_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to read BMP TLS key '{}': {e}",
            cfg.key_path.display()
        )
    })?;
    let ca_pem = std::fs::read(&cfg.client_ca_path).map_err(|e| {
        anyhow::anyhow!(
            "failed to read BMP client CA '{}': {e}",
            cfg.client_ca_path.display()
        )
    })?;

    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("failed to parse BMP TLS certificate PEM: {e}"))?;
    if certs.is_empty() {
        anyhow::bail!("no certificates found in BMP TLS cert file");
    }
    let key = rustls_pemfile::private_key(&mut key_pem.as_slice())
        .map_err(|e| anyhow::anyhow!("failed to parse BMP TLS key PEM: {e}"))?
        .ok_or_else(|| anyhow::anyhow!("no private key found in BMP TLS key file"))?;

    let mut roots = rustls::RootCertStore::empty();
    let ca_certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut ca_pem.as_slice())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("failed to parse BMP client CA PEM: {e}"))?;
    if ca_certs.is_empty() {
        anyhow::bail!("no CA certificates found in BMP client CA file");
    }
    let (added, _ignored) = roots.add_parsable_certificates(ca_certs);
    if added == 0 {
        anyhow::bail!("no usable BMP client CA certificates parsed");
    }

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        provider.clone(),
    )
    .build()
    .map_err(|e| anyhow::anyhow!("failed to build BMP client-cert verifier: {e}"))?;

    let server_config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| anyhow::anyhow!("invalid BMP TLS protocol versions: {e}"))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, PrivateKeyDer::try_from(key)?)
        .map_err(|e| anyhow::anyhow!("failed to build BMP mTLS ServerConfig: {e}"))?;

    Ok(TlsAcceptor::from(Arc::new(server_config)))
}

/// Outcome of resolving a BMP connection's Initiation evidence against
/// the trusted binding store.
#[derive(Debug, Clone)]
pub struct SpeakerResolution {
    pub scope: ExporterScope,
    /// `resolved` | `provisional` | `ambiguous` | `unknown` | `conflicting`
    pub status: &'static str,
    pub binding_id: Option<String>,
    /// Present only when the resolution is authoritative (`Resolved`):
    /// everything the periodic revalidation loop needs to re-check that
    /// this session's authorization still holds (BN-021f hardening).
    pub authorization: Option<SessionAuthorization>,
}

/// Authorization evidence for one live authoritative BMP session.
/// Retained in memory only — never journaled — so the revalidation
/// loop can re-run the exact same resolution against fresh bindings.
#[derive(Debug, Clone)]
pub struct SessionAuthorization {
    pub binding_id: String,
    pub canonical_exporter_id: String,
    pub device_id: Option<String>,
    /// Verified credential fingerprint for verified-ingress sessions;
    /// `None` for claim-compatible sessions.
    pub credential_fingerprint: Option<String>,
    /// The observed evidence envelope used for the original
    /// resolution, replayed verbatim on revalidation.
    pub observed: TelemetrySourceIdentity,
    /// The transport-attested principal (verified mode only).
    pub principal: Option<VerifiedIngressPrincipal>,
}

/// Shared state of the reduced routing service.
pub struct RoutingServiceState {
    projection_semantics_ready: bool,
    pub config: RoutingServiceConfig,
    pub rib: RwLock<RibManager>,
    pub bmp_journal: BmpJournal,
    pub event_journal: RibEventJournal,
    pub decode_contexts: RwLock<HashMap<PeerKey, BgpDecodeContext>>,
    pub transport_gaps: RwLock<HashMap<SpeakerKey, TransportGapRecord>>,
    /// Trusted exporter-binding store (BN-021b) when the management
    /// service's `bonsai_config.db` is reachable. Read-only here:
    /// telemetry can never create or modify bindings.
    pub identity_store: Option<Arc<crate::sqlite_store::SqliteStore>>,
    // Serialize raw observation order, sequence allocation and durable publication.
    ingestion_lock: tokio::sync::Mutex<()>,
}

impl RoutingServiceState {
    pub async fn new(config: RoutingServiceConfig) -> anyhow::Result<Arc<Self>> {
        config
            .validate()
            .map_err(|e| anyhow::anyhow!("Invalid RoutingServiceConfig: {e}"))?;

        let event_journal = RibEventJournal::open(config.event_journal_path.as_ref()).await?;
        let mut projection_semantics_ready = event_journal.events_covered_by(&[]).await;
        let (mut rib_mgr, _rx) = RibManager::new(4096);
        let mut recovered_contexts = HashMap::new();
        let mut recovered_gaps = HashMap::new();

        if let Some(ref bmp_path) = config.bmp_journal_path {
            if bmp_path.exists() {
                let outcome = BmpJournal::replay_into_with_state(bmp_path, &mut rib_mgr)?;
                projection_semantics_ready = event_journal
                    .events_covered_by(&outcome.emitted_events)
                    .await;
                recovered_contexts = outcome.decode_contexts;
                recovered_gaps = outcome
                    .transport_gaps
                    .into_iter()
                    .map(|(key, g)| {
                        (
                            key,
                            TransportGapRecord {
                                speaker_addr: g.speaker_addr,
                                exporter_instance: g.exporter_instance,
                                session_epoch: g.session_epoch,
                                disconnected_at: g.disconnected_at,
                                reason: g.reason,
                            },
                        )
                    })
                    .collect();

                event_journal
                    .enrich_replayed_eors(&outcome.emitted_events)
                    .await?;
                let existing_seqs = event_journal.existing_sequences().await;
                let missing_events: Vec<_> = outcome
                    .emitted_events
                    .into_iter()
                    .filter(|ev| !existing_seqs.contains(&ev.event_key.sequence))
                    .collect();
                if !missing_events.is_empty() {
                    event_journal.append_batch(&missing_events).await?;
                }
            }
        }

        let latest_seq = event_journal.latest_seq().await;
        rib_mgr.ensure_min_event_seq(latest_seq + 1);

        let bmp_journal = BmpJournal::open(config.bmp_journal_path.as_ref()).await?;
        let identity_store = Self::open_identity_store(&config);

        Ok(Arc::new(Self {
            projection_semantics_ready,
            config,
            rib: RwLock::new(rib_mgr),
            bmp_journal,
            event_journal,
            decode_contexts: RwLock::new(recovered_contexts),
            transport_gaps: RwLock::new(recovered_gaps),
            identity_store,
            ingestion_lock: tokio::sync::Mutex::new(()),
        }))
    }

    /// Open the trusted exporter-binding store. Explicit
    /// `identity_db_path` is opened unconditionally; otherwise the
    /// `bonsai_config.db` beside the configured journals is used only
    /// when it already exists (packaged deployments share the runtime
    /// volume between `bonsai` and `bonsai-routing`).
    fn open_identity_store(
        config: &RoutingServiceConfig,
    ) -> Option<Arc<crate::sqlite_store::SqliteStore>> {
        let db_path = match &config.identity_db_path {
            Some(p) => p.clone(),
            None => {
                let dir = [&config.event_journal_path, &config.bmp_journal_path]
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p.parent())
                    .find(|dir| dir.join(crate::sqlite_store::CONFIG_DB_FILE_NAME).exists())?;
                dir.join(crate::sqlite_store::CONFIG_DB_FILE_NAME)
            }
        };
        // Read-only open (BN-021c/P1): the routing service must never
        // migrate, create tables, or switch journal mode on the shared
        // management database. An older-schema DB simply yields no
        // bindings and exporters stay connection-isolated (fail closed).
        match crate::sqlite_store::SqliteStore::open_file_readonly(&db_path) {
            Ok(store) => match store.schema_version() {
                Ok(v) if v < crate::sqlite_store::CURRENT_SCHEMA_VERSION => {
                    tracing::warn!(
                        path = %db_path.display(),
                        schema_version = v,
                        required = crate::sqlite_store::CURRENT_SCHEMA_VERSION,
                        "identity DB schema is older than required; trusted bindings unavailable, exporters stay connection-isolated"
                    );
                    Some(Arc::new(store))
                }
                _ => {
                    tracing::info!(path = %db_path.display(), "trusted exporter identity store loaded");
                    Some(Arc::new(store))
                }
            },
            Err(e) => {
                tracing::warn!(path = %db_path.display(), error = %e,
                    "trusted exporter identity store unavailable; BMP exporters stay connection-isolated");
                None
            }
        }
    }

    /// Load the active trusted BMP exporter bindings. Returns an empty
    /// set when no identity store is configured — every exporter then
    /// stays connection-isolated (fail closed).
    fn trusted_bmp_bindings(&self) -> Vec<crate::telemetry_identity::TrustedBinding> {
        self.try_trusted_bmp_bindings().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to load trusted exporter bindings");
            Vec::new()
        })
    }

    /// Fallible variant of [`Self::trusted_bmp_bindings`] for the
    /// revocation revalidation loop, which must distinguish "no
    /// bindings" from "store unreadable" (fail closed vs. tolerate).
    fn try_trusted_bmp_bindings(
        &self,
    ) -> Result<Vec<crate::telemetry_identity::TrustedBinding>, String> {
        let Some(store) = &self.identity_store else {
            return Ok(Vec::new());
        };
        store
            .list_exporter_bindings(None, Some("bmp"), false)
            .map_err(|e| e.to_string())
    }

    /// Resolve a BMP connection's Initiation evidence against trusted
    /// bindings.
    ///
    /// `verified` is the transport-authenticated principal established
    /// by the ingress layer (mTLS client certificate, BN-021f) — `None`
    /// for plaintext/unauthenticated transports. It is the ONLY input
    /// allowed to produce canonical attribution; BMP Initiation fields
    /// remain self-reported evidence.
    ///
    /// - [`BmpIdentityMode::Verified`] (default): only
    ///   `resolve_verified_ingress` may produce authoritative state —
    ///   a credential-pinned binding must match the verified
    ///   fingerprint, and the resolved scope is the provenance-fenced
    ///   `vexp:*` namespace. Everything else gets an isolated
    ///   `conn:{id}` scope with `device_id = None`.
    /// - [`BmpIdentityMode::ClaimCompatible`] (explicitly unsafe):
    ///   BN-021c claim-based resolution — `Resolved` → `exp:*` scope,
    ///   everything else → `conn:*` isolation.
    /// Resolve a BMP connection with no transport-verified principal
    /// (plaintext/unauthenticated path). Equivalent to
    /// [`Self::resolve_bmp_exporter_with_ingress`] with `verified = None`.
    pub fn resolve_bmp_exporter(
        &self,
        speaker_addr: IpAddr,
        connection_id: &str,
        sys_name: Option<&str>,
        sys_descr: Option<&str>,
    ) -> SpeakerResolution {
        self.resolve_bmp_exporter_with_ingress(
            speaker_addr,
            connection_id,
            sys_name,
            sys_descr,
            None,
        )
    }

    pub fn resolve_bmp_exporter_with_ingress(
        &self,
        speaker_addr: IpAddr,
        connection_id: &str,
        sys_name: Option<&str>,
        sys_descr: Option<&str>,
        verified: Option<&VerifiedIngressPrincipal>,
    ) -> SpeakerResolution {
        let provisional_claim = sys_name
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|n| format!("bmp:{n}"))
            .unwrap_or_else(|| speaker_addr.to_string());
        let probe = rbmp_core::bmp::types::ObservationSource::with_ids(
            "default",
            provisional_claim.clone(),
            speaker_addr,
            "pending",
            0,
        );
        // The scope is the operator-configured trusted ingress scope —
        // never inferred from the self-reported sysName (BN-021c/P0-D).
        let mut observed =
            identity_from_bmp_observation(&probe, sys_name, self.config.identity_scope.clone());
        let _ = sys_descr;
        let bindings = self.trusted_bmp_bindings();

        if self.config.bmp_identity_mode == BmpIdentityMode::Verified {
            // ── BN-021f verified-ingress resolution ──
            let Some(principal) = verified else {
                // Plaintext/unauthenticated transport: never authoritative.
                return SpeakerResolution {
                    scope: ExporterScope {
                        exporter_instance: format!("conn:{connection_id}"),
                        exporter_id: sys_name
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(|n| format!("bmp:{n}")),
                        device_id: None,
                    },
                    status: "unauthenticated",
                    binding_id: None,
                    authorization: None,
                };
            };
            observed = observed.with_verified_ingress(principal.clone());
            let resolution =
                IdentityResolver::resolve_verified_ingress(&observed, &bindings, principal);
            return match resolution.status {
                crate::telemetry_identity::IdentityStatus::Resolved => {
                    let canonical = resolution
                        .canonical_exporter_id
                        .clone()
                        .unwrap_or_else(|| provisional_claim.clone());
                    SpeakerResolution {
                        scope: ExporterScope {
                            exporter_instance: verified_exporter_instance(
                                observed.resolution_scope(),
                                &canonical,
                            ),
                            exporter_id: Some(canonical.clone()),
                            device_id: resolution.device_id.clone(),
                        },
                        status: "resolved",
                        binding_id: resolution.binding_id.clone(),
                        authorization: Some(SessionAuthorization {
                            binding_id: resolution.binding_id.clone().unwrap_or_default(),
                            canonical_exporter_id: canonical,
                            device_id: resolution.device_id.clone(),
                            credential_fingerprint: Some(
                                principal.credential_fingerprint().to_string(),
                            ),
                            observed: observed.clone(),
                            principal: Some(principal.clone()),
                        }),
                    }
                }
                status => {
                    if !matches!(
                        status,
                        crate::telemetry_identity::IdentityStatus::Provisional
                            | crate::telemetry_identity::IdentityStatus::Unknown
                    ) {
                        tracing::warn!(
                            %speaker_addr,
                            ?status,
                            credential = %principal.credential_fingerprint(),
                            "verified BMP credential failed binding resolution; isolating connection scope"
                        );
                    }
                    SpeakerResolution {
                        scope: ExporterScope {
                            exporter_instance: format!("conn:{connection_id}"),
                            exporter_id: sys_name
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(|n| format!("bmp:{n}")),
                            device_id: None,
                        },
                        status: match status {
                            crate::telemetry_identity::IdentityStatus::Ambiguous => "ambiguous",
                            crate::telemetry_identity::IdentityStatus::Conflicting => "conflicting",
                            crate::telemetry_identity::IdentityStatus::Provisional => "provisional",
                            _ => "unknown",
                        },
                        binding_id: None,
                        authorization: None,
                    }
                }
            };
        }

        // ── BN-021c claim-compatible resolution (explicitly unsafe) ──
        let resolution = IdentityResolver::resolve(&observed, &bindings);

        match resolution.status {
            crate::telemetry_identity::IdentityStatus::Resolved => {
                let canonical = resolution
                    .canonical_exporter_id
                    .clone()
                    .unwrap_or_else(|| provisional_claim.clone());
                SpeakerResolution {
                    scope: ExporterScope {
                        // Canonical exporter ids are unique per
                        // (scope, protocol) — NOT globally. The speaker
                        // scope must encode that namespace or two sites
                        // reusing the same exporter id behind one NAT
                        // address would collide into one SpeakerKey.
                        // The default scope keeps the legacy `exp:{id}`
                        // form so pre-existing journals/checkpoints stay
                        // continuous; any other scope is encoded.
                        exporter_instance: resolved_exporter_instance(
                            observed.resolution_scope(),
                            &canonical,
                        ),
                        exporter_id: Some(canonical.clone()),
                        device_id: resolution.device_id.clone(),
                    },
                    status: "resolved",
                    binding_id: resolution.binding_id.clone(),
                    authorization: Some(SessionAuthorization {
                        binding_id: resolution.binding_id.clone().unwrap_or_default(),
                        canonical_exporter_id: canonical,
                        device_id: resolution.device_id.clone(),
                        credential_fingerprint: None,
                        observed: observed.clone(),
                        principal: None,
                    }),
                }
            }
            status => {
                if !matches!(
                    status,
                    crate::telemetry_identity::IdentityStatus::Provisional
                        | crate::telemetry_identity::IdentityStatus::Unknown
                ) {
                    tracing::warn!(
                        %speaker_addr,
                        ?status,
                        "BMP exporter identity ambiguous/conflicting; isolating connection scope"
                    );
                }
                SpeakerResolution {
                    scope: ExporterScope {
                        exporter_instance: format!("conn:{connection_id}"),
                        // Provisional protocol claim stays evidence-only.
                        exporter_id: sys_name
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(|n| format!("bmp:{n}")),
                        device_id: None,
                    },
                    status: match status {
                        crate::telemetry_identity::IdentityStatus::Ambiguous => "ambiguous",
                        crate::telemetry_identity::IdentityStatus::Conflicting => "conflicting",
                        crate::telemetry_identity::IdentityStatus::Provisional => "provisional",
                        _ => "unknown",
                    },
                    binding_id: None,
                    authorization: None,
                }
            }
        }
    }

    /// Decode a raw BMP wire frame using session-aware capabilities when available,
    /// journal the accepted observation (including wire bytes and connection epoch),
    /// apply to `RibManager`, and journal emitted `RibEvent`s.
    ///
    /// Legacy entry point: attributes the frame to the unspecified
    /// exporter scope (speaker instance 0) — pre-BN-021c behavior.
    pub async fn ingest_bmp_bytes(
        &self,
        speaker_addr: IpAddr,
        session_epoch: Option<u64>,
        frame: &[u8],
    ) -> anyhow::Result<BmpMessage> {
        self.ingest_bmp_bytes_scoped(speaker_addr, &ExporterScope::legacy(), session_epoch, frame)
            .await
    }

    /// Decode and ingest a raw BMP wire frame under an explicit
    /// logical-exporter [`ExporterScope`]. The scope is stamped onto the
    /// journaled `BmpMessage` so replay reconstructs identical
    /// speaker/peer/RIB partitioning.
    pub async fn ingest_bmp_bytes_scoped(
        &self,
        speaker_addr: IpAddr,
        scope: &ExporterScope,
        session_epoch: Option<u64>,
        frame: &[u8],
    ) -> anyhow::Result<BmpMessage> {
        // Determine session-aware decode context if this is a RouteMonitoring (type 0) frame
        let ctx = if frame.len() >= 48 && frame[0] == 3 && frame[5] == 0 {
            if let Ok((peer_hdr, _)) = parse_peer_header(&frame[6..]) {
                let peer_key =
                    PeerKey::from_header(speaker_addr, &scope.exporter_instance, &peer_hdr);
                let contexts = self.decode_contexts.read().await;
                contexts
                    .get(&peer_key)
                    .cloned()
                    .unwrap_or_else(|| BgpDecodeContext::new(peer_hdr.peer_flags.four_byte_asn()))
            } else {
                BgpDecodeContext::default()
            }
        } else {
            BgpDecodeContext::default()
        };

        let payload =
            parse_bmp_message_with_context(frame, speaker_addr, self.config.max_frame_bytes, &ctx)?;
        let msg = BmpMessage {
            id: Uuid::new_v4(),
            received_at: Utc::now(),
            speaker_addr,
            exporter_instance: scope.exporter_instance.clone(),
            exporter_id: scope.exporter_id.clone(),
            device_id: scope.device_id.clone(),
            payload,
        };
        self.ingest_bmp_message_internal(msg, session_epoch, Some(frame))
            .await
    }

    /// Persist a parsed `BmpMessage` to the raw journal, process through `RibManager`,
    /// and persist all emitted `RibEvent`s directly to `RibEventJournal`.
    pub async fn ingest_bmp_message(
        &self,
        msg: BmpMessage,
        session_epoch: Option<u64>,
    ) -> anyhow::Result<BmpMessage> {
        self.ingest_bmp_message_internal(msg, session_epoch, None)
            .await
    }

    async fn ingest_bmp_message_internal(
        &self,
        msg: BmpMessage,
        session_epoch: Option<u64>,
        raw_frame: Option<&[u8]>,
    ) -> anyhow::Result<BmpMessage> {
        let _ingestion = self.ingestion_lock.lock().await;
        self.bmp_journal
            .append_observation(&msg, session_epoch, raw_frame)
            .await?;

        // Update session-aware decode context on PeerUp per RFC 7911 Section 5
        if let BmpPayload::PeerUp(ref pu) = msg.payload {
            let peer_key =
                PeerKey::from_header(msg.speaker_addr, &msg.exporter_instance, &pu.peer_header);
            let ctx = BgpDecodeContext::from_negotiated_open(
                pu.peer_header.peer_flags.four_byte_asn(),
                &pu.sent_open.capabilities,
                &pu.recv_open.capabilities,
            );
            self.decode_contexts.write().await.insert(peer_key, ctx);
        }

        let speaker_key = SpeakerKey::new(msg.speaker_addr, &msg.exporter_instance);
        let (emitted, effective_epoch) = {
            let mut rib = self.rib.write().await;
            let current_ep =
                session_epoch.or_else(|| rib.speaker(speaker_key).map(|s| s.source.session_epoch));
            let events = rib.process_for_epoch(msg.clone(), session_epoch);
            let post_ep =
                current_ep.or_else(|| rib.speaker(speaker_key).map(|s| s.source.session_epoch));
            (events, post_ep)
        };

        // Epoch-aware transport gap clearing (R04): only clear a speaker's gap when a newer
        // session epoch establishes synchronization (Initiation / PeerUp) or on EndOfRib / Termination.
        {
            let mut gaps = self.transport_gaps.write().await;
            if let Some(existing_gap) = gaps.get(&speaker_key) {
                if rbmp_store::should_clear_transport_gap(
                    existing_gap.session_epoch,
                    effective_epoch,
                    &msg.payload,
                ) {
                    gaps.remove(&speaker_key);
                }
            }
        }

        self.event_journal.append_batch(&emitted).await?;

        Ok(msg)
    }

    /// Record BMP transport loss without fabricating BGP route withdrawals.
    /// Last-known routes remain in the RIB marked under a transport gap.
    /// Legacy entry point: unspecified exporter scope (instance 0).
    pub async fn record_transport_disconnect(
        &self,
        speaker_addr: IpAddr,
        reason: impl Into<String>,
    ) -> TransportDisconnectOutcome {
        self.record_transport_disconnect_scoped(
            speaker_addr,
            &ExporterScope::legacy(),
            None,
            reason,
        )
        .await
    }

    /// Legacy epoch-fenced disconnect entry point attributing the
    /// disconnect to the unspecified exporter scope.
    pub async fn record_transport_disconnect_for_epoch(
        &self,
        speaker_addr: IpAddr,
        session_epoch: Option<u64>,
        reason: impl Into<String>,
    ) -> TransportDisconnectOutcome {
        self.record_transport_disconnect_scoped(
            speaker_addr,
            &ExporterScope::legacy(),
            session_epoch,
            reason,
        )
        .await
    }

    /// Epoch-fenced BMP transport disconnect recorder (R04).
    /// Stale disconnects from an older TCP connection epoch (< active speaker epoch) are ignored,
    /// while active disconnects clear EoR markers and are persisted durably to `BmpJournal`.
    /// Scoped variant: the gap and EOR clearing are fenced to the
    /// speaker's logical-exporter instance so one exporter's disconnect
    /// can never touch another exporter's state behind a shared NAT IP.
    pub async fn record_transport_disconnect_scoped(
        &self,
        speaker_addr: IpAddr,
        scope: &ExporterScope,
        session_epoch: Option<u64>,
        reason: impl Into<String>,
    ) -> TransportDisconnectOutcome {
        self.record_transport_disconnect_for_key(
            scope.speaker_key(speaker_addr),
            &scope.exporter_instance,
            session_epoch,
            reason,
        )
        .await
    }

    /// Epoch-fenced disconnect recorder keyed by an explicit
    /// [`SpeakerKey`]. `exporter_instance` is the scope string recorded
    /// into the durable journal gap.
    ///
    /// Ordering guarantee (BN-021c review P0-2): the durable journal
    /// append happens BEFORE any in-memory mutation, and a failed
    /// append mutates nothing — the live view then matches what replay
    /// will reconstruct (route observations with no disconnect record).
    /// Callers must only retire connection-scoped state when the
    /// outcome is [`TransportDisconnectOutcome::Recorded`] or
    /// [`TransportDisconnectOutcome::NoActiveSession`].
    pub async fn record_transport_disconnect_for_key(
        &self,
        speaker_key: SpeakerKey,
        exporter_instance: &str,
        session_epoch: Option<u64>,
        reason: impl Into<String>,
    ) -> TransportDisconnectOutcome {
        let _ingestion = self.ingestion_lock.lock().await;
        let reason_str = reason.into();
        let effective_epoch = {
            let rib = self.rib.read().await;
            let speaker = rib.speaker(speaker_key);
            let active_epoch = speaker.map(|s| s.source.session_epoch);
            if let (Some(disc_ep), Some(act_ep)) = (session_epoch, active_epoch) {
                if disc_ep < act_ep {
                    tracing::debug!(
                        speaker = %speaker_key.speaker_addr,
                        instance = speaker_key.speaker_instance,
                        disc_ep,
                        act_ep,
                        "Ignoring stale transport disconnect from older session epoch"
                    );
                    return TransportDisconnectOutcome::Stale;
                }
            }
            // A scoped disconnect for a speaker that no longer exists
            // means the session already ended deliberately (clean BMP
            // Termination). Recording a transport gap here would pin
            // completeness to "gap" forever for a dead speaker and
            // diverge from replay, which has no speaker to gap either.
            // Legacy callers (no session epoch) keep the historical
            // behavior of recording the gap unconditionally.
            if session_epoch.is_some() && speaker.is_none() {
                return TransportDisconnectOutcome::NoActiveSession;
            }
            session_epoch.or(active_epoch)
        };

        let disconnected_at = Utc::now();
        let gap = TransportGapRecord {
            speaker_addr: speaker_key.speaker_addr,
            exporter_instance: exporter_instance.to_string(),
            session_epoch: effective_epoch,
            disconnected_at,
            reason: reason_str.clone(),
        };
        let journal_gap = rbmp_store::JournalTransportGap {
            speaker_addr: speaker_key.speaker_addr,
            exporter_instance: exporter_instance.to_string(),
            session_epoch: effective_epoch,
            disconnected_at,
            reason: reason_str,
        };
        if let Err(e) = self
            .bmp_journal
            .append_transport_disconnect(&journal_gap)
            .await
        {
            // Persistence failed: leave RIB EOR markers, the in-memory
            // gap map and conn:* retirement decisions untouched so the
            // live view stays identical to journal replay. This is an
            // operational durability error — surface it loudly.
            tracing::error!(
                speaker = %speaker_key.speaker_addr,
                instance = speaker_key.speaker_instance,
                error = %e,
                "BMP transport disconnect could not be durably journaled; \
                 live state left unchanged (no gap recorded, no retirement)"
            );
            return TransportDisconnectOutcome::PersistenceFailed;
        }
        self.rib.write().await.clear_speaker_eor(speaker_key);
        self.transport_gaps.write().await.insert(speaker_key, gap);
        TransportDisconnectOutcome::Recorded
    }

    /// Retire an isolated connection-scoped (`conn:*`) speaker after its
    /// TCP connection ended: drops its peer sessions and RIB tables
    /// without emitting withdrawals (the scope can never receive new
    /// data; durable history remains in the journals), prunes its
    /// session-aware decode contexts, and clears its transport gap — a
    /// retired scope holds no state under a gap, so keeping one would
    /// pin global completeness to `"gap"` forever. Context pruning and
    /// gap removal run even when the speaker was already removed (e.g.
    /// by a clean BMP Termination). Returns whether a speaker was
    /// retired. Canonical `exp:*` speakers are NEVER retired here —
    /// they keep last-known routes under the transport gap.
    pub async fn retire_connection_scoped_speaker(&self, speaker_key: SpeakerKey) -> bool {
        let mut rib = self.rib.write().await;
        let retired = rib.retire_speaker(speaker_key);
        drop(rib);
        self.transport_gaps.write().await.remove(&speaker_key);
        self.decode_contexts.write().await.retain(|k, _| {
            !(k.speaker_addr == speaker_key.speaker_addr
                && k.speaker_instance == speaker_key.speaker_instance)
        });
        retired
    }

    /// Overall data-quality completeness state across observed speakers/peers.
    pub async fn overall_completeness(&self) -> &'static str {
        let gaps = self.transport_gaps.read().await;
        if !gaps.is_empty() {
            return "gap";
        }
        let rib = self.rib.read().await;
        let speakers = rib.speakers();
        if speakers.is_empty() {
            return "syncing";
        }
        let mut any_peer = false;
        let mut all_complete = true;
        let mut any_gap = false;
        let mut any_stale = false;
        for spk in speakers {
            if spk.peers.is_empty() {
                all_complete = false;
                continue;
            }
            for peer in spk.peers.values() {
                any_peer = true;
                match peer.state {
                    PeerState::Down => {
                        if peer.llgr_active {
                            any_stale = true;
                        } else {
                            any_gap = true;
                        }
                        all_complete = false;
                    }
                    PeerState::Unknown => {
                        all_complete = false;
                    }
                    PeerState::Up => {
                        if peer.eor_families.is_empty() {
                            all_complete = false;
                        }
                    }
                }
            }
        }
        if any_gap {
            "gap"
        } else if any_stale {
            "stale"
        } else if !any_peer {
            "syncing"
        } else if all_complete {
            "complete_for_observed_view"
        } else {
            "syncing"
        }
    }
}

/// Per-connection BMP byte stream: plaintext TCP or a mutually-
/// authenticated TLS session (BN-021f). Both implement
/// `AsyncRead`/`AsyncWrite` so the frame loop is transport-agnostic.
enum BmpIo {
    Plain(TcpStream),
    Tls(tokio_rustls::server::TlsStream<TcpStream>),
}

impl AsyncRead for BmpIo {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            BmpIo::Plain(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            BmpIo::Tls(s) => std::pin::Pin::new(s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for BmpIo {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            BmpIo::Plain(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            BmpIo::Tls(s) => std::pin::Pin::new(s).poll_write(cx, buf),
        }
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            BmpIo::Plain(s) => std::pin::Pin::new(s).poll_flush(cx),
            BmpIo::Tls(s) => std::pin::Pin::new(s).poll_flush(cx),
        }
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            BmpIo::Plain(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            BmpIo::Tls(s) => std::pin::Pin::new(s).poll_shutdown(cx),
        }
    }
}

/// Extract the verified ingress principal from a completed mTLS
/// session: SHA-256 of the DER-encoded leaf client certificate. This is
/// the ONLY producer of [`VerifiedIngressPrincipal`] — it can never be
/// influenced by BMP payload content.
fn mtls_peer_principal(
    tls: &tokio_rustls::server::TlsStream<TcpStream>,
) -> Option<VerifiedIngressPrincipal> {
    let (_, conn) = tls.get_ref();
    let leaf = conn.peer_certificates()?.first()?;
    let digest = Sha256::digest(leaf.as_ref());
    Some(VerifiedIngressPrincipal::from_transport_attestation(
        IngressAuthMethod::MutualTls,
        format!("sha256:{digest:x}"),
    ))
}

/// Spawn a bounded BMP listener that feeds accepted frames into `RoutingServiceState`.
///
/// `mtls_acceptor`: when `Some`, every accepted TCP connection must
/// complete a mutual-TLS handshake (client certificate chaining to the
/// configured CA) before any BMP byte is read. The verified credential
/// fingerprint is passed to identity resolution; plaintext or
/// unauthenticated connections can never obtain canonical attribution
/// under [`BmpIdentityMode::Verified`].
///
/// - `Resolved` → the connection joins the stable `exp:{exporter_id}`
///   speaker scope. Connections positively associated with the same
///   logical exporter (e.g. FRR global + per-VRF BMP sessions) share
///   the speaker epoch exactly as before.
/// - Anything else → the connection gets an isolated `conn:{id}`
///   speaker scope that can never merge route, peer, session, decode or
///   gap state with another exporter behind the same NAT address.
///
/// Epoch fencing is per [`SpeakerKey`]: a stale connection cannot clear
/// a newer session, and a disconnect cannot touch another exporter's
/// EOR/completeness state. When the last connection of a `conn:*`
/// scope closes, its speaker state is retired (the scope can never
/// receive new data); `exp:*` speakers keep last-known routes under the
/// transport gap as before.
/// TLS handshake timeout for BMP mTLS connections
/// (`BONSAI_ROUTING_BMP_TLS_HANDSHAKE_TIMEOUT_SECS`, seconds, default 10).
fn bmp_tls_handshake_timeout() -> Duration {
    std::env::var("BONSAI_ROUTING_BMP_TLS_HANDSHAKE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(Duration::from_secs_f64)
        .unwrap_or(Duration::from_secs(10))
}

/// Maximum concurrent in-flight TLS handshakes
/// (`BONSAI_ROUTING_BMP_TLS_MAX_HANDSHAKES`, default 64, clamped 1..=4096).
/// Excess TCP connections are dropped predictably at accept time rather
/// than queued without bound.
fn bmp_tls_max_handshakes() -> usize {
    std::env::var("BONSAI_ROUTING_BMP_TLS_MAX_HANDSHAKES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(64)
        .clamp(1, 4096)
}

/// Interval between batched trusted-binding revalidations of live
/// authoritative BMP sessions (`BONSAI_ROUTING_BMP_RECHECK_INTERVAL_MS`,
/// milliseconds, default 5000, clamped 50..=3_600_000). `0` disables
/// active-session revocation (documented unsafe for production).
fn bmp_binding_recheck_interval() -> Duration {
    std::env::var("BONSAI_ROUTING_BMP_RECHECK_INTERVAL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|ms| {
            if ms == 0 {
                Duration::from_secs(86_400)
            } else {
                Duration::from_millis(ms.clamp(50, 3_600_000))
            }
        })
        .unwrap_or(Duration::from_secs(5))
}

/// Resolves only when the shutdown watch value becomes `true`. If the
/// sender is dropped without signalling (test fixtures do this while
/// expecting the listener to keep running), the future parks forever
/// instead of busy-looping on `RecvError`.
async fn wait_shutdown(mut rx: tokio::sync::watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// One live authoritative BMP session tracked for credential/binding
/// revalidation (BN-021f hardening). Holds identifiers and evidence
/// only — never key or certificate material.
#[derive(Debug)]
struct ActiveBmpSession {
    connection_id: String,
    speaker_key: SpeakerKey,
    session_epoch: u64,
    binding_id: String,
    canonical_exporter_id: String,
    device_id: Option<String>,
    credential_fingerprint: Option<String>,
    /// Evidence envelope replayed verbatim on revalidation.
    observed: TelemetrySourceIdentity,
    /// Transport-attested principal (verified mode only).
    principal: Option<VerifiedIngressPrincipal>,
    /// Signalled to terminate the session's ingestion loop.
    revoke_tx: tokio::sync::watch::Sender<bool>,
}

/// Shared per-listener state: service state plus the session/lifecycle
/// registries every connection task and the revalidation monitor use.
#[derive(Clone)]
struct BmpListenerShared {
    state: Arc<RoutingServiceState>,
    /// SpeakerKey -> (session_epoch, live connection count)
    live_sessions: Arc<tokio::sync::Mutex<HashMap<SpeakerKey, (u64, usize)>>>,
    /// SpeakerKey -> raw BMP Termination frame deferred while sibling
    /// connections share the speaker scope (BN-021c/P0-C).
    deferred_terminations: Arc<tokio::sync::Mutex<HashMap<SpeakerKey, Vec<u8>>>>,
    /// connection_id -> authorized session awaiting revalidation.
    active_sessions: Arc<tokio::sync::Mutex<HashMap<String, ActiveBmpSession>>>,
    shutdown: tokio::sync::watch::Receiver<bool>,
}

/// Periodically re-resolve every authorized BMP session against a
/// fresh binding snapshot. A session whose binding was revoked,
/// re-pinned, or otherwise no longer resolves to the SAME canonical
/// (binding, exporter, device) triple is signalled to terminate —
/// revocation never requires a service restart and never touches
/// unaffected exporters.
async fn bmp_binding_revalidation_loop(shared: BmpListenerShared) {
    let mut consecutive_failures: u32 = 0;
    loop {
        tokio::select! {
            _ = wait_shutdown(shared.shutdown.clone()) => break,
            _ = tokio::time::sleep(bmp_binding_recheck_interval()) => {}
        }
        let bindings = match shared.state.try_trusted_bmp_bindings() {
            Ok(b) => {
                consecutive_failures = 0;
                b
            }
            Err(e) => {
                consecutive_failures += 1;
                tracing::warn!(
                    error = %e,
                    consecutive_failures,
                    "BMP binding revalidation failed; authorized sessions retain authority until the store is readable again"
                );
                if consecutive_failures >= 3 {
                    // Fail closed: inability to establish that active
                    // credentials remain authorized must not grant
                    // indefinite authority. Three consecutive failures
                    // (~15s at the default interval) is the bounded
                    // tolerance for transient store unavailability.
                    let sessions = shared.active_sessions.lock().await;
                    for sess in sessions.values() {
                        tracing::warn!(
                            connection_id = %sess.connection_id,
                            binding_id = %sess.binding_id,
                            credential = ?sess.credential_fingerprint,
                            "revoking BMP session: identity store unreadable"
                        );
                        let _ = sess.revoke_tx.send(true);
                    }
                }
                continue;
            }
        };
        let sessions = shared.active_sessions.lock().await;
        for sess in sessions.values() {
            let resolution = match &sess.principal {
                Some(p) => IdentityResolver::resolve_verified_ingress(&sess.observed, &bindings, p),
                None => IdentityResolver::resolve(&sess.observed, &bindings),
            };
            let still_authorized = resolution.status == IdentityStatus::Resolved
                && resolution.binding_id.as_deref() == Some(sess.binding_id.as_str())
                && resolution.canonical_exporter_id.as_deref()
                    == Some(sess.canonical_exporter_id.as_str())
                && resolution.device_id == sess.device_id;
            if !still_authorized {
                tracing::warn!(
                    connection_id = %sess.connection_id,
                    speaker_key = ?sess.speaker_key,
                    session_epoch = sess.session_epoch,
                    binding_id = %sess.binding_id,
                    credential = ?sess.credential_fingerprint,
                    status = ?resolution.status,
                    "revoking BMP session: authorization no longer valid"
                );
                let _ = sess.revoke_tx.send(true);
            }
        }
    }
}

/// Spawn a bounded BMP listener that feeds accepted frames into `RoutingServiceState`.
///
/// `mtls_acceptor`: when `Some`, every accepted TCP connection must
/// complete a mutual-TLS handshake (client certificate chaining to the
/// configured CA) before any BMP byte is read. The verified credential
/// fingerprint is passed to identity resolution; plaintext or
/// unauthenticated connections can never obtain canonical attribution
/// under [`BmpIdentityMode::Verified`].
///
/// Handshakes run in per-connection tasks bounded by a semaphore
/// ([`bmp_tls_max_handshakes`]) with [`bmp_tls_handshake_timeout`]; the
/// accept loop never waits on an individual handshake, so stalled
/// clients cannot serialize legitimate exporters. Excess connections
/// beyond the handshake limit are dropped at accept time.
///
/// - `Resolved` → the connection joins the stable `vexp:{exporter_id}`
///   (verified) or `exp:{exporter_id}` (claim-compatible) speaker
///   scope. Verified connections presenting the SAME credential may
///   share the speaker (FRR global+VRF sessions); claim-resolved
///   scopes admit at most one live connection.
/// - Anything else → the connection gets an isolated `conn:{id}`
///   speaker scope that can never merge route, peer, session, decode or
///   gap state with another exporter behind the same NAT address.
///
/// Authorized sessions are revalidated against the identity store every
/// [`bmp_binding_recheck_interval`]; a revoked credential/binding
/// terminates the session's ingestion without a service restart.
///
/// Epoch fencing is per [`SpeakerKey`]: a stale connection cannot clear
/// a newer session, and a disconnect cannot touch another exporter's
/// EOR/completeness state. When the last connection of a `conn:*`
/// scope closes, its speaker state is retired (the scope can never
/// receive new data); `vexp:*`/`exp:*` speakers keep last-known routes
/// under the transport gap as before.
pub async fn run_bmp_listener(
    listener: TcpListener,
    mtls_acceptor: Option<TlsAcceptor>,
    state: Arc<RoutingServiceState>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let shared = BmpListenerShared {
        state,
        live_sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        deferred_terminations: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        active_sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        shutdown: shutdown.clone(),
    };
    {
        let monitor = shared.clone();
        tokio::spawn(async move { bmp_binding_revalidation_loop(monitor).await });
    }
    let handshake_permits = mtls_acceptor
        .as_ref()
        .map(|_| Arc::new(tokio::sync::Semaphore::new(bmp_tls_max_handshakes())));
    let handshake_timeout = bmp_tls_handshake_timeout();
    loop {
        tokio::select! {
            _ = wait_shutdown(shutdown.clone()) => break,
            accept_res = listener.accept() => {
                let Ok((stream, peer_sock)) = accept_res else { continue; };
                let speaker_ip = peer_sock.ip();
                // ── Bounded handshake admission ──
                // The accept loop must never serialize on a stalled
                // handshake: acquire a permit WITHOUT awaiting (excess
                // connections are dropped predictably), then let the
                // spawned task perform the handshake concurrently.
                let permit = match &handshake_permits {
                    Some(sem) => match sem.clone().try_acquire_owned() {
                        Ok(p) => Some(p),
                        Err(_) => {
                            tracing::warn!(
                                %speaker_ip,
                                max_handshakes = bmp_tls_max_handshakes(),
                                "BMP TLS handshake capacity exhausted; dropping connection"
                            );
                            continue;
                        }
                    },
                    None => None,
                };
                let ctx = shared.clone();
                let acceptor = mtls_acceptor.clone();
                let conn_shutdown = shutdown.clone();
                tokio::spawn(async move {
                    // ── Transport authentication (BN-021f) ──
                    // mTLS termination happens BEFORE any BMP byte is
                    // read: a failed or timed-out handshake yields no
                    // verified principal and no state. The principal is
                    // derived solely from the peer certificate.
                    let (io, verified) = match &acceptor {
                        Some(acc) => {
                            let hs = tokio::select! {
                                _ = wait_shutdown(conn_shutdown) => return,
                                res = tokio::time::timeout(
                                    handshake_timeout,
                                    acc.accept(stream),
                                ) => res,
                            };
                            match hs {
                                Ok(Ok(tls)) => {
                                    let principal = mtls_peer_principal(&tls);
                                    (BmpIo::Tls(tls), principal)
                                }
                                Ok(Err(e)) => {
                                    tracing::debug!(%speaker_ip, error = %e, "BMP mTLS handshake rejected");
                                    return;
                                }
                                Err(_) => {
                                    tracing::debug!(%speaker_ip, "BMP mTLS handshake timed out");
                                    return;
                                }
                            }
                        }
                        None => (BmpIo::Plain(stream), None),
                    };
                    // The handshake slot is freed once the transport is
                    // authenticated — it must not cap live sessions.
                    drop(permit);
                    handle_bmp_connection(io, verified, speaker_ip, ctx).await;
                });
            }
        }
    }
}

/// Per-connection BMP session: read Initiation, resolve identity,
/// allocate the speaker epoch, then stream frames until EOF,
/// Termination, invalid frame, authorization revocation, or transport
/// error — always flowing through the durable disconnect lifecycle.
async fn handle_bmp_connection(
    io: BmpIo,
    verified: Option<VerifiedIngressPrincipal>,
    speaker_ip: IpAddr,
    ctx: BmpListenerShared,
) {
    let st = ctx.state.clone();
    let sessions = ctx.live_sessions.clone();
    let deferred = ctx.deferred_terminations.clone();
    {
        let mut reader = BufReader::new(io);
        let hdr_timeout = Duration::from_secs(30);
        let body_timeout = Duration::from_secs(10);
        let bmp_trace = std::env::var("BONSAI_BMP_TRACE").is_ok_and(|v| v == "1");

        // ── Phase 1: BMP Initiation gates identity resolution ──
        // TCP open is not session start; the first BMP message
        // must be Initiation so exporter evidence exists before
        // any authoritative state is created (fail closed).
        let first_frame = match read_bmp_frame(
            &mut reader,
            st.config.max_frame_bytes,
            hdr_timeout,
            body_timeout,
        )
        .await
        {
            Ok(Some(frame)) => frame,
            Ok(None) => return,
            Err(e) => {
                tracing::debug!(%speaker_ip, error = %e, "BMP connection closed before Initiation");
                return;
            }
        };
        let is_initiation = first_frame.len() >= 6 && first_frame[0] == 3 && first_frame[5] == 4;
        if !is_initiation {
            tracing::warn!(%speaker_ip, "BMP connection did not open with Initiation; closing without creating state");
            return;
        }
        let (sys_name, sys_descr) = match parse_bmp_message_with_context(
            &first_frame,
            speaker_ip,
            st.config.max_frame_bytes,
            &BgpDecodeContext::default(),
        ) {
            Ok(payload) => match payload {
                BmpPayload::Initiation {
                    sys_name,
                    sys_descr,
                    ..
                } => (sys_name, sys_descr),
                _ => (None, None),
            },
            Err(e) => {
                tracing::warn!(%speaker_ip, error = %e, "unparseable BMP Initiation; using connection-isolated scope");
                (None, None)
            }
        };

        let connection_id = Uuid::new_v4().simple().to_string();
        let resolution = st.resolve_bmp_exporter_with_ingress(
            speaker_ip,
            &connection_id,
            sys_name.as_deref(),
            sys_descr.as_deref(),
            verified.as_ref(),
        );
        let mut scope = resolution.scope;

        // ── Phase 2: epoch allocation per SpeakerKey ──
        let epoch = {
            // Lock ordering: `sessions` is only ever acquired before `rib`
            // (never the reverse), so holding it across the rib write is safe.
            let mut live = sessions.lock().await;
            // P0-1/BN-021f hardening: at most ONE live
            // connection may hold a CLAIM-resolved `exp:*`
            // speaker scope. BMP sysName is a
            // self-reported claim — a second simultaneous
            // connection presenting the same claim
            // (impostor or legitimate multi-session
            // exporter) cannot be proven to share exporter
            // ownership in claim-compatible mode, so it is
            // isolated into its own `conn:*` scope instead
            // of merging authoritative speaker/RIB state.
            // `vexp:*` verified scopes are exempt: multiple
            // connections presenting the SAME verified
            // credential legitimately share the authorized
            // exporter (FRR global+VRF sessions). Isolated
            // `conn:*` scopes NEVER carry device
            // attribution — the claim remains evidence-only.
            if scope.exporter_instance.starts_with("exp:") {
                let claimed = scope.speaker_key(speaker_ip);
                if live.get(&claimed).is_some_and(|&(_, c)| c > 0) {
                    tracing::warn!(
                        %speaker_ip,
                        exporter_instance = %scope.exporter_instance,
                        device_id = ?scope.device_id,
                        "Concurrent BMP connection claims an already-live \
                         exporter scope; isolating to conn:* (same-claim \
                         connections cannot be proven to share ownership)"
                    );
                    scope = ExporterScope {
                        exporter_instance: format!("conn:{connection_id}"),
                        exporter_id: scope.exporter_id.clone(),
                        device_id: None,
                    };
                }
            }
            let speaker_key = scope.speaker_key(speaker_ip);
            match live.get(&speaker_key) {
                Some(&(epoch, count)) if count > 0 => {
                    if let Some(entry) = live.get_mut(&speaker_key) {
                        entry.1 += 1;
                    }
                    epoch
                }
                _ => {
                    let epoch = {
                        let mut rib = st.rib.write().await;
                        rib.open_speaker_session(
                            speaker_ip,
                            &scope.exporter_instance,
                            scope.exporter_id.as_deref(),
                            scope.device_id.as_deref(),
                            Utc::now(),
                        )
                    };
                    live.insert(speaker_key, (epoch, 1));
                    epoch
                }
            }
        };
        let speaker_key = scope.speaker_key(speaker_ip);
        tracing::info!(
            %speaker_ip,
            exporter_instance = %scope.exporter_instance,
            status = resolution.status,
            device_id = ?scope.device_id,
            binding_id = ?resolution.binding_id,
            "BMP exporter identity resolved"
        );

        // ── Register authorized session for revocation ──
        // Only binding-resolved sessions carry authority;
        // conn:*-isolated connections are never registered.
        // The revalidation monitor re-resolves `observed`
        // against fresh bindings and signals `revoke_tx`.
        let mut revoke_rx = match resolution.authorization.clone() {
            Some(auth) => {
                let (tx, rx) = tokio::sync::watch::channel(false);
                ctx.active_sessions.lock().await.insert(
                    connection_id.clone(),
                    ActiveBmpSession {
                        connection_id: connection_id.clone(),
                        speaker_key,
                        session_epoch: epoch,
                        binding_id: auth.binding_id,
                        canonical_exporter_id: auth.canonical_exporter_id,
                        device_id: auth.device_id,
                        credential_fingerprint: auth.credential_fingerprint,
                        observed: auth.observed,
                        principal: auth.principal,
                        revoke_tx: tx,
                    },
                );
                Some(rx)
            }
            None => None,
        };

        // ── Phase 3: stream frames under the resolved scope ──
        let mut pending_first = Some(first_frame);
        let disconnect_reason = loop {
            let frame = match pending_first.take() {
                Some(f) => Some(f),
                None => {
                    let read = read_bmp_frame(
                        &mut reader,
                        st.config.max_frame_bytes,
                        hdr_timeout,
                        body_timeout,
                    );
                    let frame_res = match revoke_rx.as_mut() {
                        Some(rx) => tokio::select! {
                            // Only `true` is ever sent, and
                            // sender drop means the registry
                            // entry is gone — either way
                            // authorization is no longer
                            // provable: fail closed.
                            _ = rx.changed() => {
                                break Some("bmp_authorization_revoked".to_string());
                            }
                            res = read => res,
                        },
                        None => read.await,
                    };
                    match frame_res {
                        Ok(Some(f)) => Some(f),
                        Ok(None) => None,
                        Err(e) => {
                            break Some(format!("bmp_transport_error: {e}"));
                        }
                    }
                }
            };
            let Some(frame) = frame else {
                // TCP EOF without BMP Termination = observation gap, not BGP withdraw
                break Some("bmp_tcp_eof".to_string());
            };
            if bmp_trace {
                // BMPv3 common header: ver(1) len(4) type(1);
                // peer header follows for types 0-4.
                let ver = frame.first().copied().unwrap_or(0);
                let mlen = frame
                    .get(1..5)
                    .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
                    .unwrap_or(0);
                let mtype = frame.get(5).copied().unwrap_or(0);
                let head_hex = frame
                    .iter()
                    .take(48)
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>();
                tracing::info!(%speaker_ip, %scope.exporter_instance, ver, mlen, mtype, head = %head_hex, "bmp_frame");
            }
            // BMP Termination (v3 msg type 5) ends THIS
            // connection's BMP session — RFC 7854 §4.5. While
            // sibling connections share this SpeakerKey, defer
            // the frame instead of letting `RibManager` destroy
            // shared speaker/RIB state; it is applied when the
            // LAST connection closes (see teardown below). In
            // every case this connection stops reading
            // immediately: frames received after Termination
            // must never mutate RIB state (a terminated BMP
            // session accepts no further observations), and a
            // deferred Termination still applies at teardown if
            // this connection turns out to be the last one.
            let is_termination = frame.len() >= 6 && frame[0] == 3 && frame[5] == 5;
            if is_termination {
                let live_count = sessions
                    .lock()
                    .await
                    .get(&speaker_key)
                    .map(|&(_, c)| c)
                    .unwrap_or(0);
                if live_count > 1 {
                    deferred.lock().await.insert(speaker_key, frame.clone());
                    tracing::info!(
                        %speaker_ip,
                        exporter_instance = %scope.exporter_instance,
                        live_connections = live_count,
                        "BMP Termination deferred while sibling connections are live"
                    );
                    break Some("bmp_termination".to_string());
                }
                // This is the last live connection: any frame
                // deferred by a sibling is superseded by this
                // effective speaker-wide termination.
                deferred.lock().await.remove(&speaker_key);
            }
            if let Err(e) = st
                .ingest_bmp_bytes_scoped(speaker_ip, &scope, Some(epoch), &frame)
                .await
            {
                // A rejected frame still closes the connection,
                // so it MUST flow through the same disconnect
                // lifecycle as TCP EOF — otherwise valid routing
                // state would linger with no durable gap record
                // (BN-021c/P0-B). The reason is recorded
                // distinctly from transport errors and EOF.
                tracing::warn!(%speaker_ip, exporter_instance = %scope.exporter_instance, error = %e, "Rejected invalid BMP frame");
                break Some(format!("bmp_frame_rejected: {e}"));
            }
            if is_termination {
                // Session terminated cleanly: no further frames
                // are accepted on this connection.
                break Some("bmp_termination".to_string());
            }
        };
        // Release this connection's share of the speaker epoch before recording
        // the disconnect so a fast reconnect opens a fresh epoch.
        let last_connection = {
            let mut live = sessions.lock().await;
            if let Some(entry) = live.get_mut(&speaker_key) {
                entry.1 = entry.1.saturating_sub(1);
                if entry.1 == 0 {
                    live.remove(&speaker_key);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if let Some(reason) = disconnect_reason {
            if last_connection {
                // A Termination deferred by a sibling becomes
                // effective now that no connections remain:
                // ingest it so the speaker teardown, SpeakerDown
                // event and journal record match a live inline
                // termination exactly (replay-consistent).
                if let Some(term_frame) = deferred.lock().await.remove(&speaker_key) {
                    if let Err(e) = st
                        .ingest_bmp_bytes_scoped(speaker_ip, &scope, Some(epoch), &term_frame)
                        .await
                    {
                        tracing::warn!(%speaker_ip, exporter_instance = %scope.exporter_instance, error = %e, "Failed to apply deferred BMP Termination");
                    }
                }
                let outcome = st
                    .record_transport_disconnect_for_key(
                        speaker_key,
                        &scope.exporter_instance,
                        Some(epoch),
                        reason,
                    )
                    .await;
                // An isolated connection scope can never receive
                // new data; retire its state instead of leaking
                // last-known routes per dead connection. Retire
                // only when the durable lifecycle requirement is
                // satisfied: Recorded (gap journaled) or
                // NoActiveSession (session already ended via
                // Termination — nothing to gap). A stale-fenced
                // or persistence-failed disconnect must NOT
                // retire state: the former belongs to a newer
                // session, the latter would diverge live view
                // from journal replay (P0-2).
                if matches!(
                    outcome,
                    TransportDisconnectOutcome::Recorded
                        | TransportDisconnectOutcome::NoActiveSession
                ) && scope.exporter_instance.starts_with("conn:")
                {
                    st.retire_connection_scoped_speaker(speaker_key).await;
                }
            }
        }
        // Session ended (EOF/Termination/revocation/transport
        // error): drop its revalidation registration.
        ctx.active_sessions.lock().await.remove(&connection_id);
    }
}

// ── Internal Routing API (`/internal/routing/v1/*`) ──────────────────────────

#[derive(Debug, Deserialize, Default)]
pub struct PeersQuery {
    pub speaker: Option<String>,
    /// Canonical exporter selector (e.g. "exp:rtr-a" resolved or the
    /// provisional "bmp:{sysName}") — matches the speaker's
    /// `ObservationSource.exporter_id`, independent of transport IP.
    pub exporter_id: Option<String>,
    /// Logical-exporter scope selector (`exp:*`/`conn:*`/legacy).
    pub exporter_instance: Option<String>,
    /// Canonical inventory device id selector (resolved exporters only).
    pub device_id: Option<String>,
    pub vrf_rd: Option<String>,
    pub state: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
pub struct RoutesQuery {
    /// Speaker transport address, or the exact `speaker_key` form
    /// `"<addr>|<exporter_instance>"` emitted by `/speakers` — required
    /// to select one exporter behind a shared NAT address.
    pub speaker: Option<String>,
    pub peer: Option<String>,
    pub view: Option<String>,
    pub vrf_rd: Option<String>,
    pub prefix: Option<String>,
    pub family: Option<String>,
    /// Logical-exporter scope selector (`exp:*`/`conn:*`/legacy).
    pub exporter_instance: Option<String>,
    /// Canonical exporter id selector.
    pub exporter_id: Option<String>,
    /// Canonical inventory device id selector (resolved exporters only).
    pub device_id: Option<String>,
    pub limit: Option<usize>,
    pub cursor: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
pub struct PrefixHistoryQuery {
    pub prefix: Option<String>,
    /// Speaker transport address or `"<addr>|<exporter_instance>"` key.
    pub speaker: Option<String>,
    pub vrf_rd: Option<String>,
    /// Logical-exporter scope selector (`exp:*`/`conn:*`/legacy).
    pub exporter_instance: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
pub struct EventsQuery {
    pub after_seq: Option<u64>,
    /// Speaker transport address or `"<addr>|<exporter_instance>"` key.
    pub speaker: Option<String>,
    pub vrf_rd: Option<String>,
    /// Logical-exporter scope selector (`exp:*`/`conn:*`/legacy).
    pub exporter_instance: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct IngestBmpRequest {
    pub speaker_addr: Option<String>,
    pub speaker: Option<String>,
    pub vrf_rd: Option<String>,
    pub session_epoch: Option<u64>,
    /// Optional logical-exporter scope (`exp:*`/`conn:*`) for callers
    /// that multiplex exporters over one transport address. Empty keeps
    /// the legacy shared-per-address scope.
    #[serde(default)]
    pub exporter_instance: Option<String>,
    #[serde(default)]
    pub exporter_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
    pub frame_hex: String,
}

fn check_internal_token(headers: &HeaderMap, state: &RoutingServiceState) -> Result<(), Response> {
    use subtle::ConstantTimeEq;
    if let Some(ref expected) = state.config.internal_service_token {
        let provided = headers
            .get("x-bonsai-routing-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let matches = provided.len() == expected.len()
            && bool::from(provided.as_bytes().ct_eq(expected.as_bytes()));
        if !matches {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "unauthorized_internal_client",
                    "message": "Missing or invalid x-bonsai-routing-token"
                })),
            )
                .into_response());
        }
    } else {
        let allow_no_token = std::env::var("BONSAI_ROUTING_ALLOW_INSECURE_NO_TOKEN")
            .or_else(|_| std::env::var("BONSAI_ALLOW_INSECURE_DEV"))
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false);
        if !allow_no_token {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "unauthorized_internal_client",
                    "message": "Routing service internal endpoint requires authentication; internal_service_token is not configured"
                })),
            )
                .into_response());
        }
    }
    Ok(())
}

pub fn build_internal_routing_router(state: Arc<RoutingServiceState>) -> Router {
    Router::new()
        .route("/internal/routing/v1/health", get(internal_health_handler))
        .route(
            "/internal/routing/v1/speakers",
            get(internal_speakers_handler),
        )
        .route("/internal/routing/v1/peers", get(internal_peers_handler))
        .route("/internal/routing/v1/routes", get(internal_routes_handler))
        .route(
            "/internal/routing/v1/prefix-history",
            get(internal_prefix_history_handler),
        )
        .route(
            "/internal/routing/v1/coverage",
            get(internal_coverage_handler),
        )
        .route(
            "/internal/routing/v1/evidence-coverage",
            get(internal_evidence_coverage_handler),
        )
        .route(
            "/internal/routing/v1/prefix-journey",
            get(internal_prefix_journey_handler),
        )
        .route(
            "/internal/routing/v1/peer-timeline",
            get(internal_peer_timeline_handler),
        )
        .route(
            "/internal/routing/v1/compare-snapshots",
            get(internal_compare_snapshots_handler),
        )
        .route(
            "/internal/routing/v1/explain-prefix",
            get(internal_explain_prefix_handler),
        )
        .route(
            "/internal/routing/v1/adjudicate-prefix",
            post(internal_adjudicate_prefix_handler),
        )
        .route(
            "/internal/routing/v1/adjudications",
            get(internal_adjudications_handler),
        )
        .route("/internal/routing/v1/events", get(internal_events_handler))
        .route(
            "/internal/routing/v1/ingest-bmp",
            post(internal_ingest_bmp_handler),
        )
        .with_state(state)
}

async fn internal_health_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_health_json(&state).await).into_response()
}

async fn internal_speakers_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_speakers_json(&state).await).into_response()
}

async fn internal_peers_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<PeersQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_peers_json(&state, &q).await).into_response()
}

async fn internal_routes_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<RoutesQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    let (status, val) = build_routes_json(&state, &q).await;
    (status, Json(val)).into_response()
}

async fn internal_prefix_history_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<PrefixHistoryQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    let (status, val) = build_prefix_history_json(&state, &q).await;
    (status, Json(val)).into_response()
}

async fn internal_coverage_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_coverage_json(&state).await).into_response()
}

async fn internal_evidence_coverage_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<EvidenceCoverageQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_evidence_coverage_json(&state, &q).await).into_response()
}

async fn internal_prefix_journey_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<PrefixJourneyQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    let (status, val) = build_prefix_journey_json(&state, &q).await;
    (status, Json(val)).into_response()
}

async fn internal_peer_timeline_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<PeerTimelineQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_peer_timeline_json(&state, &q).await).into_response()
}

async fn internal_compare_snapshots_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<CompareSnapshotsQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_compare_snapshots_json(&state, &q).await).into_response()
}

async fn internal_explain_prefix_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<ExplainPrefixQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    let (status, val) = build_explain_prefix_json(&state, &q).await;
    (status, Json(val)).into_response()
}

async fn internal_adjudicate_prefix_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Json(req): Json<OperatorAdjudicationRequest>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    let (status, val) = build_adjudicate_prefix_json(req);
    (status, Json(val)).into_response()
}

async fn internal_adjudications_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_adjudications_json()).into_response()
}

async fn internal_events_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    Json(build_events_json(&state, &q).await).into_response()
}

async fn internal_ingest_bmp_handler(
    State(state): State<Arc<RoutingServiceState>>,
    headers: HeaderMap,
    Json(req): Json<IngestBmpRequest>,
) -> Response {
    if let Err(resp) = check_internal_token(&headers, &state) {
        return resp;
    }
    if let (Some(s1), Some(s2)) = (req.speaker.as_deref(), req.speaker_addr.as_deref()) {
        if s1 != s2 {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "conflicting_speaker_aliases",
                    "message": format!("Conflicting speaker aliases: speaker ('{s1}') != speaker_addr ('{s2}')")
                })),
            )
                .into_response();
        }
    }

    let Some(spk_raw) = req.speaker_addr.as_deref().or(req.speaker.as_deref()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "missing_speaker_addr",
                "message": "Missing required speaker or speaker_addr field"
            })),
        )
            .into_response();
    };

    let Ok(speaker_addr) = spk_raw.parse::<IpAddr>() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_speaker_addr",
                "message": format!("Invalid speaker IP address: {spk_raw}")
            })),
        )
            .into_response();
    };
    let Ok(bytes) = hex::decode(req.frame_hex.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_frame_hex",
                "message": "frame_hex must be valid hex-encoded BMP PDU bytes"
            })),
        )
            .into_response();
    };

    if let Some(frame_rd) = rbmp_core::bmp::parser::extract_frame_rd(&bytes) {
        if let Some(ref req_vrf) = req.vrf_rd {
            if req_vrf != &frame_rd {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": "vrf_rd_mismatch",
                        "message": format!("Ingest request vrf_rd '{req_vrf}' does not match BMP wire frame RD '{frame_rd}'")
                    })),
                )
                    .into_response();
            }
        }
    }
    let scope = ExporterScope {
        exporter_instance: req.exporter_instance.unwrap_or_default(),
        exporter_id: req.exporter_id,
        device_id: req.device_id,
    };
    match state
        .ingest_bmp_bytes_scoped(speaker_addr, &scope, req.session_epoch, &bytes)
        .await
    {
        Ok(msg) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "accepted": true,
                "message_id": msg.id,
                "speaker_addr": msg.speaker_addr,
                "durable_sequence": state.event_journal.latest_seq().await,
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "accepted": false,
                "error": "bmp_parse_or_journal_failed",
                "message": e.to_string(),
            })),
        )
            .into_response(),
    }
}

// ── Shared JSON Builders (used by internal API and in-process Bonsai proxy) ──

pub async fn build_health_json(state: &RoutingServiceState) -> serde_json::Value {
    let rib = state.rib.read().await;
    let speakers_connected = rib.speakers().len();
    let peers_up = rib.total_peers_up();
    let total_routes = rib.total_routes();
    drop(rib);
    let latest_sequence = state.event_journal.latest_seq().await;
    let completeness = state.overall_completeness().await;
    let gaps = state.transport_gaps.read().await.len();

    serde_json::json!({
        "status": "ok",
        "service": "bonsai-routing",
        "api_version": ROUTING_API_VERSION,
        "speakers_connected": speakers_connected,
        "peers_up": peers_up,
        "total_routes": total_routes,
        "durable_sequence": latest_sequence,
        "transport_gaps": gaps,
        "completeness_state": completeness,
        "receive_time": Utc::now().to_rfc3339(),
    })
}

/// Match a `speaker` query filter against a speaker transport address and
/// its logical-exporter scope. Accepts either the bare address (matches
/// every exporter instance on that address — legacy behavior) or the
/// exact `"<addr>|<exporter_instance>"` speaker_key form emitted by
/// `/speakers`, which selects one exporter behind a shared NAT address.
fn speaker_matches_filter(speaker_addr: IpAddr, exporter_instance: &str, filter: &str) -> bool {
    let addr_s = speaker_addr.to_string();
    filter == addr_s
        || filter
            .strip_prefix(addr_s.as_str())
            .is_some_and(|rest| rest.strip_prefix('|') == Some(exporter_instance))
}

/// Derive the attribution status of a speaker session for API output.
/// `resolved` requires a trusted-binding device id or an `exp:*` scope;
/// `conn:*` scopes are provisional connection-isolated exporters;
/// legacy/unspecified scopes are explicitly `legacy_unresolved`.
fn speaker_identity_status(source: &rbmp_core::bmp::types::ObservationSource) -> &'static str {
    let inst = source.exporter_instance.as_str();
    if source.device_id.is_some() || inst.starts_with("exp:") || inst.starts_with("vexp:") {
        "resolved"
    } else if inst.starts_with("conn:") {
        "provisional"
    } else {
        "legacy_unresolved"
    }
}

pub async fn build_speakers_json(state: &RoutingServiceState) -> serde_json::Value {
    let rib = state.rib.read().await;
    let gaps = state.transport_gaps.read().await;
    let latest_seq = state.event_journal.latest_seq().await;

    let mut list = Vec::new();
    for spk in rib.speakers() {
        let speaker_key = SpeakerKey::for_source(&spk.source);
        let gap = gaps.get(&speaker_key);
        let completeness = if gap.is_some() {
            "gap"
        } else if spk.peers.is_empty() {
            "syncing"
        } else if spk.peers.values().any(|p| p.state == PeerState::Down) {
            if spk
                .peers
                .values()
                .any(|p| p.state == PeerState::Down && !p.llgr_active)
            {
                "gap"
            } else {
                "stale"
            }
        } else if spk
            .peers
            .values()
            .all(|p| p.state == PeerState::Up && !p.eor_families.is_empty())
        {
            "complete_for_observed_view"
        } else {
            "syncing"
        };
        list.push(serde_json::json!({
            "speaker_addr": spk.speaker_addr,
            "speaker_key": format!("{}|{}", spk.speaker_addr, spk.source.exporter_instance),
            "exporter_instance": spk.source.exporter_instance,
            "exporter_id": spk.source.exporter_id,
            "device_id": spk.source.device_id,
            "identity_status": speaker_identity_status(&spk.source),
            "session_epoch": spk.source.session_epoch,
            "source": spk.source,
            "sys_name": spk.sys_name,
            "sys_descr": spk.sys_descr,
            "connected_at": spk.connected_at,
            "peer_count": spk.peer_count(),
            "up_peer_count": spk.up_peer_count(),
            "total_routes": spk.total_routes(),
            "completeness_state": completeness,
            "transport_gap": gap,
        }));
    }

    serde_json::json!({
        "api_version": ROUTING_API_VERSION,
        "durable_sequence": latest_seq,
        "receive_time": Utc::now().to_rfc3339(),
        "completeness_state": state.overall_completeness().await,
        "speakers": list,
    })
}

pub async fn build_peers_json(state: &RoutingServiceState, q: &PeersQuery) -> serde_json::Value {
    let rib = state.rib.read().await;
    let gaps = state.transport_gaps.read().await;
    let latest_seq = state.event_journal.latest_seq().await;
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);

    let mut peers = Vec::new();
    for spk in rib.speakers() {
        if let Some(ref spk_filter) = q.speaker {
            if !speaker_matches_filter(spk.speaker_addr, &spk.source.exporter_instance, spk_filter)
            {
                continue;
            }
        }
        if let Some(ref exp_filter) = q.exporter_id {
            if &spk.source.exporter_id != exp_filter {
                continue;
            }
        }
        if let Some(ref inst_filter) = q.exporter_instance {
            if &spk.source.exporter_instance != inst_filter {
                continue;
            }
        }
        if let Some(ref dev_filter) = q.device_id {
            if spk.source.device_id.as_deref() != Some(dev_filter.as_str()) {
                continue;
            }
        }
        let has_transport_gap = gaps.contains_key(&SpeakerKey::for_source(&spk.source));
        for (peer_key, peer) in &spk.peers {
            if let Some(ref rd_filter) = q.vrf_rd {
                if peer_key.distinguisher_string() != *rd_filter {
                    continue;
                }
            }
            let state_str = match peer.state {
                PeerState::Up => "up",
                PeerState::Down => "down",
                PeerState::Unknown => "unknown",
            };
            if let Some(ref st_filter) = q.state {
                if state_str != st_filter.to_lowercase() {
                    continue;
                }
            }
            let completeness = if has_transport_gap {
                "gap"
            } else if peer.state == PeerState::Down {
                if peer.llgr_active { "stale" } else { "gap" }
            } else if !peer.eor_families.is_empty() {
                "complete_for_observed_view"
            } else {
                "syncing"
            };
            peers.push(serde_json::json!({
                "speaker_addr": spk.speaker_addr,
                "speaker_key": format!("{}|{}", spk.speaker_addr, spk.source.exporter_instance),
                "speaker_exporter_instance": spk.source.exporter_instance,
                "speaker_exporter_id": spk.source.exporter_id,
                "speaker_device_id": spk.source.device_id,
                "speaker_identity_status": speaker_identity_status(&spk.source),
                "speaker_session_epoch": spk.source.session_epoch,
                "speaker_sys_name": spk.sys_name,
                "speaker_sys_descr": spk.sys_descr,
                "peer_key": peer_key,
                "peer_type": peer.peer_type,
                "peer_distinguisher": peer_key.distinguisher_string(),
                "peer_address": peer.peer_address,
                "peer_as": peer.peer_as,
                "peer_bgp_id": peer.peer_bgp_id,
                "local_as": peer.local_as,
                "hold_time": peer.hold_time,
                "state": state_str,
                "up_at": peer.up_at,
                "down_at": peer.down_at,
                "flap_count": peer.flap_count,
                "llgr_active": peer.llgr_active,
                "route_counts": peer.route_counts,
                "eor_families": peer.eor_families,
                "completeness_state": completeness,
            }));
            if peers.len() >= limit {
                break;
            }
        }
    }

    serde_json::json!({
        "api_version": ROUTING_API_VERSION,
        "durable_sequence": latest_seq,
        "receive_time": Utc::now().to_rfc3339(),
        "completeness_state": state.overall_completeness().await,
        "peers": peers,
    })
}

pub async fn build_routes_json(
    state: &RoutingServiceState,
    q: &RoutesQuery,
) -> (StatusCode, serde_json::Value) {
    let latest_seq = state.event_journal.latest_seq().await;

    // Check explicit family filter against FamilySupportStatus
    if let Some(ref fam) = q.family {
        let fam_lower = fam.trim().to_lowercase();
        let afi_safi = match fam_lower.as_str() {
            "ipv4-unicast" | "ipv4" | "1/1" => AfiSafi::ipv4_unicast(),
            "ipv6-unicast" | "ipv6" | "2/1" => AfiSafi::ipv6_unicast(),
            "ipv4-vpn" | "vpnv4" | "1/128" => AfiSafi::ipv4_vpn(),
            "ipv6-vpn" | "vpnv6" | "2/128" => AfiSafi::ipv6_vpn(),
            "l2vpn-evpn" | "evpn" | "25/70" => AfiSafi::evpn(),
            "bgp-ls" | "bgpls" | "16388/71" => AfiSafi::new(16388, 71),
            _ => AfiSafi::new(65535, 255),
        };
        let support = afi_safi.support_status();
        if support != FamilySupportStatus::Supported {
            return (
                StatusCode::OK,
                serde_json::json!({
                    "api_version": ROUTING_API_VERSION,
                    "family": fam,
                    "support_status": support,
                    "completeness_state": "unsupported",
                    "durable_sequence": latest_seq,
                    "receive_time": Utc::now().to_rfc3339(),
                    "routes": [],
                    "message": format!("Address family '{}' has support status {:?} and is not projected as a complete RIB view.", fam, support),
                }),
            );
        }
    }

    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let offset = q.cursor.unwrap_or(0);
    let rib = state.rib.read().await;
    let gaps = state.transport_gaps.read().await;

    let mut all_matching = Vec::new();
    for spk in rib.speakers() {
        if let Some(ref spk_filter) = q.speaker {
            if !speaker_matches_filter(spk.speaker_addr, &spk.source.exporter_instance, spk_filter)
            {
                continue;
            }
        }
        if let Some(ref inst_filter) = q.exporter_instance {
            if &spk.source.exporter_instance != inst_filter {
                continue;
            }
        }
        if let Some(ref exp_filter) = q.exporter_id {
            if &spk.source.exporter_id != exp_filter {
                continue;
            }
        }
        if let Some(ref dev_filter) = q.device_id {
            if spk.source.device_id.as_deref() != Some(dev_filter.as_str()) {
                continue;
            }
        }
        let speaker_key = SpeakerKey::for_source(&spk.source);
        let spk_gap = gaps.contains_key(&speaker_key);
        for (peer_key, table) in rib.ribs_for_speaker(speaker_key) {
            if let Some(ref peer_filter) = q.peer {
                if peer_key.peer_address.to_string() != *peer_filter {
                    continue;
                }
            }
            if let Some(ref rd_filter) = q.vrf_rd {
                if peer_key.distinguisher_string() != *rd_filter {
                    continue;
                }
            }
            let peer_sess = spk.peers.get(peer_key);
            for rib_type in [
                RibType::AdjRibInPrePolicy,
                RibType::AdjRibInPostPolicy,
                RibType::LocRib,
                RibType::LocRibFiltered,
                RibType::AdjRibOutPrePolicy,
                RibType::AdjRibOutPostPolicy,
            ] {
                let view_str = format!("{:?}", rib_type);
                if let Some(ref view_filter) = q.view {
                    if !view_str.eq_ignore_ascii_case(view_filter) {
                        continue;
                    }
                }
                for entry in table.iter_rib(rib_type) {
                    let pfx_str = entry.prefix.to_string();
                    if let Some(ref pfx_filter) = q.prefix {
                        if pfx_str != *pfx_filter {
                            continue;
                        }
                    }
                    let afi_safi = entry.prefix.afi_safi();
                    let eor_done = peer_sess
                        .map(|ps| ps.has_eor(rib_type, afi_safi))
                        .unwrap_or(false);
                    let completeness = if spk_gap {
                        "gap"
                    } else if entry.llgr_state != rbmp_core::bgp::types::LlgrState::Normal {
                        "stale"
                    } else if eor_done {
                        "complete_for_observed_view"
                    } else {
                        "syncing"
                    };

                    all_matching.push(serde_json::json!({
                        "source": spk.source,
                        "speaker_addr": spk.speaker_addr,
                        "peer_key": peer_key,
                        "peer_addr": entry.peer_addr,
                        "peer_as": entry.peer_as,
                        "peer_distinguisher": peer_key.distinguisher_string(),
                        "view": view_str,
                        "prefix": pfx_str,
                        "afi_safi": afi_safi.to_string(),
                        "path_id": entry.path_id,
                        "is_best": entry.is_best,
                        "llgr_state": entry.llgr_state,
                        "observation_time": entry.received_at,
                        "completeness_state": completeness,
                        "as_path": entry.attributes.as_path.as_ref().map(|p| p.to_string()),
                        "next_hop": entry.attributes.next_hop,
                        "local_pref": entry.attributes.local_pref,
                        "med": entry.attributes.multi_exit_disc,
                        "origin": entry.attributes.origin.map(|o| o.to_string()),
                        "communities": entry.attributes.communities.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
                    }));
                }
            }
        }
    }

    let total_matching = all_matching.len();
    let page: Vec<_> = all_matching.into_iter().skip(offset).take(limit).collect();
    let next_cursor = if offset + page.len() < total_matching {
        Some(offset + page.len())
    } else {
        None
    };

    (
        StatusCode::OK,
        serde_json::json!({
            "api_version": ROUTING_API_VERSION,
            "support_status": "supported",
            "completeness_state": state.overall_completeness().await,
            "durable_sequence": latest_seq,
            "receive_time": Utc::now().to_rfc3339(),
            "total_matching": total_matching,
            "next_cursor": next_cursor,
            "routes": page,
        }),
    )
}

pub async fn build_prefix_history_json(
    state: &RoutingServiceState,
    q: &PrefixHistoryQuery,
) -> (StatusCode, serde_json::Value) {
    let Some(ref prefix) = q.prefix else {
        return (
            StatusCode::BAD_REQUEST,
            serde_json::json!({
                "error": "missing_prefix_parameter",
                "message": "Query parameter 'prefix' is required for /prefix-history"
            }),
        );
    };
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let history: Vec<PrefixHistoryItem> = state
        .event_journal
        .prefix_history_filtered(
            prefix,
            q.speaker.as_deref(),
            q.vrf_rd.as_deref(),
            q.exporter_instance.as_deref(),
            limit,
        )
        .await;
    let history_window = state
        .event_journal
        .recent_window(rbmp_store::journal::HISTORY_SCAN_LIMIT)
        .await;
    let latest_seq = history_window.scanned_to_seq;
    let completeness = state.overall_completeness().await;

    (
        StatusCode::OK,
        serde_json::json!({
            "api_version": ROUTING_API_VERSION,
            "prefix": prefix,
            "speaker": q.speaker,
            "vrf_rd": q.vrf_rd,
            "support_status": "supported",
            "completeness_state": completeness,
            "durable_sequence": latest_seq,
            "receive_time": Utc::now().to_rfc3339(),
            "events": history,
            "history_coverage": if history_window.truncated || !state.projection_semantics_ready { "partial" } else { "complete" },
            "history_window": history_window,
        }),
    )
}

pub async fn build_coverage_json(state: &RoutingServiceState) -> serde_json::Value {
    let families = [
        ("ipv4-unicast", AfiSafi::ipv4_unicast()),
        ("ipv6-unicast", AfiSafi::ipv6_unicast()),
        ("ipv4-vpn", AfiSafi::ipv4_vpn()),
        ("ipv6-vpn", AfiSafi::ipv6_vpn()),
        ("l2vpn-evpn", AfiSafi::evpn()),
        ("bgp-ls", AfiSafi::new(16388, 71)),
    ];
    let matrix: Vec<_> = families
        .iter()
        .map(|(name, af)| {
            serde_json::json!({
                "family": name,
                "afi": af.afi.as_u16(),
                "safi": af.safi.as_u8(),
                "support_status": af.support_status(),
            })
        })
        .collect();

    serde_json::json!({
        "api_version": ROUTING_API_VERSION,
        "durable_sequence": state.event_journal.latest_seq().await,
        "completeness_state": state.overall_completeness().await,
        "receive_time": Utc::now().to_rfc3339(),
        "families": matrix,
    })
}

pub async fn build_events_json(state: &RoutingServiceState, q: &EventsQuery) -> serde_json::Value {
    let after_seq = q.after_seq.unwrap_or(0);
    let limit = q.limit.unwrap_or(100).clamp(1, 10_000);
    let latest_seq = state.event_journal.latest_seq().await;

    let mut current_cursor = after_seq;
    let mut events = Vec::new();
    let mut scanned_to_seq = after_seq;
    let chunk_size = 1000;
    let mut scanned_events = 0;
    let scan_limit = rbmp_store::journal::HISTORY_SCAN_LIMIT;

    while events.len() < limit && current_cursor < latest_seq && scanned_events < scan_limit {
        let chunk = state
            .event_journal
            .events_after_seq(current_cursor, chunk_size)
            .await;
        if chunk.is_empty() {
            break;
        }
        for ev in chunk {
            scanned_events += 1;
            current_cursor = current_cursor.max(ev.event_key.sequence);
            scanned_to_seq = current_cursor;

            if let Some(ref spk) = q.speaker {
                if !speaker_matches_filter(ev.speaker, &ev.event_key.source.exporter_instance, spk)
                {
                    continue;
                }
            }
            if let Some(ref inst) = q.exporter_instance {
                if &ev.event_key.source.exporter_instance != inst {
                    continue;
                }
            }
            if let Some(ref rd) = q.vrf_rd {
                let ev_rd = match &ev.payload {
                    RibEventPayload::RouteChange(rc) => Some(rc.peer_key.distinguisher_string()),
                    RibEventPayload::PeerUp { peer_header, .. }
                    | RibEventPayload::PeerDown { peer_header, .. }
                    | RibEventPayload::EndOfRib { peer_header, .. }
                    | RibEventPayload::Stats { peer_header, .. } => Some(
                        PeerKey::from_header(
                            ev.speaker,
                            &ev.event_key.source.exporter_instance,
                            peer_header,
                        )
                        .distinguisher_string(),
                    ),
                    RibEventPayload::SpeakerUp { .. } | RibEventPayload::SpeakerDown { .. } => None,
                };
                if ev_rd.as_deref() != Some(rd.as_str()) {
                    continue;
                }
            }

            events.push(ev);
            if events.len() >= limit {
                break;
            }
        }
    }

    let next_after_seq = events
        .last()
        .map(|e| e.event_key.sequence)
        .unwrap_or(scanned_to_seq);

    let truncated = scanned_to_seq < latest_seq;

    serde_json::json!({
        "api_version": ROUTING_API_VERSION,
        "durable_sequence": latest_seq,
        "after_seq": after_seq,
        "projection_semantics_version": if state.projection_semantics_ready { ROUTING_PROJECTION_VERSION } else { 0 },
        "next_after_seq": next_after_seq,
        "scanned_to_seq": scanned_to_seq,
        "scanned_events": scanned_events,
        "scan_limit": scan_limit,
        "history_coverage": if truncated { "partial" } else { "complete" },
        "truncated": truncated,
        "completeness_state": state.overall_completeness().await,
        "receive_time": Utc::now().to_rfc3339(),
        "events": events,
    })
}

// ── B13: Checkpointed Graph Summary Projector ────────────────────────────────

/// Projection identity for one speaker scope: transport address plus
/// logical-exporter instance. Two exporters behind one NAT address
/// produce distinct ids, so graph/checkpoint state can never merge.
/// Legacy/unspecified scopes keep the bare address for continuity with
/// pre-BN-021c checkpoints.
pub(crate) fn speaker_projection_id(ev: &rbmp_rib::event::RibEvent) -> String {
    let src = &ev.event_key.source;
    let inst = src.exporter_instance.trim();
    if inst.is_empty() || inst == rbmp_core::bmp::types::LEGACY_EXPORTER_INSTANCE {
        ev.speaker.to_string()
    } else {
        format!("{}|{}", ev.speaker, inst)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpeakerSummaryProjection {
    pub speaker_addr: String,
    #[serde(default)]
    pub exporter_instance: String,
    #[serde(default)]
    pub exporter_id: String,
    #[serde(default)]
    pub device_id: Option<String>,
    pub sys_name: Option<String>,
    pub sys_descr: Option<String>,
    pub session_epoch: u64,
    pub is_up: bool,
    pub last_event_sequence: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerSummaryProjection {
    pub peer_key_id: String,
    pub speaker_addr: String,
    #[serde(default)]
    pub exporter_instance: String,
    pub peer_addr: String,
    pub peer_as: u32,
    pub peer_type: u8,
    pub peer_distinguisher: String,
    pub state: String,
    pub active_prefixes: HashSet<String>,
    pub announce_events_count: u64,
    pub withdraw_events_count: u64,
    pub eor_families: HashSet<String>,
    pub last_event_sequence: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RoutingProjectionCheckpoint {
    #[serde(default)]
    pub projection_version: u32,
    pub last_applied_sequence: u64,
    pub last_event_id: String,
    pub updated_at_ns: i64,
    pub applied_event_count: u64,
    pub skipped_raw_route_churn_count: u64,
    pub speaker_summaries: HashMap<String, SpeakerSummaryProjection>,
    pub peer_summaries: HashMap<String, PeerSummaryProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectionStepReport {
    pub newly_applied_events: usize,
    pub journal_latest_sequence: u64,
    pub graph_checkpoint_sequence: u64,
    pub sequence_lag: u64,
    pub checkpoint: RoutingProjectionCheckpoint,
}

/// Load the durable routing projection checkpoint from `GraphStore`.
pub async fn load_projection_checkpoint(
    store: &crate::graph::GraphStore,
) -> RoutingProjectionCheckpoint {
    let items = store
        .list_config_items(Some(ROUTING_CHECKPOINT_CONFIG_CLASS.to_string()))
        .await
        .unwrap_or_default();
    if let Some(ci) = items
        .into_iter()
        .find(|i| i.id == ROUTING_CHECKPOINT_ID && i.enabled)
    {
        if let Ok(cp) = serde_json::from_str::<RoutingProjectionCheckpoint>(&ci.content_json) {
            if cp.projection_version == 1 {
                return cp;
            }
        }
    }
    RoutingProjectionCheckpoint {
        projection_version: 1,
        ..Default::default()
    }
}

/// Resolve the private routing service `(base_url, token, has_explicit_db_url)` from `GraphStore` (`app_config`) or environment (`R05`/`R06`).
pub async fn resolve_routing_service_endpoint_config(
    store: &crate::graph::GraphStore,
) -> (String, Option<String>, bool) {
    let items = store
        .list_config_items(Some("app_config".to_string()))
        .await
        .unwrap_or_default();
    let db_url = items
        .iter()
        .find(|i| i.name == "routing_service_url" && i.enabled)
        .map(|i| i.content_json.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty());
    let has_explicit_db_url = db_url.is_some();
    let url = db_url.unwrap_or_else(|| {
        std::env::var("BONSAI_ROUTING_SERVICE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8081".to_string())
    });
    let token = items
        .iter()
        .find(|i| {
            (i.name == "routing_service_token" || i.name == "internal_service_token") && i.enabled
        })
        .map(|i| i.content_json.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("BONSAI_ROUTING_SERVICE_TOKEN")
                .ok()
                .filter(|s| !s.trim().is_empty())
        });
    (
        url.trim_end_matches('/').to_string(),
        token,
        has_explicit_db_url,
    )
}

/// Project new durable `RibEvent`s after `checkpoint.last_applied_sequence` into `GraphStore`.
///
/// Session/lifecycle events (`SpeakerUp`, `SpeakerDown`, `PeerUp`, `PeerDown`, `EndOfRib`)
/// update graph provenance and `BmpSession` state, while `RouteChange` events update bounded
/// per-peer route counts/summaries without duplicating raw route churn into the `BgpRoute` graph table.
pub async fn project_routing_events_to_graph(
    event_journal: &RibEventJournal,
    store: &crate::graph::GraphStore,
    batch_limit: usize,
) -> anyhow::Result<ProjectionStepReport> {
    let cp = load_projection_checkpoint(store).await;
    let pending = event_journal
        .events_after_seq(cp.last_applied_sequence, batch_limit.max(1))
        .await;
    let latest_seq = event_journal.latest_seq().await;
    apply_routing_events_to_graph(cp, pending, latest_seq, store).await
}

/// Project new durable `RibEvent`s from either a configured remote `bonsai-routing` HTTP service
/// (`/internal/routing/v1/events`) or an in-process `RoutingServiceState` into `GraphStore` (`R05`).
pub async fn project_remote_routing_events_to_graph(
    store: &crate::graph::GraphStore,
    batch_limit: usize,
) -> anyhow::Result<ProjectionStepReport> {
    let (base_url, token, has_explicit_db_url) =
        resolve_routing_service_endpoint_config(store).await;
    if !has_explicit_db_url {
        if let Some(in_proc) = default_routing_service() {
            if !in_proc.projection_semantics_ready {
                anyhow::bail!(
                    "routing history lacks BMP data needed for snapshot retirement projection"
                );
            }
            return project_routing_events_to_graph(&in_proc.event_journal, store, batch_limit)
                .await;
        }
    }

    let cp = load_projection_checkpoint(store).await;
    let limit = batch_limit.clamp(1, 1000);
    let url = format!(
        "{base_url}/internal/routing/v1/events?after_seq={}&limit={limit}",
        cp.last_applied_sequence
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()?;
    let mut req = client.get(&url);
    if let Some(t) = token {
        req = req.header("x-bonsai-routing-token", t);
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "remote routing events endpoint returned HTTP {}",
            resp.status()
        );
    }
    let body: serde_json::Value = resp.json().await?;
    if body
        .get("projection_semantics_version")
        .and_then(|v| v.as_u64())
        != Some(ROUTING_PROJECTION_VERSION)
    {
        anyhow::bail!(
            "remote routing service does not provide compatible snapshot retirement semantics"
        );
    }
    let latest_seq = body
        .get("durable_sequence")
        .and_then(|v| v.as_u64())
        .unwrap_or(cp.last_applied_sequence);
    let pending: Vec<rbmp_rib::event::RibEvent> = body
        .get("events")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();

    apply_routing_events_to_graph(cp, pending, latest_seq, store).await
}

async fn apply_routing_events_to_graph(
    mut cp: RoutingProjectionCheckpoint,
    pending: Vec<rbmp_rib::event::RibEvent>,
    latest_seq: u64,
    store: &crate::graph::GraphStore,
) -> anyhow::Result<ProjectionStepReport> {
    let mut newly_applied = 0usize;

    for ev in pending {
        if ev.event_key.sequence <= cp.last_applied_sequence {
            continue;
        }
        let seq = ev.event_key.sequence;
        // Exporter-scoped projection id: two BMP exporters behind one
        // NAT address never share speaker/peer summary state.
        let spk_str = speaker_projection_id(&ev);
        let src_inst = ev.event_key.source.exporter_instance.clone();

        match &ev.payload {
            RibEventPayload::SpeakerUp {
                sys_name,
                sys_descr,
            } => {
                let entry = cp.speaker_summaries.entry(spk_str.clone()).or_default();
                entry.speaker_addr = spk_str;
                entry.exporter_instance = src_inst.clone();
                entry.exporter_id = ev.event_key.source.exporter_id.clone();
                entry.device_id = ev.event_key.source.device_id.clone();
                entry.sys_name = sys_name.clone();
                entry.sys_descr = sys_descr.clone();
                entry.session_epoch = ev.event_key.source.session_epoch;
                entry.is_up = true;
                entry.last_event_sequence = seq;
            }
            RibEventPayload::SpeakerDown { .. } => {
                let entry = cp.speaker_summaries.entry(spk_str.clone()).or_default();
                entry.speaker_addr = spk_str;
                entry.exporter_instance = src_inst.clone();
                entry.exporter_id = ev.event_key.source.exporter_id.clone();
                entry.device_id = ev.event_key.source.device_id.clone();
                entry.session_epoch = ev.event_key.source.session_epoch;
                entry.is_up = false;
                entry.last_event_sequence = seq;
            }
            RibEventPayload::PeerUp {
                peer_header,
                local_asn,
                remote_asn,
                ..
            } => {
                let peer_key = PeerKey::from_header(ev.speaker, &src_inst, peer_header);
                let pk_id = format!(
                    "{}|{}|{}|{}",
                    spk_str,
                    peer_header.peer_type as u8,
                    peer_key.distinguisher_string(),
                    peer_header.peer_address
                );
                let ps = cp.peer_summaries.entry(pk_id.clone()).or_default();
                ps.peer_key_id = pk_id;
                ps.speaker_addr = spk_str.clone();
                ps.exporter_instance = src_inst.clone();
                ps.peer_addr = peer_header.peer_address.to_string();
                ps.peer_as = *remote_asn;
                ps.peer_type = peer_header.peer_type as u8;
                ps.peer_distinguisher = peer_key.distinguisher_string();
                ps.state = "UP".to_string();
                ps.last_event_sequence = seq;

                // Project session transition into GraphStore without raw route churn
                let ts_ns = ev.occurred_at.timestamp_nanos_opt().unwrap_or(0);
                let rd_u64 = u64::from_be_bytes(peer_header.peer_distinguisher);
                let bmp_ev = crate::streaming::bmp::BmpEvent {
                    timestamp_ns: ts_ns,
                    collector_peer: format!("{}:11019", spk_str),
                    message_type: "peer_up".to_string(),
                    peer_type: peer_header.peer_type as u8,
                    peer_flags: peer_header.peer_flags.0,
                    router_distinguisher: rd_u64,
                    router_address: spk_str.clone(),
                    peer_address: ps.peer_addr.clone(),
                    peer_as: *remote_asn,
                    peer_bgp_id: peer_header.peer_bgp_id.to_string(),
                    session_state: "Established".to_string(),
                    route_entries: Vec::new(),
                    raw_len: 48,
                    peer_down_reason: None,
                    peer_down_reason_name: None,
                    stats: Vec::new(),
                    sys_name: None,
                    sys_descr: None,
                    termination_reason: None,
                    termination_reason_name: None,
                    rib_type: Some(format!("{:?}", peer_header.rib_type)),
                    peer_up_info: None,
                    init_admin_string: None,
                    termination_message: None,
                };
                let _ = store
                    .write(crate::telemetry::TelemetryUpdate {
                        timestamp_ns: ts_ns,
                        target: spk_str,
                        vendor: "unknown".to_string(),
                        hostname: String::new(),
                        role: "router".to_string(),
                        site: String::new(),
                        path: "streaming/bmp/peer-up".to_string(),
                        value: serde_json::to_value(&bmp_ev).unwrap_or_default(),
                    })
                    .await;
                let _ = local_asn;
            }
            RibEventPayload::PeerDown {
                peer_header,
                reason,
            } => {
                let peer_key = PeerKey::from_header(ev.speaker, &src_inst, peer_header);
                let pk_id = format!(
                    "{}|{}|{}|{}",
                    spk_str,
                    peer_header.peer_type as u8,
                    peer_key.distinguisher_string(),
                    peer_header.peer_address
                );
                let ps = cp.peer_summaries.entry(pk_id.clone()).or_default();
                ps.peer_key_id = pk_id;
                ps.speaker_addr = spk_str.clone();
                ps.exporter_instance = src_inst.clone();
                ps.peer_addr = peer_header.peer_address.to_string();
                ps.peer_as = peer_header.peer_as;
                ps.peer_type = peer_header.peer_type as u8;
                ps.peer_distinguisher = peer_key.distinguisher_string();
                ps.state = "DOWN".to_string();
                ps.active_prefixes.clear();
                ps.eor_families.clear();
                ps.last_event_sequence = seq;

                let ts_ns = ev.occurred_at.timestamp_nanos_opt().unwrap_or(0);
                let rd_u64 = u64::from_be_bytes(peer_header.peer_distinguisher);
                let bmp_ev = crate::streaming::bmp::BmpEvent {
                    timestamp_ns: ts_ns,
                    collector_peer: format!("{}:11019", spk_str),
                    message_type: "peer_down".to_string(),
                    peer_type: peer_header.peer_type as u8,
                    peer_flags: peer_header.peer_flags.0,
                    router_distinguisher: rd_u64,
                    router_address: spk_str.clone(),
                    peer_address: ps.peer_addr.clone(),
                    peer_as: peer_header.peer_as,
                    peer_bgp_id: peer_header.peer_bgp_id.to_string(),
                    session_state: "Down".to_string(),
                    route_entries: Vec::new(),
                    raw_len: 48,
                    peer_down_reason: Some(1),
                    peer_down_reason_name: Some(reason.clone()),
                    stats: Vec::new(),
                    sys_name: None,
                    sys_descr: None,
                    termination_reason: None,
                    termination_reason_name: None,
                    rib_type: Some(format!("{:?}", peer_header.rib_type)),
                    peer_up_info: None,
                    init_admin_string: None,
                    termination_message: None,
                };
                let _ = store
                    .write(crate::telemetry::TelemetryUpdate {
                        timestamp_ns: ts_ns,
                        target: spk_str,
                        vendor: "unknown".to_string(),
                        hostname: String::new(),
                        role: "router".to_string(),
                        site: String::new(),
                        path: "streaming/bmp/peer-down".to_string(),
                        value: serde_json::to_value(&bmp_ev).unwrap_or_default(),
                    })
                    .await;
            }
            RibEventPayload::EndOfRib {
                peer_header,
                afi_safi,
                retired_routes,
            } => {
                let peer_key = PeerKey::from_header(ev.speaker, &src_inst, peer_header);
                let pk_id = format!(
                    "{}|{}|{}|{}",
                    spk_str,
                    peer_header.peer_type as u8,
                    peer_key.distinguisher_string(),
                    peer_header.peer_address
                );
                let ps = cp.peer_summaries.entry(pk_id.clone()).or_default();
                ps.peer_key_id = pk_id;
                ps.speaker_addr = spk_str.clone();
                ps.exporter_instance = src_inst.clone();
                ps.peer_addr = peer_header.peer_address.to_string();
                ps.peer_as = peer_header.peer_as;
                ps.peer_type = peer_header.peer_type as u8;
                ps.peer_distinguisher = peer_key.distinguisher_string();
                ps.eor_families.insert(afi_safi.clone());
                for retired in retired_routes {
                    let route_id = format!(
                        "{:?}|{}|{:?}",
                        retired.route_key.view.rib_type, retired.prefix, retired.route_key.path_id
                    );
                    ps.active_prefixes.remove(&route_id);
                }
                ps.last_event_sequence = seq;
            }
            RibEventPayload::RouteChange(rc) => {
                let pk_id = format!(
                    "{}|{}|{}|{}",
                    spk_str,
                    rc.peer_header.peer_type as u8,
                    rc.peer_key.distinguisher_string(),
                    rc.peer_header.peer_address
                );
                let ps = cp.peer_summaries.entry(pk_id.clone()).or_default();
                ps.peer_key_id = pk_id;
                ps.speaker_addr = spk_str.clone();
                ps.exporter_instance = src_inst.clone();
                ps.peer_addr = rc.peer_header.peer_address.to_string();
                ps.peer_as = rc.peer_header.peer_as;
                ps.peer_type = rc.peer_header.peer_type as u8;
                ps.peer_distinguisher = rc.peer_key.distinguisher_string();
                let route_id = format!("{:?}|{}|{:?}", rc.rib_type, rc.prefix, rc.path_id);
                match rc.action {
                    RouteAction::Announce => {
                        ps.active_prefixes.insert(route_id);
                        ps.announce_events_count += 1;
                    }
                    RouteAction::Withdraw => {
                        ps.active_prefixes.remove(&route_id);
                        ps.withdraw_events_count += 1;
                    }
                }
                ps.last_event_sequence = seq;
                // Intentionally do NOT write individual BgpRoute rows into GraphStore!
                cp.skipped_raw_route_churn_count += 1;
            }
            RibEventPayload::Stats { .. } => {}
        }

        cp.last_applied_sequence = seq;
        cp.last_event_id = ev.id.to_string();
        cp.applied_event_count += 1;
        cp.updated_at_ns = Utc::now().timestamp_nanos_opt().unwrap_or(0);
        newly_applied += 1;
    }

    if newly_applied > 0 {
        let record = crate::graph::ConfigItemRecord {
            id: ROUTING_CHECKPOINT_ID.to_string(),
            config_class: ROUTING_CHECKPOINT_CONFIG_CLASS.to_string(),
            vendor: String::new(),
            name: "routing_projection_checkpoint".to_string(),
            version: ROUTING_API_VERSION.to_string(),
            content_json: serde_json::to_string(&cp)?,
            enabled: true,
            created_by: "routing_projector".to_string(),
        };
        store.upsert_config_item(record).await?;
    }

    let lag = latest_seq.saturating_sub(cp.last_applied_sequence);

    Ok(ProjectionStepReport {
        newly_applied_events: newly_applied,
        journal_latest_sequence: latest_seq,
        graph_checkpoint_sequence: cp.last_applied_sequence,
        sequence_lag: lag,
        checkpoint: cp,
    })
}

/// Helper to build a synthetic BMP Initiation + PeerUp + RouteMonitoring sequence for testing.
pub fn sample_bmp_session_frames(
    peer_ip: Ipv4Addr,
    peer_as: u32,
    prefix_octets: [u8; 3],
    local_pref: u32,
) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    // 1. Initiation: sysDescr="FRR 9.1", sysName="edge-a"
    let init = b"\x03\x00\x00\x00\x1b\x04\x00\x01\x00\x07FRR 9.1\x00\x02\x00\x06edge-a".to_vec();

    // 2. PeerUp (type 3): 6-byte BMP + 42-byte PeerHeader + 20-byte local addr/ports + 29-byte sent OPEN + 29-byte recv OPEN = 126 bytes
    let mut peer_hdr = Vec::with_capacity(42);
    peer_hdr.push(0); // GlobalInstance
    peer_hdr.push(0); // flags = 0 (IPv4, 4-byte ASN)
    peer_hdr.extend_from_slice(&[0u8; 8]); // RD = 0
    peer_hdr.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    peer_hdr.extend_from_slice(&peer_ip.octets());
    peer_hdr.extend_from_slice(&peer_as.to_be_bytes());
    peer_hdr.extend_from_slice(&peer_ip.octets()); // BGP ID
    peer_hdr.extend_from_slice(&1700000000u32.to_be_bytes());
    peer_hdr.extend_from_slice(&0u32.to_be_bytes());

    let mut open = Vec::with_capacity(29);
    open.extend_from_slice(&[0xff; 16]);
    open.extend_from_slice(&29u16.to_be_bytes());
    open.push(1); // OPEN
    open.push(4); // version 4
    open.extend_from_slice(&(peer_as as u16).to_be_bytes());
    open.extend_from_slice(&90u16.to_be_bytes());
    open.extend_from_slice(&peer_ip.octets());
    open.push(0); // opt_params_len = 0

    let mut peer_up = Vec::with_capacity(126);
    peer_up.push(3);
    peer_up.extend_from_slice(&126u32.to_be_bytes());
    peer_up.push(3); // PeerUp
    peer_up.extend_from_slice(&peer_hdr);
    peer_up.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192, 0, 2, 254]); // local_addr
    peer_up.extend_from_slice(&179u16.to_be_bytes());
    peer_up.extend_from_slice(&50000u16.to_be_bytes());
    peer_up.extend_from_slice(&open);
    peer_up.extend_from_slice(&open);

    // 3. RouteMonitoring (type 0) announcing prefix_octets/24
    let mut attrs = Vec::new();
    attrs.extend_from_slice(&[0x40, 1, 1, 0]); // ORIGIN = IGP
    attrs.extend_from_slice(&[0x40, 2, 6, 2, 1]); // AS_PATH Sequence(1)
    attrs.extend_from_slice(&peer_as.to_be_bytes());
    attrs.extend_from_slice(&[0x40, 3, 4]); // NEXT_HOP
    attrs.extend_from_slice(&peer_ip.octets());
    attrs.extend_from_slice(&[0x40, 5, 4]); // LOCAL_PREF
    attrs.extend_from_slice(&local_pref.to_be_bytes());

    let nlri = [24, prefix_octets[0], prefix_octets[1], prefix_octets[2]];
    let mut bgp_update = Vec::new();
    bgp_update.extend_from_slice(&[0xff; 16]);
    let bgp_len = (19 + 2 + 2 + attrs.len() + nlri.len()) as u16;
    bgp_update.extend_from_slice(&bgp_len.to_be_bytes());
    bgp_update.push(2); // UPDATE
    bgp_update.extend_from_slice(&0u16.to_be_bytes());
    bgp_update.extend_from_slice(&(attrs.len() as u16).to_be_bytes());
    bgp_update.extend_from_slice(&attrs);
    bgp_update.extend_from_slice(&nlri);

    let rm_len = (6 + 42 + bgp_update.len()) as u32;
    let mut rm = Vec::with_capacity(rm_len as usize);
    rm.push(3);
    rm.extend_from_slice(&rm_len.to_be_bytes());
    rm.push(0); // RouteMonitoring
    rm.extend_from_slice(&peer_hdr);
    rm.extend_from_slice(&bgp_update);

    (init, peer_up, rm)
}

pub mod diagnostics;
pub use diagnostics::*;
pub mod migration;
pub mod scenario_pack;
pub use scenario_pack::*;

/// Build a canonical 6-byte BMP RouteMonitoring (type 0) withdrawal frame for `prefix_octets/24`.
pub fn sample_bmp_withdraw_frame(
    peer_ip: Ipv4Addr,
    peer_as: u32,
    prefix_octets: [u8; 3],
) -> Vec<u8> {
    let mut peer_hdr = Vec::with_capacity(42);
    peer_hdr.push(0); // GlobalInstance
    peer_hdr.push(0); // flags = 0 (IPv4, 4-byte ASN)
    peer_hdr.extend_from_slice(&[0u8; 8]);
    peer_hdr.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    peer_hdr.extend_from_slice(&peer_ip.octets());
    peer_hdr.extend_from_slice(&peer_as.to_be_bytes());
    peer_hdr.extend_from_slice(&peer_ip.octets());
    peer_hdr.extend_from_slice(&1700000010u32.to_be_bytes());
    peer_hdr.extend_from_slice(&0u32.to_be_bytes());

    let withdrawn = [24, prefix_octets[0], prefix_octets[1], prefix_octets[2]];
    let mut bgp_update = Vec::new();
    bgp_update.extend_from_slice(&[0xff; 16]);
    let bgp_len = (19 + 2 + withdrawn.len() + 2) as u16;
    bgp_update.extend_from_slice(&bgp_len.to_be_bytes());
    bgp_update.push(2); // UPDATE
    bgp_update.extend_from_slice(&(withdrawn.len() as u16).to_be_bytes());
    bgp_update.extend_from_slice(&withdrawn);
    bgp_update.extend_from_slice(&0u16.to_be_bytes()); // total_path_attr_len = 0

    let rm_len = (6 + 42 + bgp_update.len()) as u32;
    let mut rm = Vec::with_capacity(rm_len as usize);
    rm.push(3);
    rm.extend_from_slice(&rm_len.to_be_bytes());
    rm.push(0); // RouteMonitoring
    rm.extend_from_slice(&peer_hdr);
    rm.extend_from_slice(&bgp_update);
    rm
}

/// Compute a deterministic SHA-256 digest of all active routes in `RoutingServiceState`
/// sorted by `(speaker_addr, peer_address, peer_distinguisher, view, prefix, path_id, next_hop, local_pref, as_path)`.
pub async fn compute_rib_digest(state: &RoutingServiceState) -> (usize, String) {
    use sha2::{Digest, Sha256};
    let rib = state.rib.read().await;
    let mut canonical_rows = Vec::new();
    for spk in rib.speakers() {
        let speaker_key = SpeakerKey::for_source(&spk.source);
        for (peer_key, table) in rib.ribs_for_speaker(speaker_key) {
            for rib_type in [
                RibType::AdjRibInPrePolicy,
                RibType::AdjRibInPostPolicy,
                RibType::LocRib,
                RibType::LocRibFiltered,
                RibType::AdjRibOutPrePolicy,
                RibType::AdjRibOutPostPolicy,
            ] {
                for entry in table.iter_rib(rib_type) {
                    canonical_rows.push(format!(
                        "{}|{}|{}|{}|{:?}|{}|{:?}|{:?}|{:?}|{:?}",
                        spk.speaker_addr,
                        spk.source.exporter_instance,
                        peer_key.peer_address,
                        peer_key.distinguisher_string(),
                        rib_type,
                        entry.prefix,
                        entry.path_id,
                        entry.attributes.next_hop,
                        entry.attributes.local_pref,
                        entry.attributes.as_path.as_ref().map(|p| p.to_string()),
                    ));
                }
            }
        }
    }
    canonical_rows.sort();
    let count = canonical_rows.len();
    let joined = canonical_rows.join("\n");
    let digest = hex::encode(Sha256::digest(joined.as_bytes()));
    (count, digest)
}
