//! BN-021g — authenticated collector identity and scoped ingest
//! authorization for the collector→core gRPC channel.
//!
//! Trust model (independent layers, never conflated):
//!
//! 1. **Transport authentication** — the peer certificate is extracted
//!    from tonic's `TlsConnectInfo` request extension, populated by the
//!    TLS layer only after a completed handshake against the configured
//!    client CA. RPC metadata and payload fields are never identity.
//! 2. **Principal attestation** — every authorization decision requires
//!    `VerifiedIngressPrincipal::is_transport_attested()`. A principal
//!    deserialized from journals, API payloads, or any other non-transport
//!    source carries a valid-looking fingerprint but is NEVER authorized.
//! 3. **Credential→identity binding** — the `"sha256:<hex>"` leaf
//!    fingerprint must match `exporter_bindings.match_ingress_credential`
//!    on an ACTIVE binding. Collector ingest additionally requires the
//!    binding's `exporter_id` to equal the claimed `collector_id`, so a
//!    valid certificate cannot claim another collector's id. Ambiguous
//!    matches (same credential+collector id provisioned under multiple
//!    scopes) fail closed — the first row is never selected.
//! 4. **Capability authorization** — an explicit RPC capability matrix
//!    (see [`CAPABILITY_MATRIX`] in `api.rs`): ingest plane per claimed
//!    collector id, `"detection-ingest"` for remote detection submission,
//!    `"operator-read"` for global read RPCs, `"core-admin"` for
//!    administrative mutations and ad-hoc query, `"sidecar"` for sidecar
//!    registration. Collector-scoped reads are derived from the
//!    credential's collector bindings, never from client-supplied labels.
//! 5. **Observation authorization** — authenticating the forwarding
//!    collector is not authority over its payload. `api.rs` decodes and
//!    classifies each observation with the same `TelemetryUpdate::
//!    classify` the graph writer uses, then applies the resulting
//!    authority class: canonical classes (YANG/gNMI, BMP, BGP-LS,
//!    summaries — never path prefixes) require an assigned-device
//!    target with inventory-rewritten metadata; provisional classes
//!    (`*src/` keys) are re-namespaced to the authenticated collector
//!    and remain provisional downstream exactly as BN-021d/e/f define.
//!
//! Revocation: decisions are cached for [`AUTHZ_CACHE_TTL`] (5s) and
//! stamped with the store's authorization epoch — `exporter_bindings`
//! mutations (upsert/revoke) bump the epoch and invalidate every cached
//! decision immediately, so same-process revocation takes effect on the
//! next authorization check. Cross-process binding changes and TTL
//! expiry bound the window at ~5s; long-lived streams revalidate
//! through the same path on [`STREAM_REVALIDATE_INTERVAL`].
//!
//! Runtime policy ([`AuthPolicy`]): `Required` fails closed when no
//! transport principal exists (missing TLS peer, plaintext transport,
//! unattested principal, unavailable identity store). `DevelopmentCompat`
//! is the explicit opt-in legacy path for isolated local development.
//! It is NEVER inferred from the absence of TLS peer information — the
//! deployment chooses it via `runtime.auth_mode`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tonic::{Request, Status};

use crate::sqlite_store::SqliteStore;
use crate::telemetry_identity::{IngressAuthMethod, VerifiedIngressPrincipal};

/// `exporter_bindings.protocol` value authorizing the collector ingest
/// plane (telemetry/detection ingest identity, registration, heartbeat).
/// The binding's `exporter_id` is the authorized `collector_id`.
pub const COLLECTOR_PROTOCOL: &str = "collector";
/// Capability protocol for remote detection submission
/// (`DetectionIngest`). Separate from `"collector"` so ordinary ingest
/// authority never implies detection-authoring authority (G2).
pub const DETECTION_INGEST_PROTOCOL: &str = "detection-ingest";
/// Capability protocol for privileged core RPCs (device/site/credential
/// mutation, ad-hoc query, detection/remediation writes, remediation
/// execution).
pub const CORE_ADMIN_PROTOCOL: &str = "core-admin";
/// Capability protocol for sidecar registration and heartbeat.
pub const SIDECAR_PROTOCOL: &str = "sidecar";
/// Capability protocol for GLOBAL operational reads (full inventory,
/// topology, event stream). Collector-scoped reads do not use this
/// protocol — they are derived from collector bindings (see
/// [`CollectorAuthz::read_scope`]).
pub const OPERATOR_READ_PROTOCOL: &str = "operator-read";

