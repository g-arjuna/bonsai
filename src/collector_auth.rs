//! BN-021g — authenticated collector identity and scoped ingest
//! authorization for the collector→core gRPC channel.
//!
//! Trust model (three independent layers):
//!
//! 1. **Transport authentication** — the peer certificate is extracted
//!    from tonic's `TlsConnectInfo` request extension, populated by the
//!    TLS layer only after a completed handshake against the configured
//!    client CA. RPC metadata and payload fields are never identity.
//! 2. **Credential→collector binding** — the `"sha256:<hex>"` leaf
//!    fingerprint must match `exporter_bindings.match_ingress_credential`
//!    on an ACTIVE binding with `protocol = "collector"` and
//!    `exporter_id` equal to the claimed `collector_id`. A valid
//!    certificate alone grants nothing; claiming another collector's id
//!    fails because the binding pins both credential and identity.
//! 3. **Capability authorization** — non-ingest RPCs require an active
//!    credential-pinned binding for their capability protocol
//!    (`"core-admin"`, `"sidecar"`); read-only RPCs require any active
//!    pinned binding. Revocation takes effect within
//!    [`AUTHZ_CACHE_TTL`] because decisions are cached briefly, never
//!    for the connection lifetime.
//!
//! Plaintext mode (no TLS peer certificate — development deployments
//! without `runtime.tls`) is the explicit legacy compatibility path:
//! every check passes. When server TLS is enabled the handshake already
//! requires a client certificate, so `None` principals cannot occur.
//!
//! This slice authenticates the FORWARDING COLLECTOR only. It does not
//! authenticate downstream exporters: observations inside an authorized
//! stream remain provisional evidence exactly as BN-021d/e/f define.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tonic::{Request, Status};

use crate::sqlite_store::SqliteStore;
use crate::telemetry_identity::{IngressAuthMethod, VerifiedIngressPrincipal};

/// `exporter_bindings.protocol` value authorizing the collector ingest
/// plane (telemetry/detection ingest, registration, heartbeat). The
/// binding's `exporter_id` is the authorized `collector_id`.
pub const COLLECTOR_PROTOCOL: &str = "collector";
/// Capability protocol for privileged core RPCs (device/site/credential
/// mutation, ad-hoc query, detection/remediation writes).
pub const CORE_ADMIN_PROTOCOL: &str = "core-admin";
/// Capability protocol for sidecar registration and heartbeat.
pub const SIDECAR_PROTOCOL: &str = "sidecar";

/// Authorization decisions are cached this long so revocation and
/// binding changes take effect within a bounded window without a
/// database lookup per telemetry message.
const AUTHZ_CACHE_TTL: Duration = Duration::from_secs(5);
/// Bound on cached decisions; on overflow the cache is cleared rather
/// than growing unboundedly (next lookup re-reads the store).
const AUTHZ_CACHE_CAPACITY: usize = 4096;

/// A granted collector-ingest authorization: the transport-attested
/// principal plus the trusted binding that authorized the claimed
/// `collector_id`. Carries scope/device provenance only — it confers
/// NO authority over downstream observation identity.
#[derive(Debug, Clone)]
pub struct CollectorGrant {
    pub fingerprint: String,
    pub collector_id: String,
    pub binding_id: String,
    pub scope: String,
    pub device_id: String,
}

/// Shared authorization engine for the gRPC service. One instance per
/// `BonsaiService`; the cache is process-local and rebuilt after
/// restart (bindings themselves are durable in SQLite).
pub struct CollectorAuthz {
    store: Option<Arc<SqliteStore>>,
    /// (fingerprint, subject) → (decision, evaluated_at). `subject` is
    /// the claimed collector id for ingest or `"cap:<protocol>"` /
    /// `"any"` for capability checks. `None` decision = recent denial.
    cache: Mutex<HashMap<(String, String), (Option<CollectorGrant>, Instant)>>,
}