/// Runtime authentication policy for the gRPC service (G5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthPolicy {
    /// Authenticated distributed operation: every RPC requires a
    /// transport-attested principal with an authorized binding. Missing
    /// principals, plaintext transport, unattested principals and
    /// unavailable identity stores all fail closed.
    Required,
    /// Explicit isolated-development compatibility: connections without
    /// a transport principal keep the legacy pass-through behavior.
    /// Must be opted into via `runtime.auth_mode = "development-compat"`.
    DevelopmentCompat,
}

/// Upper bound on cached authorization decisions. Same-process binding
/// mutations invalidate the cache immediately via the store's
/// authorization epoch; this TTL bounds staleness for changes made
/// outside this process (or when no epoch is available). Long-lived
/// streams revalidate on this cadence.
pub const AUTHZ_CACHE_TTL: Duration = Duration::from_secs(5);
/// How often an idle long-lived stream revalidates its authorization.
/// Worst-case revocation visibility for an idle stream is one interval
/// plus one cache TTL (pre-epoch caches); with epoch invalidation the
/// effective bound is ~one interval for same-process changes.
pub const STREAM_REVALIDATE_INTERVAL: Duration = AUTHZ_CACHE_TTL;
/// Bound on cached decisions; on overflow the cache is cleared rather
/// than growing unboundedly (next lookup re-reads the store).
const AUTHZ_CACHE_CAPACITY: usize = 4096;
/// Maximum accepted length for attacker-controlled identity fields
/// (claimed collector ids, credential fingerprints). Bounds cache keys
/// and store query parameters.
const MAX_IDENTITY_FIELD_LEN: usize = 256;
/// Maximum source-event references verified per remote detection.
pub const MAX_DETECTION_EVENT_REFS: usize = 64;

/// Result of read-plane authorization (G3).
#[derive(Debug, Clone)]
pub enum ReadScope {
    /// Credential holds `operator-read`: unrestricted operational reads.
    Global,
    /// Credential maps to collector binding(s): reads are restricted to
    /// devices assigned to those collectors. Global topology/site/event
    /// reads are NOT available in this scope.
    CollectorScoped(Vec<String>),
}

/// A granted collector-ingest authorization: the transport-attested
/// principal plus the trusted binding that authorized the claimed
/// `collector_id`. Fields are private — only the authorization engine
/// can construct one, so application code or deserialized data cannot
/// mint a grant. Carries scope/device provenance only — it confers NO
/// authority over downstream observation identity.
#[derive(Debug, Clone)]
pub struct CollectorGrant {
    fingerprint: String,
    collector_id: String,
    binding_id: String,
    scope: String,
    device_id: String,
    /// `updated_at_ns` of the authorizing binding row — the grant
    /// revision recorded in provenance audits so stale audit records
    /// cannot substitute for the live authorization decision.
    binding_revision: i64,
}

impl CollectorGrant {
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    pub fn collector_id(&self) -> &str {
        &self.collector_id
    }
    pub fn binding_id(&self) -> &str {
        &self.binding_id
    }
    pub fn scope(&self) -> &str {
        &self.scope
    }
    pub fn device_id(&self) -> &str {
        &self.device_id
    }
    pub fn binding_revision(&self) -> i64 {
        self.binding_revision
    }
}

/// Cached authorization decision. `Collector` carries the full grant;
/// `Allowed` memoizes capability/any-binding approvals;
/// `CollectorScope` memoizes the collector-id set behind a scoped read.
#[derive(Clone)]
enum CachedDecision {
    Collector(CollectorGrant),
    Allowed,
    CollectorScope(Vec<String>),
}

/// Shared authorization engine for the gRPC service. One instance per
/// `BonsaiService`; clones share the same store handle and decision
/// cache (used by long-lived streams for bounded revalidation). The
/// cache is process-local and rebuilt after restart (bindings
/// themselves are durable in SQLite).
#[derive(Clone)]
pub struct CollectorAuthz {
    store: Option<Arc<SqliteStore>>,
    /// (fingerprint, subject) → (decision, evaluated_at, authz_epoch).
    /// `subject` is `"collector:<id>"`, `"cap:<protocol>"`, `"any"` or
    /// `"read:scope"`. `None` decision = recent denial. Entries are
    /// valid only while `evaluated_at` is within [`AUTHZ_CACHE_TTL`]
    /// AND the store's authorization epoch is unchanged — binding
    /// mutations (upsert/revoke) invalidate every cached decision
    /// immediately, so revocation takes effect on the next check
    /// rather than after the TTL.
    cache: Arc<Mutex<HashMap<(String, String), (Option<CachedDecision>, Instant, u64)>>>,
}