impl CollectorAuthz {
    pub fn new(store: Option<Arc<SqliteStore>>) -> Self {
        Self {
            store,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Extract the transport-attested peer principal from tonic request
    /// extensions. `TlsConnectInfo` is populated by tonic's TLS acceptor
    /// only after the handshake verified the client certificate against
    /// the configured client CA — payload and metadata fields are never
    /// consulted. Returns `None` for plaintext connections.
    pub fn principal<T>(req: &Request<T>) -> Option<VerifiedIngressPrincipal> {
        use tonic::transport::server::{TcpConnectInfo, TlsConnectInfo};
        let tls = req.extensions().get::<TlsConnectInfo<TcpConnectInfo>>()?;
        let certs = tls.peer_certs()?;
        let leaf = certs.first()?;
        let digest = Sha256::digest(leaf.as_ref());
        Some(VerifiedIngressPrincipal::from_transport_attestation(
            IngressAuthMethod::MutualTls,
            format!("sha256:{digest:x}"),
        ))
    }

    fn bindings(
        &self,
        protocol: Option<&str>,
    ) -> Result<Vec<crate::telemetry_identity::TrustedBinding>, Status> {
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| Status::unavailable("identity store unavailable"))?;
        store
            .list_exporter_bindings(None, protocol, false)
            .map_err(|e| {
                tracing::error!(%e, "collector authz binding lookup failed");
                Status::internal("identity store error")
            })
    }

    fn cached(&self, fp: &str, subject: &str) -> Option<Option<CollectorGrant>> {
        let cache = self.cache.lock().unwrap();
        cache
            .get(&(fp.to_string(), subject.to_string()))
            .and_then(|(decision, at)| (at.elapsed() < AUTHZ_CACHE_TTL).then(|| decision.clone()))
    }

    fn store_decision(&self, fp: &str, subject: &str, decision: Option<CollectorGrant>) {
        let mut cache = self.cache.lock().unwrap();
        if cache.len() >= AUTHZ_CACHE_CAPACITY {
            cache.clear();
        }
        cache.insert(
            (fp.to_string(), subject.to_string()),
            (decision, Instant::now()),
        );
    }

    /// Authorize a claimed `collector_id` for the ingest plane. The
    /// credential fingerprint AND the collector identity must both be
    /// pinned by the same active binding — a certificate authorized for
    /// collector A cannot claim collector B.
    pub fn authorize_collector(
        &self,
        principal: &VerifiedIngressPrincipal,
        collector_id: &str,
    ) -> Result<CollectorGrant, Status> {
        let fp = principal.credential_fingerprint();
        let subject = format!("collector:{collector_id}");
        if let Some(hit) = self.cached(fp, &subject) {
            return hit.ok_or_else(|| {
                metrics::counter!("bonsai_collector_authz_total", "result" => "denied_cached")
                    .increment(1);
                Status::permission_denied(format!(
                    "collector '{collector_id}' is not authorized for this credential"
                ))
            });
        }

        let bindings = self.bindings(Some(COLLECTOR_PROTOCOL))?;
        let grant = bindings
            .iter()
            .find(|b| {
                b.is_active()
                    && b.exporter_id == collector_id
                    && b.match_ingress_credential.as_deref() == Some(fp)
            })
            .map(|b| CollectorGrant {
                fingerprint: fp.to_string(),
                collector_id: collector_id.to_string(),
                binding_id: b.binding_id.clone(),
                scope: b.scope.clone(),
                device_id: b.device_id.clone(),
            });

        let result = grant.clone();
        self.store_decision(fp, &subject, grant);
        match result {
            Some(g) => {
                metrics::counter!("bonsai_collector_authz_total", "result" => "allowed")
                    .increment(1);
                tracing::debug!(
                    collector_id = %g.collector_id,
                    binding_id = %g.binding_id,
                    scope = %g.scope,
                    "collector ingest authorized"
                );
                Ok(g)
            }
            None => {
                metrics::counter!("bonsai_collector_authz_total", "result" => "denied")
                    .increment(1);
                tracing::warn!(
                    collector_id,
                    fingerprint = fp,
                    "collector ingest denied: no active credential-pinned binding"
                );
                Err(Status::permission_denied(format!(
                    "collector '{collector_id}' is not authorized for this credential"
                )))
            }
        }
    }

    /// Authorize a capability-class RPC (admin mutations, sidecar).
    /// Any ACTIVE binding pinning this credential with the required
    /// protocol authorizes the capability.
    pub fn authorize_capability(
        &self,
        principal: &VerifiedIngressPrincipal,
        protocol: &str,
    ) -> Result<(), Status> {
        let fp = principal.credential_fingerprint();
        let subject = format!("cap:{protocol}");
        if let Some(hit) = self.cached(fp, &subject) {
            return hit.map(|_| ()).ok_or_else(|| {
                Status::permission_denied(format!(
                    "credential is not authorized for '{protocol}' operations"
                ))
            });
        }

        let bindings = self.bindings(Some(protocol))?;
        let allowed = bindings
            .iter()
            .any(|b| b.is_active() && b.match_ingress_credential.as_deref() == Some(fp));
        // Cache a synthetic grant-shaped marker is unnecessary — store a
        // decision with empty fields purely as an authorization memo.
        self.store_decision(
            fp,
            &subject,
            allowed.then(|| CollectorGrant {
                fingerprint: fp.to_string(),
                collector_id: String::new(),
                binding_id: String::new(),
                scope: String::new(),
                device_id: String::new(),
            }),
        );
        if allowed {
            metrics::counter!("bonsai_collector_authz_total", "result" => "capability_allowed")
                .increment(1);
            Ok(())
        } else {
            metrics::counter!("bonsai_collector_authz_total", "result" => "capability_denied")
                .increment(1);
            tracing::warn!(
                fingerprint = fp,
                protocol,
                "capability denied: no active credential-pinned binding"
            );
            Err(Status::permission_denied(format!(
                "credential is not authorized for '{protocol}' operations"
            )))
        }
    }

    /// Read-plane authorization: the credential must be pinned by at
    /// least one active binding of ANY protocol — i.e. it is a
    /// provisioned identity, not an arbitrary CA-issued certificate.
    pub fn authorize_any_binding(
        &self,
        principal: &VerifiedIngressPrincipal,
    ) -> Result<(), Status> {
        let fp = principal.credential_fingerprint();
        let subject = "any".to_string();
        if let Some(hit) = self.cached(fp, &subject) {
            return hit
                .map(|_| ())
                .ok_or_else(|| Status::permission_denied("credential has no active binding"));
        }

        let bindings = self.bindings(None)?;
        let allowed = bindings
            .iter()
            .any(|b| b.is_active() && b.match_ingress_credential.as_deref() == Some(fp));
        self.store_decision(
            fp,
            &subject,
            allowed.then(|| CollectorGrant {
                fingerprint: fp.to_string(),
                collector_id: String::new(),
                binding_id: String::new(),
                scope: String::new(),
                device_id: String::new(),
            }),
        );
        if allowed {
            Ok(())
        } else {
            metrics::counter!("bonsai_collector_authz_total", "result" => "any_denied")
                .increment(1);
            Err(Status::permission_denied(
                "credential has no active binding",
            ))
        }
    }
}