impl CollectorAuthz {
    pub fn new(store: Option<Arc<SqliteStore>>) -> Self {
        Self {
            store,
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Process-local authorization epoch of the backing store. `0` when
    /// no store is configured (decisions then rely on TTL alone).
    fn authz_epoch(&self) -> u64 {
        self.store.as_ref().map(|s| s.authz_epoch()).unwrap_or(0)
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

    /// Fail-closed attestation gate applied before ANY binding lookup.
    /// A serialized/restored principal has a fingerprint but no live
    /// transport attestation and must never authorize anything.
    fn require_attested(&self, principal: &VerifiedIngressPrincipal) -> Result<String, Status> {
        if !principal.is_transport_attested() {
            metrics::counter!("bonsai_collector_authz_total", "result" => "unattested_denied")
                .increment(1);
            tracing::warn!(
                fingerprint = principal.credential_fingerprint(),
                "authorization denied: principal is not transport-attested"
            );
            return Err(Status::permission_denied(
                "principal is not transport-attested",
            ));
        }
        let fp = principal.credential_fingerprint();
        if fp.len() > MAX_IDENTITY_FIELD_LEN {
            return Err(Status::invalid_argument("credential fingerprint too long"));
        }
        Ok(fp.to_string())
    }

    fn identity_field(value: &str) -> Result<&str, Status> {
        if value.is_empty() || value.len() > MAX_IDENTITY_FIELD_LEN {
            return Err(Status::invalid_argument("invalid collector identity"));
        }
        Ok(value)
    }

    fn cached(&self, fp: &str, subject: &str) -> Option<Option<CachedDecision>> {
        let epoch = self.authz_epoch();
        let cache = self.cache.lock().unwrap();
        cache
            .get(&(fp.to_string(), subject.to_string()))
            .and_then(|(decision, at, entry_epoch)| {
                (at.elapsed() < AUTHZ_CACHE_TTL && *entry_epoch == epoch).then(|| decision.clone())
            })
    }

    /// Store a decision stamped with `epoch` — the authorization epoch
    /// captured BEFORE the backing store was read. If a binding
    /// mutation lands between the epoch capture and this store, the
    /// entry is born stale (its epoch no longer matches) and the next
    /// check re-reads — a lookup begun before revocation can never
    /// cache an obsolete decision under the new epoch.
    fn store_decision(
        &self,
        fp: &str,
        subject: &str,
        decision: Option<CachedDecision>,
        epoch: u64,
    ) {
        let mut cache = self.cache.lock().unwrap();
        if cache.len() >= AUTHZ_CACHE_CAPACITY {
            // Bounded clear: a flood of distinct (credential, subject)
            // pairs cannot grow the map; next lookups re-read the store.
            cache.clear();
            metrics::counter!("bonsai_collector_authz_cache_clears_total").increment(1);
        }
        cache.insert(
            (fp.to_string(), subject.to_string()),
            (decision, Instant::now(), epoch),
        );
    }

    fn store(&self) -> Result<&SqliteStore, Status> {
        self.store
            .as_ref()
            .map(|s| s.as_ref())
            .ok_or_else(|| Status::unavailable("identity store unavailable"))
    }

    /// Authorize a claimed `collector_id` for the ingest plane. The
    /// credential fingerprint AND the collector identity must both be
    /// pinned by the same active binding — a certificate authorized for
    /// collector A cannot claim collector B. Multiple distinct matching
    /// bindings (same credential+id provisioned under several scopes)
    /// are ambiguous and fail closed.
    pub fn authorize_collector(
        &self,
        principal: &VerifiedIngressPrincipal,
        collector_id: &str,
    ) -> Result<CollectorGrant, Status> {
        let fp = self.require_attested(principal)?;
        Self::identity_field(collector_id)?;
        let subject = format!("collector:{collector_id}");
        if let Some(hit) = self.cached(&fp, &subject) {
            return match hit {
                Some(CachedDecision::Collector(g)) => Ok(g),
                _ => Err(self.denied(&fp, collector_id, "denied_cached")),
            };
        }

        // Capture the epoch BEFORE the store read so a concurrent
        // mutation cannot stamp a stale decision as fresh.
        let epoch = self.authz_epoch();
        let bindings = self
            .store()?
            .find_credential_bindings(Some(COLLECTOR_PROTOCOL), Some(collector_id), &fp)
            .map_err(|e| {
                tracing::error!(%e, "collector authz binding lookup failed");
                Status::internal("identity store error")
            })?;
        let active: Vec<_> = bindings.iter().filter(|b| b.is_active()).collect();

        let grant = match active.len() {
            0 => None,
            1 => {
                let b = active[0];
                Some(CollectorGrant {
                    fingerprint: fp.clone(),
                    collector_id: collector_id.to_string(),
                    binding_id: b.binding_id.clone(),
                    scope: b.scope.clone(),
                    device_id: b.device_id.clone(),
                    binding_revision: b.updated_at_ns,
                })
            }
            _ => {
                // Ambiguous provisioning: never select the first row.
                metrics::counter!("bonsai_collector_authz_total", "result" => "ambiguous_denied")
                    .increment(1);
                tracing::warn!(
                    collector_id,
                    fingerprint = %fp,
                    matches = active.len(),
                    "collector authorization denied: ambiguous credential bindings"
                );
                self.store_decision(&fp, &subject, None, epoch);
                return Err(Status::permission_denied(format!(
                    "collector '{collector_id}' authorization is ambiguous"
                )));
            }
        };

        // BN-021g R4 global uniqueness: even when THIS credential's
        // (credential, collector id) pair is unique, the collector
        // identity must be globally unique — one active binding across
        // all scopes/credentials — and the credential must map to at
        // most one collector identity. Ambiguous pre-existing rows
        // (written before enforcement, or via direct DB edits) fail
        // closed for EVERY claimant rather than silently picking a row.
        if grant.is_some() && self.collector_identity_ambiguous(&fp, collector_id)? {
            metrics::counter!("bonsai_collector_authz_total", "result" => "ambiguous_denied")
                .increment(1);
            tracing::warn!(
                collector_id,
                fingerprint = %fp,
                "collector authorization denied: globally ambiguous collector identity"
            );
            self.store_decision(&fp, &subject, None, epoch);
            return Err(Status::permission_denied(format!(
                "collector '{collector_id}' authorization is ambiguous"
            )));
        }

        let result = grant.clone();
        self.store_decision(&fp, &subject, grant.map(CachedDecision::Collector), epoch);
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
            None => Err(self.denied(&fp, collector_id, "denied")),
        }
    }

    /// BN-021g R4 global uniqueness probe. Returns `Ok(true)` when the
    /// collector identity is claimed by more than one ACTIVE
    /// `protocol='collector'` binding across all scopes/credentials, or
    /// when the credential itself is bound to more than one distinct
    /// collector identity — both are ambiguous and must fail closed.
    /// Store failures propagate as retryable `Status::internal`.
    fn collector_identity_ambiguous(&self, fp: &str, collector_id: &str) -> Result<bool, Status> {
        let store = self.store()?;
        let identity_rows = store
            .collector_bindings_for_exporter(collector_id)
            .map_err(|e| {
                tracing::error!(%e, "collector authz identity lookup failed");
                Status::internal("identity store error")
            })?;
        if identity_rows.len() != 1 {
            return Ok(true);
        }
        let cred_rows = store.collector_bindings_for_credential(fp).map_err(|e| {
            tracing::error!(%e, "collector authz credential lookup failed");
            Status::internal("identity store error")
        })?;
        let distinct_ids: std::collections::HashSet<&str> =
            cred_rows.iter().map(|b| b.exporter_id.as_str()).collect();
        Ok(distinct_ids.len() > 1)
    }

    fn denied(&self, fp: &str, collector_id: &str, result: &str) -> Status {
        metrics::counter!("bonsai_collector_authz_total", "result" => result.to_string())
            .increment(1);
        tracing::warn!(
            collector_id,
            fingerprint = fp,
            "collector ingest denied: no unique active credential-pinned binding"
        );
        Status::permission_denied(format!(
            "collector '{collector_id}' is not authorized for this credential"
        ))
    }

    /// Authorize a capability-class RPC (admin mutations, sidecar,
    /// detection submission, global reads). Any ACTIVE binding pinning
    /// this credential with the required protocol authorizes the
    /// capability; multiple matches are not ambiguous for a boolean
    /// capability.
    pub fn authorize_capability(
        &self,
        principal: &VerifiedIngressPrincipal,
        protocol: &str,
    ) -> Result<(), Status> {
        let fp = self.require_attested(principal)?;
        Self::identity_field(protocol)?;
        let subject = format!("cap:{protocol}");
        if let Some(hit) = self.cached(&fp, &subject) {
            return match hit {
                Some(CachedDecision::Allowed) => Ok(()),
                _ => Err(Status::permission_denied(format!(
                    "credential is not authorized for '{protocol}' operations"
                ))),
            };
        }

        let epoch = self.authz_epoch();
        let allowed = self
            .store()?
            .credential_binding_permits(Some(protocol), &fp)
            .map_err(|e| {
                tracing::error!(%e, "collector authz capability lookup failed");
                Status::internal("identity store error")
            })?;
        self.store_decision(
            &fp,
            &subject,
            allowed.then_some(CachedDecision::Allowed),
            epoch,
        );
        if allowed {
            metrics::counter!("bonsai_collector_authz_total", "result" => "capability_allowed")
                .increment(1);
            Ok(())
        } else {
            metrics::counter!("bonsai_collector_authz_total", "result" => "capability_denied")
                .increment(1);
            tracing::warn!(
                fingerprint = %fp,
                protocol,
                "capability denied: no active credential-pinned binding"
            );
            Err(Status::permission_denied(format!(
                "credential is not authorized for '{protocol}' operations"
            )))
        }
    }

    /// Read-plane authorization for RPCs that require ANY active
    /// credential-pinned binding (collector discovery of the active
    /// core). This is deliberately narrow: it reveals only leadership
    /// metadata, not inventory.
    pub fn authorize_any_binding(
        &self,
        principal: &VerifiedIngressPrincipal,
    ) -> Result<(), Status> {
        let fp = self.require_attested(principal)?;
        let subject = "any".to_string();
        if let Some(hit) = self.cached(&fp, &subject) {
            return match hit {
                Some(CachedDecision::Allowed) => Ok(()),
                _ => Err(Status::permission_denied(
                    "credential has no active binding",
                )),
            };
        }

        let epoch = self.authz_epoch();
        let allowed = self
            .store()?
            .credential_binding_permits(None, &fp)
            .map_err(|e| {
                tracing::error!(%e, "collector authz binding lookup failed");
                Status::internal("identity store error")
            })?;
        self.store_decision(
            &fp,
            &subject,
            allowed.then_some(CachedDecision::Allowed),
            epoch,
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

    /// Resolve the read scope for a credential (G3). Global operational
    /// reads require an explicit `operator-read` binding; otherwise the
    /// credential's ACTIVE collector bindings define a collector-scoped
    /// read limited to assigned devices. Exporter-only credentials
    /// (BMP/OTLP/etc.) get neither.
    pub fn read_scope(&self, principal: &VerifiedIngressPrincipal) -> Result<ReadScope, Status> {
        let fp = self.require_attested(principal)?;

        if self
            .authorize_capability(principal, OPERATOR_READ_PROTOCOL)
            .is_ok()
        {
            return Ok(ReadScope::Global);
        }

        let subject = "read:scope".to_string();
        if let Some(hit) = self.cached(&fp, &subject) {
            return match hit {
                Some(CachedDecision::CollectorScope(ids)) => Ok(ReadScope::CollectorScoped(ids)),
                _ => Err(Status::permission_denied(
                    "credential has no read capability",
                )),
            };
        }

        let epoch = self.authz_epoch();
        let bindings = self
            .store()?
            .collector_bindings_for_credential(&fp)
            .map_err(|e| {
                tracing::error!(%e, "collector authz scope lookup failed");
                Status::internal("identity store error")
            })?;
        // H5: the same fail-closed ambiguity rule as ingest — a
        // collector id pinned by MORE THAN ONE active binding for this
        // credential (e.g. the same id provisioned under multiple
        // scopes) is ambiguous and must not silently gain scoped read
        // access. Unambiguous ids remain readable.
        let mut binding_counts: HashMap<&str, usize> = HashMap::new();
        for b in &bindings {
            *binding_counts.entry(b.exporter_id.as_str()).or_default() += 1;
        }
        let mut ids: Vec<String> = bindings
            .iter()
            .filter(|b| binding_counts.get(b.exporter_id.as_str()) == Some(&1))
            .map(|b| b.exporter_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        if ids.is_empty() {
            self.store_decision(&fp, &subject, None, epoch);
            metrics::counter!("bonsai_collector_authz_total", "result" => "read_denied")
                .increment(1);
            return Err(Status::permission_denied(
                "credential has no read capability",
            ));
        }
        self.store_decision(
            &fp,
            &subject,
            Some(CachedDecision::CollectorScope(ids.clone())),
            epoch,
        );
        Ok(ReadScope::CollectorScoped(ids))
    }
}
