use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::{collections::HashMap, time::Duration};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use anyhow::{Context, Result};
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;
use tracing::{debug, info, warn};

const BACKOFF_INITIAL: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const BACKOFF_RESET_THRESHOLD: Duration = Duration::from_secs(60);
const SAMPLE_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Bounded grace for orderly shutdown lifecycle delivery. Long enough for
/// the lifecycle consumer to make room under transient saturation, short
/// enough that subscriber teardown cannot hang on a dead consumer.
const LIFECYCLE_SHUTDOWN_SEND_TIMEOUT: Duration = Duration::from_millis(500);
/// Bounded deadline for the initial synchronization sequence after a
/// Subscribe RPC is established. One sync_response=true per
/// SubscriptionList (i.e. per Subscribe RPC stream) completes it; a
/// device that never syncs fails the session rather than holding
/// submitted-but-unverified authority forever.
const INITIAL_SYNC_TIMEOUT: Duration = Duration::from_secs(30);

use crate::proto::gnmi::g_nmi_client::GNmiClient;
use crate::proto::gnmi::{
    CapabilityRequest, Path, PathElem, SubscribeRequest, Subscription, SubscriptionList,
    SubscriptionMode, subscribe_request, subscription_list,
};

use crate::config::TargetConfig;
use crate::config::{AcquisitionMode, SelectedSubscriptionPath};
use crate::event_bus::InProcessBus;
use crate::ingest::TelemetryDebouncer;
use crate::subscription_sessions::{LifecyclePathEntry, SessionLifecycleEvent};
use crate::subscription_status::{SubscriptionPathExpectation, SubscriptionPlan};
use crate::telemetry::TelemetryUpdate;

/// Handle a subscriber uses to report authoritative device-facing
/// session lifecycle transitions. Remote collectors forward these over
/// the `SubscriptionLifecycle` RPC; local acquisition applies them
/// in-process. `authority_epoch`/`assignment_generation` carry the
/// assignment authority the collector applied (0/0 for trusted local
/// acquisition, which bypasses remote authority checks).
#[derive(Clone)]
pub struct LifecycleHandle {
    pub tx: mpsc::Sender<SessionLifecycleEvent>,
    pub authority_epoch: u64,
    pub assignment_generation: u64,
    /// Core-issued collector session generation from the
    /// AssignmentUpdate that established this handle — echoed on every
    /// lifecycle event so the core can order executions across
    /// collector restarts (0 for trusted local acquisition).
    pub collector_session_generation: u64,
}

/// Per-session lifecycle emitter: monotonic transition revisions,
/// stamped with the applicable assignment authority.
pub struct SessionEmitter {
    tx: Option<mpsc::Sender<SessionLifecycleEvent>>,
    collector_id: String,
    target: String,
    plan_id: String,
    session_id: String,
    authority_epoch: u64,
    assignment_generation: u64,
    collector_session_generation: u64,
    /// Per-(collector session, target) execution counter for the
    /// session this emitter reports (0 for plan-level emitters).
    execution_generation: u64,
    seq: u64,
    /// Actual submitted Subscribe manifest + acquisition intent,
    /// attached to `submitted`/`synchronized` transitions so the core
    /// can bind coverage expectations to the real effective plan
    /// (M5.2B review B3). Empty for plan-level emitters.
    submitted_paths: Vec<LifecyclePathEntry>,
    acquisition: String,
    schema_binding: Option<crate::config::SchemaBindingConfig>,
    /// Set once this emitter publishes a terminal state, so teardown
    /// paths can skip a redundant terminal transition.
    terminated: Option<Arc<AtomicBool>>,
}

fn lifecycle_state_terminal(state: &str) -> bool {
    matches!(state, "failed" | "disconnected" | "disabled" | "superseded")
}

impl SessionEmitter {
    /// Plan-level emitter for assignment outcomes decided outside a
    /// subscriber (rejected/failed/disabled assignments).
    pub fn for_plan(handle: &LifecycleHandle, target: &str, plan_id: &str) -> Self {
        Self::new(Some(handle), target, plan_id, "", None)
    }

    fn new(
        lifecycle: Option<&LifecycleHandle>,
        target: &str,
        plan_id: &str,
        session_id: &str,
        terminated: Option<Arc<AtomicBool>>,
    ) -> Self {
        Self {
            tx: lifecycle.map(|h| h.tx.clone()),
            collector_id: String::new(),
            target: target.to_string(),
            plan_id: plan_id.to_string(),
            session_id: session_id.to_string(),
            authority_epoch: lifecycle.map_or(0, |h| h.authority_epoch),
            assignment_generation: lifecycle.map_or(0, |h| h.assignment_generation),
            collector_session_generation: lifecycle.map_or(0, |h| h.collector_session_generation),
            execution_generation: 0,
            seq: 0,
            submitted_paths: Vec::new(),
            acquisition: String::new(),
            schema_binding: None,
            terminated,
        }
    }

    /// Attach the actual submitted manifest + acquisition intent.
    /// Only `submitted`-family transitions carry it; terminal and
    /// plan-level outcomes leave it empty.
    pub fn with_manifest(
        mut self,
        acquisition: AcquisitionMode,
        paths: Vec<LifecyclePathEntry>,
    ) -> Self {
        self.acquisition = acquisition.as_str().to_string();
        self.submitted_paths = paths;
        self
    }

    /// Echo the approved schema-bound identity on every transition this
    /// emitter publishes so the core can reject forged or stale
    /// bindings at the authenticated lifecycle boundary.
    pub fn with_schema_binding(
        mut self,
        schema_binding: Option<crate::config::SchemaBindingConfig>,
    ) -> Self {
        self.schema_binding = schema_binding;
        self
    }

    /// Bind this emitter to its execution generation — the monotonic
    /// per-(collector session, target) counter value minted with the
    /// session_id. Required on every session-scoped emitter.
    pub fn with_execution_generation(mut self, execution_generation: u64) -> Self {
        self.execution_generation = execution_generation;
        self
    }

    fn event(&self, state: &str, seq: u64, detail: &str) -> SessionLifecycleEvent {
        SessionLifecycleEvent {
            collector_id: self.collector_id.clone(),
            target: self.target.clone(),
            plan_id: self.plan_id.clone(),
            session_id: self.session_id.clone(),
            state: state.to_string(),
            transition_seq: seq,
            authority_epoch: self.authority_epoch,
            assignment_generation: self.assignment_generation,
            authority_instance: 0,
            collector_session_generation: self.collector_session_generation,
            execution_generation: self.execution_generation,
            occurred_at_ns: now_unix_ns(),
            detail: detail.to_string(),
            submitted_paths: self.submitted_paths.clone(),
            acquisition: self.acquisition.clone(),
            schema_binding: self.schema_binding.clone(),
        }
    }

    /// Ordered session transition. Blocks on a full lifecycle channel —
    /// lifecycle evidence is bounded but never silently dropped.
    async fn emit(&mut self, state: &str, detail: impl Into<String>) {
        let Some(tx) = &self.tx else { return };
        self.seq += 1;
        let ev = self.event(state, self.seq, &detail.into());
        if let Err(error) = tx.send(ev).await {
            warn!(target = %self.target, %error, "lifecycle channel closed — transition lost");
        } else if lifecycle_state_terminal(state) {
            self.mark_terminated();
        }
    }

    fn mark_terminated(&self) {
        if let Some(flag) = &self.terminated {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// Bounded transition for shutdown/cancellation paths. Teardown gets a
    /// short, finite opportunity to enqueue terminal evidence; it never
    /// waits indefinitely on a saturated or closed lifecycle channel.
    async fn emit_shutdown(&mut self, state: &str, detail: impl Into<String>) {
        let Some(tx) = &self.tx else { return };
        self.seq += 1;
        let ev = self.event(state, self.seq, &detail.into());
        match tx.send_timeout(ev, LIFECYCLE_SHUTDOWN_SEND_TIMEOUT).await {
            Ok(()) => {
                if lifecycle_state_terminal(state) {
                    self.mark_terminated();
                }
            }
            Err(_) => {
                warn!(target = %self.target, "lifecycle channel saturated/closed — shutdown terminal transition dropped");
                metrics::counter!("bonsai_subscription_lifecycle_dropped_total").increment(1);
            }
        }
    }

    /// Plan-level outcome (disabled/rejected/failed plan, no session).
    /// Unordered (seq=0): latest-wins at the registry, never resurrects
    /// a terminated session.
    pub async fn emit_plan(&self, state: &str, detail: impl Into<String>) {
        let Some(tx) = &self.tx else { return };
        let ev = self.event(state, 0, &detail.into());
        if let Err(error) = tx.send(ev).await {
            warn!(target = %self.target, %error, "lifecycle channel closed — plan outcome lost");
        }
    }
}

fn now_unix_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Live device-facing session tracked for teardown-time termination.
#[derive(Clone)]
struct ActiveSession {
    session_id: String,
    plan_id: String,
    execution_generation: u64,
    terminated: Arc<AtomicBool>,
}

pub struct GnmiSubscriber {
    target: String,
    username: Option<String>,
    password: Option<String>,
    /// Overrides the vendor label in logs; model detection still uses Capabilities.
    vendor_hint: Option<String>,
    /// Human-readable hostname for this device (e.g. "srl1"); stored on Device node.
    hostname: Option<String>,
    role: Option<String>,
    site: Option<String>,
    /// Resolved transport security (explicit TLS/mTLS or audited/plaintext
    /// legacy). Never downgraded at connect time.
    conn_security: crate::gnmi_security::ConnSecurity,
    bus: Arc<InProcessBus>,
    debouncer: Option<Arc<TelemetryDebouncer>>,
    subscription_plan_tx: Option<tokio::sync::mpsc::Sender<SubscriptionPlan>>,
    selected_paths: Vec<SelectedSubscriptionPath>,
    acquisition: AcquisitionMode,
    schema_binding: Option<crate::config::SchemaBindingConfig>,
    lifecycle: Option<LifecycleHandle>,
    /// Per-(collector session, target) execution counter — monotonic
    /// across device-facing session mints so every execution has a
    /// distinct deterministic rank. Shared across subscriber
    /// respawns within the same collector session so a restarted
    /// subscriber never reuses an execution generation.
    execution_seq: Arc<AtomicU64>,
    /// Most recently minted device-facing session, used to retire its
    /// authority on shutdown paths that bypass the stream loop.
    active_session: Arc<std::sync::Mutex<Option<ActiveSession>>>,
}

impl GnmiSubscriber {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: impl Into<String>,
        username: Option<String>,
        password: Option<String>,
        vendor_hint: Option<String>,
        hostname: Option<String>,
        role: Option<String>,
        site: Option<String>,
        conn_security: crate::gnmi_security::ConnSecurity,
        bus: Arc<InProcessBus>,
        debouncer: Option<Arc<TelemetryDebouncer>>,
        subscription_plan_tx: Option<tokio::sync::mpsc::Sender<SubscriptionPlan>>,
        selected_paths: Vec<SelectedSubscriptionPath>,
        acquisition: AcquisitionMode,
        lifecycle: Option<LifecycleHandle>,
        execution_seq: Arc<AtomicU64>,
    ) -> Self {
        Self {
            target: target.into(),
            username,
            password,
            vendor_hint,
            hostname,
            role,
            site,
            conn_security,
            bus,
            debouncer,
            subscription_plan_tx,
            selected_paths,
            acquisition,
            schema_binding: None,
            lifecycle,
            execution_seq,
            active_session: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Attach the durable schema-bound approval this subscriber is
    /// authorized to execute. The binding is echoed on lifecycle events
    /// and included in the assignment fingerprint.
    pub fn with_schema_binding(
        mut self,
        schema_binding: Option<crate::config::SchemaBindingConfig>,
    ) -> Self {
        self.schema_binding = schema_binding;
        self
    }

    async fn publish_plan(&self, plan: SubscriptionPlan) {
        if let Some(tx) = &self.subscription_plan_tx
            && let Err(error) = tx.send(plan).await
        {
            warn!(target = %self.target, %error, "failed to publish subscription plan");
        }
    }

    /// Publish a plan-level lifecycle outcome (disabled/rejected/failed)
    /// for plans that own no device-facing session.
    async fn emit_plan_outcome(&self, state: &str, plan_id: &str, detail: &str) {
        if let Some(h) = &self.lifecycle {
            SessionEmitter::new(Some(h), &self.target, plan_id, "", None)
                .with_schema_binding(self.schema_binding.clone())
                .emit_plan(state, detail)
                .await;
        }
    }

    /// Publish a non-submitted plan (rejected/blocked/failed) so the
    /// verifier supersedes any previously tracked plan for this target.
    /// The plan_id is the STABLE desired/effective plan identity —
    /// failure state and detail live in the plan's state/detail fields
    /// and the lifecycle stream, never encoded into plan_id (M5.2B).
    async fn publish_blocked_plan(&self, state: &str, detail: String) {
        let plan_id = acquisition_plan_id(self.acquisition, &[]);
        self.publish_plan(SubscriptionPlan {
            target: self.target.clone(),
            paths: Vec::new(),
            plan_id: plan_id.clone(),
            session_id: String::new(),
            state: state.to_string(),
            acquisition: self.acquisition.as_str().to_string(),
            detail: detail.clone(),
            schema_id: None,
        })
        .await;
        self.emit_plan_outcome(state, &plan_id, &detail).await;
    }

    /// Terminal transition for teardown paths that bypass the stream
    /// loop (shutdown while connecting or backing off). Uses
    /// `transition_seq = u64::MAX` so it always supersedes in-flight
    /// ordered transitions for THIS session key; reconnects mint a
    /// fresh session_id and are unaffected. Skipped when the stream
    /// loop already published a terminal state.
    async fn emit_active_session_terminal(&self, state: &str, detail: &str) {
        let Some(handle) = &self.lifecycle else {
            return;
        };
        let active = self.active_session.lock().unwrap().clone();
        let Some(active) = active else { return };
        if active.terminated.load(Ordering::SeqCst) {
            return;
        }
        let ev = SessionLifecycleEvent {
            collector_id: String::new(),
            target: self.target.clone(),
            plan_id: active.plan_id,
            session_id: active.session_id,
            state: state.to_string(),
            transition_seq: u64::MAX,
            authority_epoch: handle.authority_epoch,
            assignment_generation: handle.assignment_generation,
            authority_instance: 0,
            collector_session_generation: handle.collector_session_generation,
            execution_generation: active.execution_generation,
            occurred_at_ns: now_unix_ns(),
            detail: detail.to_string(),
            submitted_paths: Vec::new(),
            acquisition: String::new(),
            schema_binding: self.schema_binding.clone(),
        };
        match tokio::time::timeout(LIFECYCLE_SHUTDOWN_SEND_TIMEOUT, handle.tx.send(ev)).await {
            Ok(Ok(())) => {
                active.terminated.store(true, Ordering::SeqCst);
            }
            _ => {
                warn!(target = %self.target, "lifecycle channel saturated/closed — shutdown terminal transition dropped");
                metrics::counter!("bonsai_subscription_lifecycle_dropped_total").increment(1);
            }
        }
    }

    pub async fn run_forever(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let mut backoff = BACKOFF_INITIAL;
        loop {
            let start = std::time::Instant::now();
            let mut stream_shutdown = shutdown.clone();
            tokio::select! {
                _ = shutdown.changed() => {
                    info!(target = %self.target, "shutdown signal received");
                    self.emit_active_session_terminal("disconnected", "shutdown")
                        .await;
                    return;
                }
                result = self.subscribe_telemetry(&mut stream_shutdown) => {
                    if let Err(e) = result {
                        warn!(target = %self.target, error = %e, "subscription failed");
                    }
                    metrics::counter!("bonsai_subscriber_reconnects_total",
                        "target" => self.target.clone()).increment(1);
                    if start.elapsed() >= BACKOFF_RESET_THRESHOLD {
                        backoff = BACKOFF_INITIAL;
                    }
                    warn!(target = %self.target, delay = ?backoff, "reconnecting");
                }
            }
            tokio::select! {
                _ = shutdown.changed() => {
                    info!(target = %self.target, "shutdown signal received");
                    self.emit_active_session_terminal("disconnected", "shutdown")
                        .await;
                    return;
                }
                _ = tokio::time::sleep(backoff) => {}
            }
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    }

    pub async fn subscribe_telemetry(
        &self,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        // A schema-bound assignment must carry the exact durable approval
        // and an explicit manifest. Invalid/incomplete/mismatched bindings
        // idle here — never fall back to automatic or capability-derived
        // collection (M6C-2 fail closed).
        if let Some(reason) =
            schema_binding_block_reason_parts(self.schema_binding.as_ref(), self.acquisition, None)
        {
            let plan_id = self
                .schema_binding
                .as_ref()
                .map(|b| b.plan_id.clone())
                .filter(|p| !p.trim().is_empty())
                .unwrap_or_else(|| acquisition_plan_id(self.acquisition, &[]));
            warn!(
                target = %self.target,
                reason,
                "schema-bound acquisition blocked — no Subscribe RPC will be emitted"
            );
            self.publish_plan(SubscriptionPlan {
                target: self.target.clone(),
                paths: Vec::new(),
                plan_id: plan_id.clone(),
                session_id: String::new(),
                state: "rejected".to_string(),
                acquisition: self.acquisition.as_str().to_string(),
                detail: reason.to_string(),
                schema_id: None,
            })
            .await;
            self.emit_plan_outcome("failed", &plan_id, reason).await;
            idle_until_shutdown(shutdown).await;
            return Ok(());
        }

        // Disabled and rejected plans need no device contact at all —
        // decide before connecting so no Capabilities/Subscribe RPC is
        // emitted. (Automatic needs real capabilities for fallback.)
        if self.acquisition != AcquisitionMode::Automatic {
            match build_acquisition_plan(
                self.acquisition,
                &self.selected_paths,
                &ModelCapabilities::fallback(None),
            ) {
                AcquisitionPlan::Disabled { plan_id } => {
                    self.publish_plan(SubscriptionPlan {
                        target: self.target.clone(),
                        paths: Vec::new(),
                        plan_id: plan_id.clone(),
                        session_id: String::new(),
                        state: "disabled".to_string(),
                        acquisition: self.acquisition.as_str().to_string(),
                        detail: "acquisition disabled".to_string(),
                        schema_id: None,
                    })
                    .await;
                    self.emit_plan_outcome("disabled", &plan_id, "acquisition disabled")
                        .await;
                    idle_until_shutdown(shutdown).await;
                    return Ok(());
                }
                AcquisitionPlan::Rejected { plan_id, errors } => {
                    let detail = errors.join("; ");
                    warn!(
                        target = %self.target,
                        plan_id = %plan_id,
                        errors = %detail,
                        "explicit acquisition plan rejected — no Subscribe RPC will be emitted"
                    );
                    self.publish_plan(SubscriptionPlan {
                        target: self.target.clone(),
                        paths: Vec::new(),
                        plan_id: plan_id.clone(),
                        session_id: String::new(),
                        state: "rejected".to_string(),
                        acquisition: self.acquisition.as_str().to_string(),
                        detail: detail.clone(),
                        schema_id: None,
                    })
                    .await;
                    self.emit_plan_outcome("failed", &plan_id, &detail).await;
                    idle_until_shutdown(shutdown).await;
                    return Ok(());
                }
                AcquisitionPlan::Ready { .. } => {}
            }
        }

        // Authorized-transport gate (M5.2A): legacy-inferred plaintext is
        // readable configuration, not an authorized operational channel.
        // Only the explicit laboratory opt-in or a TLS mode may carry a
        // device session — including credential metadata.
        if !self.conn_security.effective.authorized_for_acquisition() {
            warn!(
                target = %self.target,
                security = self.conn_security.effective.label(),
                "legacy plaintext transport not authorized for acquisition \
                 (migration_required) — no RPC will be emitted"
            );
            self.publish_blocked_plan(
                "rejected",
                "migration_required: legacy plaintext transport is not \
                 authorized for device sessions; set security=\"plaintext\" \
                 for an isolated laboratory or configure a TLS mode"
                    .to_string(),
            )
            .await;
            idle_until_shutdown(shutdown).await;
            return Ok(());
        }

        // Credential metadata must be encodable as gRPC headers before
        // any RPC — invalid values fail closed rather than being silently
        // dropped from the request.
        if let Err(error) = crate::gnmi_security::validate_auth_metadata(
            self.username.as_deref(),
            self.password.as_deref(),
        ) {
            warn!(target = %self.target, %error, "credential metadata invalid");
            self.publish_blocked_plan("rejected", format!("{error:#}"))
                .await;
            idle_until_shutdown(shutdown).await;
            return Ok(());
        }

        let channel = self.connect().await?;
        let target = self.target.clone();

        if !self.conn_security.effective.is_tls()
            && (self.username.is_some() || self.password.is_some())
        {
            warn!(
                target = %self.target,
                security = self.conn_security.effective.label(),
                "SECURITY: gNMI Subscribe is sending credentials over a plaintext (non-TLS) channel. \
                 Configure security=\"tls_verify\" or \"mtls\" with ca_cert/tls_domain."
            );
        }

        // Always call Capabilities — encoding and supported models come from the device.
        // vendor_hint overrides the label; when Capabilities fails, it also seeds the fallback.
        let mut bare = GNmiClient::new(channel.clone());
        let caps = detect_capabilities(
            &mut bare,
            &target,
            self.username.as_deref(),
            self.password.as_deref(),
            self.vendor_hint.as_deref(),
        )
        .await;

        let username = self.username.clone();
        let password = self.password.clone();

        #[allow(clippy::result_large_err)]
        let mut client = GNmiClient::with_interceptor(channel, move |mut req: Request<()>| {
            if let Some(ref u) = username
                && let Ok(v) = MetadataValue::try_from(u.as_str())
            {
                req.metadata_mut().insert("username", v);
            }
            if let Some(ref p) = password
                && let Ok(v) = MetadataValue::try_from(p.as_str())
            {
                req.metadata_mut().insert("password", v);
            }
            Ok(req)
        });

        let plan = build_acquisition_plan(self.acquisition, &self.selected_paths, &caps);
        // M5.2B: every device-facing Subscribe session gets a fresh
        // session identity AND a fresh execution generation. The pair
        // (session_id, execution_generation) is the deterministic
        // execution identity: reconnects mint both anew and previous
        // executions are fenced by the core's per-target high-water
        // mark — never by probabilistic structures.
        let execution_generation = self.execution_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let session_id = format!("{}:{}", target, uuid::Uuid::new_v4());
        let (subscriptions, plan_paths, submitted_plan_id) = match plan {
            AcquisitionPlan::Disabled { plan_id } => {
                info!(
                    target = %target,
                    plan_id = %plan_id,
                    acquisition = self.acquisition.as_str(),
                    "acquisition disabled — no Subscribe RPC will be emitted"
                );
                self.publish_plan(SubscriptionPlan {
                    target: target.clone(),
                    paths: Vec::new(),
                    plan_id: plan_id.clone(),
                    session_id: String::new(),
                    state: "disabled".to_string(),
                    acquisition: self.acquisition.as_str().to_string(),
                    detail: "acquisition disabled".to_string(),
                    schema_id: None,
                })
                .await;
                self.emit_plan_outcome("disabled", &plan_id, "acquisition disabled")
                    .await;
                idle_until_shutdown(shutdown).await;
                return Ok(());
            }
            AcquisitionPlan::Rejected { plan_id, errors } => {
                let detail = errors.join("; ");
                warn!(
                    target = %target,
                    plan_id = %plan_id,
                    errors = %detail,
                    "explicit acquisition plan rejected — no Subscribe RPC will be emitted"
                );
                self.publish_plan(SubscriptionPlan {
                    target: target.clone(),
                    paths: Vec::new(),
                    plan_id: plan_id.clone(),
                    session_id: String::new(),
                    state: "rejected".to_string(),
                    acquisition: self.acquisition.as_str().to_string(),
                    detail: detail.clone(),
                    schema_id: None,
                })
                .await;
                self.emit_plan_outcome("failed", &plan_id, &detail).await;
                idle_until_shutdown(shutdown).await;
                return Ok(());
            }
            AcquisitionPlan::Ready {
                subscriptions,
                plan_id,
            } => {
                let plan_paths = subscriptions
                    .iter()
                    .filter_map(subscription_expectation)
                    .collect::<Vec<_>>();
                (subscriptions, plan_paths, plan_id)
            }
        };
        let idle_timeout = sample_stream_idle_timeout(&subscriptions);
        let enforce_idle_timeout = !idle_timeout.is_zero();
        info!(
            target = %target,
            vendor       = %caps.vendor_label,
            encoding     = caps.encoding,
            srl_native   = caps.has_srl_native,
            srl_bfd      = caps.has_srl_native_bfd,
            xr_native    = caps.has_xr_native,
            oc_interfaces = caps.has_oc_interfaces,
            oc_bfd       = caps.has_oc_bfd,
            oc_bgp       = caps.has_oc_bgp,
            paths        = subscriptions.len(),
            plan_id      = %submitted_plan_id,
            acquisition  = self.acquisition.as_str(),
            "subscribing"
        );

        let req = SubscribeRequest {
            request: Some(subscribe_request::Request::Subscribe(SubscriptionList {
                subscription: subscriptions.clone(),
                mode: subscription_list::Mode::Stream as i32,
                encoding: caps.encoding,
                ..Default::default()
            })),
            ..Default::default()
        };

        let mut stream = client
            .subscribe(tokio_stream::once(req))
            .await
            .context("Subscribe RPC failed")?
            .into_inner();

        // The session-bound submitted manifest is NOT published on the
        // plan channel here: it reaches the coverage verifier only
        // through the lifecycle drain AFTER the registry commits the
        // `submitted` transition — the same post-apply contract as the
        // authenticated remote lifecycle RPC (M5.2B review). Publishing
        // it earlier raced the registry apply and could be fenced as
        // stale while an older execution was still the open head.

        // Authoritative lifecycle: a successful Subscribe RPC establishes
        // SUBMISSION only — never synchronization. Initial sync
        // completes when the SubscriptionList's sync_response arrives
        // (bounded by INITIAL_SYNC_TIMEOUT).
        // Track the live session so teardown paths that bypass the
        // stream loop (shutdown during connect/backoff) can still retire
        // its authority with an explicit terminal transition.
        let terminated = Arc::new(AtomicBool::new(false));
        *self.active_session.lock().unwrap() = Some(ActiveSession {
            session_id: session_id.clone(),
            plan_id: submitted_plan_id.clone(),
            execution_generation,
            terminated: Arc::clone(&terminated),
        });
        // The submitted manifest is the ACTUAL SubscriptionList path
        // set (capability-derived for automatic plans, the validated
        // selection for explicit plans) — carried on the authenticated
        // lifecycle stream so the core binds coverage expectations to
        // what was really submitted, never a fallback prediction (B3).
        let manifest = plan_paths
            .iter()
            .map(|e| LifecyclePathEntry {
                path: e.path.clone(),
                origin: e.origin.clone(),
                mode: e.mode.clone(),
                sample_interval_ns: e.sample_interval_ns,
            })
            .collect();
        let mut emitter = SessionEmitter::new(
            self.lifecycle.as_ref(),
            &target,
            &submitted_plan_id,
            &session_id,
            Some(terminated),
        )
        .with_manifest(self.acquisition, manifest)
        .with_schema_binding(self.schema_binding.clone())
        .with_execution_generation(execution_generation);
        emitter.emit("submitted", "").await;
        emitter.emit("synchronizing", "").await;
        // gNMI sync boundary is the SubscriptionList/RPC, NOT each
        // Subscription entry (spec 3.5.1.4/3.5.2.3): one Subscribe RPC
        // carries one SubscriptionList, and the target emits exactly
        // one SyncResponse(true) after the initial updates for ALL
        // paths in that list. Requiring one sync per Subscription
        // would deadlock multi-path subscriptions into
        // INITIAL_SYNC_TIMEOUT.
        let mut pending_sync = 1_u32;
        let sync_sleep =
            tokio::time::sleep_until(tokio::time::Instant::now() + INITIAL_SYNC_TIMEOUT);
        tokio::pin!(sync_sleep);

        loop {
            let next_message = if enforce_idle_timeout {
                tokio::select! {
                    _ = shutdown.changed() => {
                        info!(target = %target, "shutdown signal received during telemetry stream");
                        emitter.emit_shutdown("disconnected", "shutdown").await;
                        return Ok(());
                    }
                    _ = &mut sync_sleep, if pending_sync > 0 => {
                        warn!(target = %target, timeout_secs = INITIAL_SYNC_TIMEOUT.as_secs(),
                            "initial synchronization deadline expired");
                        emitter.emit("failed", "initial_sync_timeout").await;
                        break;
                    }
                    _ = tokio::time::sleep(idle_timeout) => {
                        warn!(
                            target = %target,
                            idle_secs = idle_timeout.as_secs(),
                            "sample-based telemetry stream went idle; forcing reconnect"
                        );
                        emitter.emit("failed", "sample_stream_idle_timeout").await;
                        break;
                    }
                    message = stream.message() => message,
                }
            } else {
                tokio::select! {
                    _ = shutdown.changed() => {
                        info!(target = %target, "shutdown signal received during telemetry stream");
                        emitter.emit_shutdown("disconnected", "shutdown").await;
                        return Ok(());
                    }
                    _ = &mut sync_sleep, if pending_sync > 0 => {
                        warn!(target = %target, timeout_secs = INITIAL_SYNC_TIMEOUT.as_secs(),
                            "initial synchronization deadline expired");
                        emitter.emit("failed", "initial_sync_timeout").await;
                        break;
                    }
                    message = stream.message() => message,
                }
            };

            match next_message {
                Ok(Some(response)) => {
                    use crate::proto::gnmi::subscribe_response::Response;
                    if let Some(resp) = response.response {
                        match resp {
                            Response::Update(notif) => {
                                use std::collections::HashMap;
                                let prefix = notif
                                    .prefix
                                    .as_ref()
                                    .map(path_to_string)
                                    .unwrap_or_default();

                                // Devices like cEOS stream individual scalar leaves rather than a
                                // JSON blob at the container path. Group scalars within one
                                // notification by their parent path so classifiers see the same
                                // blob-at-container shape regardless of vendor.
                                let mut blobs: Vec<(String, serde_json::Value, Vec<Path>)> =
                                    Vec::new();
                                let mut leaf_groups: HashMap<
                                    String,
                                    (serde_json::Map<String, serde_json::Value>, Vec<Path>),
                                > = HashMap::new();

                                for update in &notif.update {
                                    let update_path = update
                                        .path
                                        .as_ref()
                                        .map(path_to_string)
                                        .unwrap_or_default();
                                    let path = match (prefix.is_empty(), update_path.is_empty()) {
                                        (true, _) => update_path,
                                        (false, true) => prefix.clone(),
                                        (false, false) => format!("{prefix}/{update_path}"),
                                    };
                                    // Structured evidence path: notification
                                    // prefix elements followed by the update's
                                    // own elements (gNMI relative-path rule).
                                    let gnmi_paths = apply_session_origins(
                                        resolve_notification_path(
                                            notif.prefix.as_ref(),
                                            update.path.as_ref(),
                                        )
                                        .into_iter()
                                        .collect(),
                                        &subscriptions,
                                    );
                                    let val = update
                                        .val
                                        .as_ref()
                                        .map(typed_value_to_json)
                                        .unwrap_or(serde_json::Value::Null);
                                    let is_scalar = matches!(
                                        val,
                                        serde_json::Value::Number(_)
                                            | serde_json::Value::String(_)
                                            | serde_json::Value::Bool(_)
                                    );
                                    if is_scalar && let Some(slash) = path.rfind('/') {
                                        let (group, group_paths) = leaf_groups
                                            .entry(path[..slash].to_string())
                                            .or_default();
                                        group.insert(path[slash + 1..].to_string(), val);
                                        group_paths.extend(gnmi_paths);
                                        continue;
                                    }
                                    blobs.push((path, val, gnmi_paths));
                                }

                                let all_updates = blobs.into_iter().chain(
                                    leaf_groups.into_iter().map(|(p, (obj, paths))| {
                                        (p, serde_json::Value::Object(obj), paths)
                                    }),
                                );
                                for (path, val, gnmi_paths) in all_updates {
                                    debug!(target = %target, path = %path, "update");
                                    let msg = TelemetryUpdate {
                                        plan_id: submitted_plan_id.clone(),
                                        session_id: session_id.clone(),
                                        source_protocol: "gnmi".to_string(),
                                        gnmi_path: gnmi_paths,
                                        is_delete: false,
                                        collector_id: String::new(),
                                        target: target.clone(),
                                        vendor: caps.vendor_label.clone(),
                                        hostname: self.hostname.clone().unwrap_or_default(),
                                        role: self.role.clone().unwrap_or_default(),
                                        site: self.site.clone().unwrap_or_default(),
                                        timestamp_ns: notif.timestamp,
                                        path,
                                        value: val,
                                    };
                                    if let Some(ref d) = self.debouncer
                                        && d.should_drop(&msg)
                                    {
                                        continue;
                                    }
                                    self.bus.publish(msg);
                                }

                                // gNMI Notification.delete: tombstone
                                // evidence for paths removed from the
                                // device's data tree (M5.2B B3). Delete
                                // paths resolve against the notification
                                // prefix exactly like update paths. Each
                                // delete becomes its own provenance-
                                // carrying update so the verifier can
                                // invalidate coverage for the CURRENT
                                // session only.
                                for delete in &notif.delete {
                                    let delete_path = path_to_string(delete);
                                    let path = match (prefix.is_empty(), delete_path.is_empty()) {
                                        (true, _) => delete_path,
                                        (false, true) => prefix.clone(),
                                        (false, false) => format!("{prefix}/{delete_path}"),
                                    };
                                    if path.is_empty() {
                                        continue;
                                    }
                                    let gnmi_paths = apply_session_origins(
                                        resolve_notification_path(
                                            notif.prefix.as_ref(),
                                            Some(delete),
                                        )
                                        .into_iter()
                                        .collect(),
                                        &subscriptions,
                                    );
                                    let msg = TelemetryUpdate {
                                        plan_id: submitted_plan_id.clone(),
                                        session_id: session_id.clone(),
                                        source_protocol: "gnmi".to_string(),
                                        gnmi_path: gnmi_paths,
                                        is_delete: true,
                                        collector_id: String::new(),
                                        target: target.clone(),
                                        vendor: caps.vendor_label.clone(),
                                        hostname: self.hostname.clone().unwrap_or_default(),
                                        role: self.role.clone().unwrap_or_default(),
                                        site: self.site.clone().unwrap_or_default(),
                                        timestamp_ns: notif.timestamp,
                                        path,
                                        value: serde_json::Value::Null,
                                    };
                                    if let Some(ref d) = self.debouncer
                                        && d.should_drop(&msg)
                                    {
                                        continue;
                                    }
                                    self.bus.publish(msg);
                                }
                            }
                            Response::SyncResponse(sync_response) => {
                                debug!(target = %target, sync_response, "sync response");
                                // sync_response=true completes the initial
                                // synchronization sequence for that
                                // subscription; false means more updates
                                // follow — it is NOT completion and NOT an
                                // error. Per-leaf coverage is proven by
                                // observations, never by sync alone.
                                if sync_response && pending_sync > 0 {
                                    pending_sync -= 1;
                                    if pending_sync == 0 {
                                        emitter.emit("synchronized", "").await;
                                    }
                                }
                            }
                            Response::Error(e) => {
                                warn!(target = %target, code = e.code, message = %e.message, "gNMI error");
                                emitter
                                    .emit("failed", format!("gnmi_error:{}:{}", e.code, e.message))
                                    .await;
                                break;
                            }
                        }
                    }
                }
                Ok(None) => {
                    warn!(target = %target, "stream ended");
                    emitter.emit("disconnected", "stream_ended").await;
                    break;
                }
                Err(e) => {
                    warn!(target = %target, error = %e, "stream error");
                    emitter.emit("failed", format!("{e:#}")).await;
                    break;
                }
            }
        }

        Ok(())
    }

    async fn connect(&self) -> Result<Channel> {
        let endpoint = format!("{}://{}", self.conn_security.scheme(), self.target);

        let mut builder = Channel::from_shared(endpoint.clone())
            .context("invalid endpoint")?
            .timeout(Duration::from_secs(30));

        if let Some(tls) = self.conn_security.client_tls_config()? {
            builder = builder.tls_config(tls).context("TLS config failed")?;
        }

        let channel = builder
            .connect()
            .await
            .with_context(|| format!("failed to connect to {endpoint}"))?;

        if !self.conn_security.effective.is_tls() {
            metrics::counter!(
                "bonsai_gnmi_plaintext_dial_total",
                "mode" => self.conn_security.effective.label()
            )
            .increment(1);
        }

        info!(
            target = %self.target,
            security = self.conn_security.effective.label(),
            "connected"
        );
        Ok(channel)
    }
}

/// A running subscriber plus the effective-assignment fingerprint it was
/// spawned for. The fingerprint is process-local (keyed by a random
/// per-process salt) and is NEVER logged, persisted, sent over the wire,
/// or placed in plan IDs — it exists only so assignment reconciliation
/// can detect whether the effective assignment changed.
pub struct SubscriberEntry {
    pub shutdown_tx: watch::Sender<bool>,
    pub handle: JoinHandle<()>,
    pub assignment_fingerprint: String,
    /// Shared execution counter for this target. The authoritative
    /// copy lives in `SubscriberHandleMap::exec_seqs` and OUTLIVES the
    /// entry: stopping/removing a subscriber must never reset it, or a
    /// recreated subscriber's executions would rank below the core's
    /// per-target high-water mark and be fenced as stale forever.
    pub execution_seq: Arc<AtomicU64>,
}

/// Running subscribers plus the per-target execution-counter book.
///
/// `exec_seqs` is owned by the same scope as the subscriber map (the
/// local subscriber manager task, or the collector manager for the
/// life of the collector process) and is deliberately NOT pruned on
/// subscriber stop/remove/assignment withdrawal: execution generations
/// must stay monotonic for the whole authority context, and a counter
/// is O(1) per distinct target address — the same order as the
/// assignment/registry state that produced it.
pub struct SubscriberHandleMap {
    entries: HashMap<String, SubscriberEntry>,
    exec_seqs: HashMap<String, Arc<AtomicU64>>,
}

impl SubscriberHandleMap {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            exec_seqs: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn keys(&self) -> std::collections::hash_map::Keys<'_, String, SubscriberEntry> {
        self.entries.keys()
    }

    pub fn get(&self, address: &str) -> Option<&SubscriberEntry> {
        self.entries.get(address)
    }

    pub fn contains_key(&self, address: &str) -> bool {
        self.entries.contains_key(address)
    }

    /// The monotonic execution counter for `address`, created on first
    /// use and retained across every subscriber stop/recreation.
    pub fn exec_seq(&mut self, address: &str) -> Arc<AtomicU64> {
        Arc::clone(
            self.exec_seqs
                .entry(address.to_string())
                .or_insert_with(|| Arc::new(AtomicU64::new(0))),
        )
    }

    pub fn insert(&mut self, address: String, entry: SubscriberEntry) {
        // The counter book is the source of truth: a stale entry can
        // never fork the sequence — the earliest counter wins.
        self.exec_seqs
            .entry(address.clone())
            .or_insert_with(|| Arc::clone(&entry.execution_seq));
        self.entries.insert(address, entry);
    }

    /// Stop bookkeeping only — the execution counter is retained.
    pub fn remove(&mut self, address: &str) -> Option<SubscriberEntry> {
        self.entries.remove(address)
    }
}

impl Default for SubscriberHandleMap {
    fn default() -> Self {
        Self::new()
    }
}

pub async fn stop_subscriber(address: &str, subscribers: &mut SubscriberHandleMap) {
    if let Some(entry) = subscribers.remove(address) {
        let _ = entry.shutdown_tx.send(true);
        let _ = entry.handle.await;
        info!(address = %address, "subscriber stopped");
    }
}

pub async fn stop_all_subscribers(subscribers: &mut SubscriberHandleMap) {
    let addresses: Vec<String> = subscribers.keys().cloned().collect();
    for address in addresses {
        stop_subscriber(&address, subscribers).await;
    }
}

/// Schema-bound execution guard: a present binding must be the exact
/// approved mode with every authority field populated, bound to this
/// collector, and carried by an explicit acquisition plan. Anything else
/// is an invalid binding — the subscriber must idle/reject rather than
/// fall back to capability-derived or broadened collection.
pub fn schema_binding_block_reason(target: &TargetConfig) -> Option<&'static str> {
    schema_binding_block_reason_parts(
        target.schema_binding.as_ref(),
        target.acquisition,
        target.collector_id.as_deref(),
    )
}

/// Field-level variant for subscribers that carry the binding but not the
/// full target row. `collector_id = None` still requires a populated
/// binding collector identity; exact collector equality is enforced at
/// the authenticated Core lifecycle boundary.
pub fn schema_binding_block_reason_parts(
    binding: Option<&crate::config::SchemaBindingConfig>,
    acquisition: AcquisitionMode,
    collector_id: Option<&str>,
) -> Option<&'static str> {
    let binding = binding?;
    if binding.mode != "schema_bound" {
        return Some("invalid_schema_binding");
    }
    if binding.schema_id.trim().is_empty()
        || binding.evidence_fingerprint.trim().is_empty()
        || binding.plan_id.trim().is_empty()
        || binding.binding_fingerprint.trim().is_empty()
        || binding.approved_by.trim().is_empty()
        || binding.approval_revision == 0
        || binding.snapshot_id <= 0
        || binding.approved_at_ns <= 0
        || binding.collector_id.as_deref().unwrap_or("").is_empty()
    {
        return Some("incomplete_schema_binding");
    }
    if let Some(expected) = collector_id
        && binding.collector_id.as_deref() != Some(expected)
    {
        return Some("schema_binding_collector_mismatch");
    }
    if acquisition != AcquisitionMode::Explicit {
        return Some("schema_binding_requires_explicit");
    }
    None
}

/// Per-process random key for assignment fingerprints. Generated once;
/// fingerprints from different processes are incomparable by design.
fn assignment_fingerprint_key() -> &'static [u8; 32] {
    static KEY: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    KEY.get_or_init(rand::random)
}

/// Effective-assignment fingerprint (M5.2A): detects every change that
/// must replace the running subscriber — acquisition intent, resolved
/// path selection (order-independent), transport security mode/domain,
/// certificate MATERIAL (content hash, so rotation under an unchanged
/// reference is detected), credentials (content), and the vendor hint
/// that steers capability fallback. Hostname/role/site are telemetry
/// metadata rather than connection configuration and are excluded.
/// Secret-derived material is hashed with a process-local salt and the
/// result is never exposed outside this map.
pub fn assignment_fingerprint(
    target: &TargetConfig,
    conn_security: &crate::gnmi_security::ConnSecurity,
    username: Option<&str>,
    password: Option<&str>,
    authority_epoch: u64,
    collector_session_generation: u64,
) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"bonsai-assignment-fp-v2");
    h.update(assignment_fingerprint_key());
    // Authority identity is part of the effective assignment: a new
    // core epoch or collector session generation must replace the
    // subscriber so its lifecycle handle is re-stamped — a preserved
    // subscriber would keep emitting under retired authority (M5.2B).
    h.update(authority_epoch.to_le_bytes());
    h.update(collector_session_generation.to_le_bytes());
    h.update(target.address.as_bytes());
    h.update([u8::from(target.enabled), u8::from(target.inventory_only)]);
    h.update(target.acquisition.as_str().as_bytes());
    h.update(target.vendor.as_deref().unwrap_or("").as_bytes());
    if let Some(binding) = &target.schema_binding {
        for part in [
            binding.mode.as_str(),
            binding.schema_id.as_str(),
            binding.content_id.as_deref().unwrap_or(""),
            binding.datastore.as_deref().unwrap_or(""),
            binding.evidence_fingerprint.as_str(),
            binding.plan_id.as_str(),
            binding.binding_fingerprint.as_str(),
            binding.collector_id.as_deref().unwrap_or(""),
            binding.approved_by.as_str(),
        ] {
            h.update(part.as_bytes());
            h.update([0x1f]);
        }
        h.update(binding.snapshot_id.to_le_bytes());
        h.update(binding.approval_revision.to_le_bytes());
        h.update(binding.approved_at_ns.to_le_bytes());
    }
    h.update([0x1e]);
    let mut paths: Vec<String> = target
        .selected_paths
        .iter()
        .map(|p| {
            format!(
                "{}|{}|{}|{}|{}",
                p.origin, p.path, p.mode, p.sample_interval_ns, p.optional
            )
        })
        .collect();
    paths.sort();
    for p in paths {
        h.update(p.as_bytes());
        h.update([0xff]);
    }
    h.update(conn_security.effective.label().as_bytes());
    h.update(conn_security.tls_domain.as_bytes());
    if let Some(pem) = &conn_security.ca_cert_pem {
        h.update(pem);
    }
    if let Some(pem) = &conn_security.client_cert_pem {
        h.update(pem);
    }
    if let Some(pem) = &conn_security.client_key_pem {
        h.update(pem);
    }
    if let Some(u) = username {
        h.update(u.as_bytes());
    }
    h.update([0x00]);
    if let Some(p) = password {
        h.update(p.as_bytes());
    }
    hex::encode(h.finalize())
}

/// Outcome of an assignment reconciliation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnOutcome {
    /// A new subscriber task was spawned; any obsolete subscriber was
    /// stopped and awaited first.
    Started,
    /// The effective assignment is identical to the running subscriber's
    /// — it was preserved untouched (idempotent duplicate delivery).
    Unchanged,
    /// The assignment requests no acquisition (disabled target or
    /// inventory-only asset); any previous subscriber was stopped.
    Stopped,
}

/// Resolve transport security for a target (filesystem-only, used by
/// collector mode where no vault is present). Acquisition sessions
/// require an authorized transport — legacy-inferred plaintext fails
/// closed with `migration_required`.
pub async fn conn_security_for_target_fs(
    target: &TargetConfig,
) -> Result<crate::gnmi_security::ConnSecurity> {
    crate::gnmi_security::resolve_conn_security_for_acquisition(target, None).await
}

/// Spawn or reconcile the subscriber for `target` against the running
/// set (M5.2A assignment reconciliation):
///
/// - Unchanged effective assignment → existing healthy subscriber is
///   preserved (`Unchanged`).
/// - Changed effective assignment → the obsolete subscriber is stopped
///   AND awaited before the replacement is spawned (`Started`).
/// - Disabled / inventory-only → any previous stream is stopped and no
///   replacement is started (`Stopped`).
/// - Invalid/unauthorized replacement configuration → `Err`; the caller
///   MUST stop any existing subscriber (fail closed — never silently
///   preserve an obsolete insecure or broader subscription).
pub async fn spawn_subscriber_with_creds(
    target: TargetConfig,
    bus: &std::sync::Arc<InProcessBus>,
    debouncer: Option<Arc<TelemetryDebouncer>>,
    subscription_plan_tx: Option<&mpsc::Sender<SubscriptionPlan>>,
    subscribers: &mut SubscriberHandleMap,
    lifecycle: Option<&LifecycleHandle>,
) -> Result<SpawnOutcome> {
    let address = target.address.clone();
    if !target.enabled {
        info!(address = %address, "subscriber start skipped because target is disabled");
        stop_subscriber(&address, subscribers).await;
        if let Some(h) = lifecycle {
            SessionEmitter::new(
                Some(h),
                &address,
                &acquisition_plan_id(target.acquisition, &[]),
                "",
                None,
            )
            .with_schema_binding(target.schema_binding.clone())
            .emit_plan("disabled", "target disabled")
            .await;
        }
        return Ok(SpawnOutcome::Stopped);
    }
    if target.inventory_only {
        info!(
            address = %address,
            "subscriber start skipped: inventory-only asset has no collection plan"
        );
        stop_subscriber(&address, subscribers).await;
        if let Some(h) = lifecycle {
            SessionEmitter::new(
                Some(h),
                &address,
                &acquisition_plan_id(target.acquisition, &[]),
                "",
                None,
            )
            .with_schema_binding(target.schema_binding.clone())
            .emit_plan("disabled", "inventory-only asset has no collection plan")
            .await;
        }
        return Ok(SpawnOutcome::Stopped);
    }

    if let Some(reason) = schema_binding_block_reason(&target) {
        info!(address = %address, reason, "schema-bound assignment blocked — no subscriber");
        stop_subscriber(&address, subscribers).await;
        if let Some(tx) = subscription_plan_tx {
            let plan = blocked_subscription_plan(&target, "rejected", reason.to_string());
            if tx.try_send(plan).is_err() {
                warn!(address = %address, "subscription plan channel full — status dropped");
            }
        }
        if let Some(h) = lifecycle {
            SessionEmitter::new(
                Some(h),
                &address,
                &acquisition_plan_id(target.acquisition, &[]),
                "",
                None,
            )
            .with_schema_binding(target.schema_binding.clone())
            .emit_plan("failed", reason)
            .await;
        }
        return Ok(SpawnOutcome::Stopped);
    }

    // Resolve security + credentials BEFORE deciding: an unresolvable
    // replacement must not silently keep the obsolete subscriber.
    let conn_security = conn_security_for_target_fs(&target).await?;
    crate::gnmi_security::validate_auth_metadata(
        target.username.as_deref(),
        target.password.as_deref(),
    )
    .context("assignment credential metadata invalid")?;

    let (authority_epoch, collector_session_generation) = lifecycle
        .map(|h| (h.authority_epoch, h.collector_session_generation))
        .unwrap_or((0, 0));
    let fingerprint = assignment_fingerprint(
        &target,
        &conn_security,
        target.username.as_deref(),
        target.password.as_deref(),
        authority_epoch,
        collector_session_generation,
    );

    if let Some(entry) = subscribers.get(&address)
        && entry.assignment_fingerprint == fingerprint
    {
        info!(address = %address, "assignment unchanged — preserving subscriber");
        return Ok(SpawnOutcome::Unchanged);
    }
    // Fetch the execution counter BEFORE stopping the obsolete
    // subscriber: it lives in the map's counter book and survives the
    // stop, so every recreation — disable/enable, remove/re-add,
    // plan replacement, authority renewal — keeps monotonic execution
    // generations instead of restarting below the core's high-water
    // mark (M5.2B review B1).
    let execution_seq = subscribers.exec_seq(&address);
    if subscribers.contains_key(&address) {
        info!(address = %address, "assignment changed — stopping obsolete subscriber");
        stop_subscriber(&address, subscribers).await;
    }

    // An effective plan that acquires nothing (explicit empty selection,
    // disabled intent, or rejected path set) must not leave a resident
    // task: no device contact, no Subscribe RPC — just the truthful plan
    // status. The obsolete stream was already stopped above.
    let plan = planned_subscription_plan_for_target(&target);
    if plan.state == "disabled" || plan.state == "rejected" {
        let state = plan.state.clone();
        if let Some(tx) = subscription_plan_tx {
            if tx.try_send(plan.clone()).is_err() {
                warn!(address = %address, "subscription plan channel full — status dropped");
            }
        }
        if let Some(h) = lifecycle {
            let lc_state = if state == "disabled" {
                "disabled"
            } else {
                "failed"
            };
            SessionEmitter::new(Some(h), &address, &plan.plan_id, "", None)
                .with_schema_binding(target.schema_binding.clone())
                .emit_plan(lc_state, &plan.detail)
                .await;
        }
        info!(address = %address, state = %state, "no acquisition plan — no subscriber");
        return Ok(SpawnOutcome::Stopped);
    }

    let subscriber = GnmiSubscriber::new(
        target.address.clone(),
        target.username.clone(),
        target.password.clone(),
        target.vendor.clone(),
        target.hostname.clone(),
        target.role.clone(),
        target.site.clone(),
        conn_security,
        std::sync::Arc::clone(bus),
        debouncer,
        subscription_plan_tx.cloned(),
        target.selected_paths.clone(),
        target.acquisition,
        lifecycle.cloned(),
        Arc::clone(&execution_seq),
    )
    .with_schema_binding(target.schema_binding.clone());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(async move { subscriber.run_forever(shutdown_rx).await });
    subscribers.insert(
        address.clone(),
        SubscriberEntry {
            shutdown_tx,
            handle,
            assignment_fingerprint: fingerprint,
            execution_seq,
        },
    );
    info!(address = %address, "subscriber started");
    Ok(SpawnOutcome::Started)
}

pub fn planned_subscription_plan_for_target(
    target: &crate::config::TargetConfig,
) -> SubscriptionPlan {
    if let Some(reason) = schema_binding_block_reason(target) {
        let plan_id = target
            .schema_binding
            .as_ref()
            .map(|b| b.plan_id.clone())
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| acquisition_plan_id(target.acquisition, &[]));
        return SubscriptionPlan {
            target: target.address.clone(),
            paths: Vec::new(),
            plan_id,
            session_id: String::new(),
            state: "rejected".to_string(),
            acquisition: target.acquisition.as_str().to_string(),
            detail: reason.to_string(),
            schema_id: target
                .schema_binding
                .as_ref()
                .filter(|b| b.mode == "schema_bound")
                .map(|b| b.schema_id.clone()),
        };
    }
    // Preview uses capability fallback hints for automatic mode; explicit
    // plans are validated strictly (no capability substitution).
    let schema_id = target
        .schema_binding
        .as_ref()
        .filter(|b| b.mode == "schema_bound")
        .map(|b| b.schema_id.clone());
    let caps = ModelCapabilities::fallback(
        target
            .vendor
            .as_deref()
            .or(target.hostname.as_deref())
            .or(target.role.as_deref()),
    );
    match build_acquisition_plan(target.acquisition, &target.selected_paths, &caps) {
        AcquisitionPlan::Disabled { plan_id } => SubscriptionPlan {
            target: target.address.clone(),
            paths: Vec::new(),
            plan_id,
            session_id: String::new(),
            state: "disabled".to_string(),
            acquisition: target.acquisition.as_str().to_string(),
            detail: "acquisition disabled".to_string(),
            schema_id,
        },
        AcquisitionPlan::Rejected { plan_id, errors } => SubscriptionPlan {
            target: target.address.clone(),
            paths: Vec::new(),
            plan_id,
            session_id: String::new(),
            state: "rejected".to_string(),
            acquisition: target.acquisition.as_str().to_string(),
            detail: errors.join("; "),
            schema_id,
        },
        AcquisitionPlan::Ready {
            subscriptions,
            plan_id,
            ..
        } => SubscriptionPlan {
            target: target.address.clone(),
            paths: subscriptions
                .iter()
                .filter_map(subscription_expectation)
                .collect(),
            plan_id,
            session_id: String::new(),
            state: "submitted".to_string(),
            acquisition: target.acquisition.as_str().to_string(),
            detail: String::new(),
            schema_id,
        },
    }
}

/// Build a non-submitted SubscriptionPlan for a target whose assignment
/// could not be honoured (rejected / blocked / failed). plan_id remains
/// the STABLE desired/effective plan identity — failure state and detail
/// are carried in the plan's state/detail fields and the lifecycle
/// stream, never encoded into plan_id (M5.2B).
pub fn blocked_subscription_plan(
    target: &TargetConfig,
    state: &str,
    detail: String,
) -> SubscriptionPlan {
    let mut plan = planned_subscription_plan_for_target(target);
    plan.state = state.to_string();
    plan.detail = detail;
    plan.paths = Vec::new();
    plan
}

// ── capabilities ──────────────────────────────────────────────────────────────

/// Subscription capabilities derived entirely from the device's Capabilities response.
/// Path selection is model-driven: OC models are preferred; vendor-native paths are
/// used only when OC model names are not advertised.
/// `vendor_label` is for logging and Device node tagging only — never for path routing.
struct ModelCapabilities {
    vendor_label: String,
    encoding: i32,
    /// OC interfaces model advertised → subscribe to OC interface paths.
    has_oc_interfaces: bool,
    /// OC BFD model advertised → subscribe to OC BFD paths.
    has_oc_bfd: bool,
    /// OC BGP / network-instance model advertised → subscribe to OC BGP paths.
    has_oc_bgp: bool,
    /// OC LLDP model advertised → subscribe to OC LLDP paths.
    has_oc_lldp: bool,
    /// SRL native model tree present. SRL does not advertise OC model names; this
    /// flag triggers SRL-native paths as the fallback for each concern.
    has_srl_native: bool,
    /// SRL native BFD model advertised (srl_nokia-bfd) — subscribe to SRL BFD paths.
    has_srl_native_bfd: bool,
    /// Cisco XR native stats model present. Kept as a diagnostic flag; OC is used
    /// when both OC and XR-native are available.
    has_xr_native: bool,
}

impl ModelCapabilities {
    /// Build from a real Capabilities response — single source of truth for path selection.
    fn from_response(models: &[crate::proto::gnmi::ModelData], encodings: &[i32]) -> Self {
        let json_ietf = crate::proto::gnmi::Encoding::JsonIetf as i32;
        let json = crate::proto::gnmi::Encoding::Json as i32;

        let has_oc_iface = models.iter().any(|m| m.name == "openconfig-interfaces");
        let has_oc_bfd = models.iter().any(|m| m.name == "openconfig-bfd");
        let has_oc_bgp = models
            .iter()
            .any(|m| m.name == "openconfig-bgp" || m.name == "openconfig-network-instance");
        let has_oc_lldp = models.iter().any(|m| m.name == "openconfig-lldp");
        // SRL advertises models as Nokia URNs (urn:nokia.com:srlinux:*:srl_nokia-*),
        // not as openconfig-* names — detect by the srl_nokia substring.
        let has_srl = models.iter().any(|m| m.name.contains("srl_nokia"));
        let has_srl_bfd = models.iter().any(|m| m.name.contains("srl_nokia-bfd"));
        let has_xr_native = models
            .iter()
            .any(|m| m.name.contains("Cisco-IOS-XR-infra-statsd-oper"));

        let encoding = if encodings.contains(&json_ietf) {
            json_ietf
        } else {
            json
        };

        // Vendor label: informational only — derived from model names, never used for routing.
        let vendor_label = if has_srl {
            "nokia_srl"
        } else if models.iter().any(|m| m.name.starts_with("Cisco-IOS-XR")) {
            "cisco_xrd"
        } else if models
            .iter()
            .any(|m| m.name.to_lowercase().contains("arista") || m.name.contains("EOS"))
        {
            "arista_ceos"
        } else if models
            .iter()
            .any(|m| m.name.to_lowercase().starts_with("junos"))
        {
            "juniper_crpd"
        } else {
            "openconfig"
        };

        Self {
            vendor_label: vendor_label.to_string(),
            encoding,
            has_oc_interfaces: has_oc_iface,
            has_oc_bfd,
            has_oc_bgp,
            has_oc_lldp,
            has_srl_native: has_srl,
            has_srl_native_bfd: has_srl_bfd,
            has_xr_native,
        }
    }

    /// Fallback when Capabilities RPC fails.
    /// vendor_hint sets the label and distinguishes the two known cases:
    ///   SRL  → native (SRL uses Nokia URN model names, not OC names)
    ///   XRd  → XR native for interfaces, OC for BGP/LLDP
    ///   else → assume OC (safe for any standard-compliant device)
    fn fallback(vendor_hint: Option<&str>) -> Self {
        let json_ietf = crate::proto::gnmi::Encoding::JsonIetf as i32;
        let label = vendor_hint.unwrap_or("unknown").to_string();
        match vendor_hint {
            Some(h) if h.contains("srl") || h.contains("nokia") => Self {
                vendor_label: label,
                encoding: json_ietf,
                has_srl_native: true,
                has_srl_native_bfd: true,
                has_xr_native: false,
                has_oc_interfaces: false,
                has_oc_bfd: false,
                has_oc_bgp: false,
                has_oc_lldp: false,
            },
            Some(h) if h.contains("xrd") || h.contains("cisco") => Self {
                vendor_label: label,
                encoding: json_ietf,
                has_srl_native: false,
                has_srl_native_bfd: false,
                has_xr_native: true,
                has_oc_interfaces: false,
                has_oc_bfd: true,
                has_oc_bgp: true,
                has_oc_lldp: true,
            },
            _ => Self {
                vendor_label: label,
                encoding: json_ietf,
                has_srl_native: false,
                has_srl_native_bfd: false,
                has_xr_native: false,
                has_oc_interfaces: true,
                has_oc_bfd: false,
                has_oc_bgp: true,
                has_oc_lldp: false,
            },
        }
    }
}

async fn detect_capabilities(
    client: &mut GNmiClient<tonic::transport::Channel>,
    target: &str,
    username: Option<&str>,
    password: Option<&str>,
    hint: Option<&str>,
) -> ModelCapabilities {
    let mut req = tonic::Request::new(CapabilityRequest::default());
    if let Some(u) = username
        && let Ok(v) = MetadataValue::try_from(u)
    {
        req.metadata_mut().insert("username", v);
    }
    if let Some(p) = password
        && let Ok(v) = MetadataValue::try_from(p)
    {
        req.metadata_mut().insert("password", v);
    }

    match client.capabilities(req).await {
        Ok(resp) => {
            let inner = resp.into_inner();
            let models = &inner.supported_models;
            let encodings = &inner.supported_encodings;

            let sample: Vec<_> = models.iter().take(5).map(|m| m.name.as_str()).collect();
            debug!(target, ?sample, ?encodings, "Capabilities probe");

            let mut caps = ModelCapabilities::from_response(models, encodings);
            // hint overrides label only when Capabilities succeeds
            if let Some(h) = hint {
                caps.vendor_label = h.to_string();
            }
            info!(
                target,
                vendor    = %caps.vendor_label,
                encoding  = caps.encoding,
                srl       = caps.has_srl_native,
                srl_bfd   = caps.has_srl_native_bfd,
                oc_iface  = caps.has_oc_interfaces,
                oc_bfd    = caps.has_oc_bfd,
                oc_bgp    = caps.has_oc_bgp,
                oc_lldp   = caps.has_oc_lldp,
                xr_native = caps.has_xr_native,
                "capabilities detected"
            );
            caps
        }
        Err(e) => {
            warn!(target, error = %e, "Capabilities RPC failed — using hint-aware defaults");
            ModelCapabilities::fallback(hint)
        }
    }
}

// ── subscription builder ──────────────────────────────────────────────────────

/// Build subscriptions from capability flags.
///
/// Native paths are preferred — they are richer and directly reflect what the
/// device exposes. OC paths are the fallback when no native model is detected.
/// The same rule applies to every vendor; no vendor-specific branching here.
fn build_subscriptions(caps: &ModelCapabilities) -> Vec<Subscription> {
    let mut subs = Vec::new();

    // ── Interfaces ────────────────────────────────────────────────────────────
    if caps.has_srl_native {
        subs.push(sub_sample(
            srl_path(&[("interface", &[("name", "*")])]).with_tail("statistics"),
            10_000_000_000,
        ));
    } else if caps.has_xr_native {
        subs.push(sub_sample(xr_native_stats_path(), 10_000_000_000));
    } else if caps.has_oc_interfaces {
        subs.push(sub_sample(oc_path(&["interfaces"]), 10_000_000_000));
    }

    // ── BFD ───────────────────────────────────────────────────────────────────
    // SRL peer sessions live under bfd/network-instance[name=X]/peer[local-discriminator=Y],
    // not under bfd/subinterface — subscribe to the network-instance container for state.
    if caps.has_srl_native && caps.has_srl_native_bfd {
        subs.push(sub_on_change(srl_path(&[
            ("bfd", &[]),
            ("network-instance", &[("name", "default")]),
        ])));
    } else if caps.has_oc_bfd {
        subs.push(sub_on_change(oc_path(&["bfd"])));
    }

    // ── BGP ───────────────────────────────────────────────────────────────────
    if caps.has_srl_native {
        subs.push(sub_on_change(srl_bgp_neighbors_path()));
    } else if caps.has_oc_bgp {
        subs.push(sub_on_change(oc_path(&["network-instances"])));
    }

    // ── Interface oper-status ─────────────────────────────────────────────────
    if caps.has_srl_native {
        // ON_CHANGE: fires immediately when an interface goes up or down.
        subs.push(sub_on_change(
            srl_path(&[("interface", &[("name", "*")])]).with_tail("oper-state"),
        ));
    } else if caps.has_oc_interfaces {
        // cEOS: OC path carries oper-status; subscribe to state container.
        subs.push(sub_on_change(oc_path(&["interfaces"])));
    }

    // ── IS-IS adjacency ───────────────────────────────────────────────────────
    if caps.has_srl_native {
        subs.push(sub_on_change(srl_isis_adjacency_path()));
    }

    // ── LLDP ──────────────────────────────────────────────────────────────────
    if caps.has_srl_native {
        subs.push(sub_on_change(srl_lldp_neighbors_path()));
    } else if caps.has_xr_native {
        // SAMPLE because ON_CHANGE misses neighbors already discovered before subscription starts.
        subs.push(sub_sample(xr_native_lldp_path(), 60_000_000_000));
    } else if caps.has_oc_lldp {
        subs.push(sub_on_change(oc_path(&["lldp"])));
    }

    if subs.is_empty() {
        warn!(vendor = %caps.vendor_label, "no subscribable paths derived from capabilities");
    }

    subs
}

// ── acquisition plan ──────────────────────────────────────────────────────────

/// Effective acquisition plan for one subscribe cycle.
enum AcquisitionPlan {
    /// No Subscribe RPC is emitted (acquisition disabled or an explicit
    /// selection that resolved to zero paths after overrides).
    Disabled { plan_id: String },
    /// Explicit plan failed validation — no Subscribe RPC, no fallback.
    Rejected {
        plan_id: String,
        errors: Vec<String>,
    },
    /// Valid plan ready to submit.
    Ready {
        subscriptions: Vec<Subscription>,
        plan_id: String,
    },
}

/// Deterministic plan identity: hash of acquisition mode + normalized
/// (origin, path, mode, interval) tuples in canonical order. Equivalent
/// textual path spellings that parse to the same structured path produce
/// the same identity only when their normalized form matches — dedup of
/// subscriptions themselves happens on the structured Path, not the string.
pub fn acquisition_plan_id(
    acquisition: AcquisitionMode,
    entries: &[(String, String, String, u64)],
) -> String {
    use std::hash::{Hash, Hasher};
    // Ordering-independent: the same effective plan in a different
    // configuration order must produce the same identity.
    let mut sorted = entries.to_vec();
    sorted.sort();
    let mut canonical = String::new();
    for (origin, path, mode, interval) in &sorted {
        canonical.push_str(&format!("{}|{}|{}|{}\n", origin, path, mode, interval));
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    acquisition.as_str().hash(&mut hasher);
    canonical.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Authorized explicit-manifest entries for `target`: the normalized
/// `(origin, path, mode, sample_interval_ns)` tuples an explicit
/// acquisition may legitimately submit — exactly the set
/// [`build_acquisition_plan`] would resolve from `selected_paths`.
/// Returns `Err` when the selection itself is invalid (the plan would
/// be `Rejected`, never submitted) so callers fail closed. An empty
/// set means explicit-empty → Disabled: no live plan may be submitted.
/// Used by the authenticated lifecycle boundary to reject manifests
/// that broaden the authorized path/mode/interval set (M5.2B review D).
pub(crate) fn authorized_explicit_entries(
    target: &crate::config::TargetConfig,
) -> Result<std::collections::HashSet<(String, String, String, u64)>, String> {
    let mut authorized = std::collections::HashSet::new();
    for selected in &target.selected_paths {
        let (sub, _canonical) = subscription_from_selected_path(selected)?;
        if let Some(e) = subscription_expectation(&sub) {
            authorized.insert((e.origin, e.path, e.mode, e.sample_interval_ns));
        }
    }
    Ok(authorized)
}

/// Build the effective acquisition plan.
///
/// - `Disabled` intent → Disabled (never falls back).
/// - `Explicit` intent → atomic validation: every selected path must parse
///   and carry a supported mode; any failure rejects the WHOLE plan (no
///   partial application, no capability fallback). An explicit selection
///   that resolves to zero paths is Disabled — removing the last path can
///   never reactivate automatic collection.
/// - `Automatic` (default/legacy) → selected paths honoured when they yield
///   valid subscriptions; otherwise the historical capability-derived plan.
fn build_acquisition_plan(
    acquisition: AcquisitionMode,
    selected_paths: &[SelectedSubscriptionPath],
    caps: &ModelCapabilities,
) -> AcquisitionPlan {
    match acquisition {
        AcquisitionMode::Disabled => AcquisitionPlan::Disabled {
            plan_id: acquisition_plan_id(acquisition, &[]),
        },
        AcquisitionMode::Explicit => {
            let mut errors = Vec::new();
            let mut subscriptions = Vec::new();
            let mut entries = Vec::new();
            // Canonical structured identity → (mode, interval). Identical
            // duplicates collapse; same path with conflicting parameters
            // rejects the whole plan — never silently overwrite.
            let mut seen: std::collections::HashMap<String, (String, u64)> =
                std::collections::HashMap::new();
            for selected in selected_paths {
                match subscription_from_selected_path(selected) {
                    Ok((sub, canonical)) => {
                        if let Some(expectation) = subscription_expectation(&sub) {
                            let sig = (expectation.mode.clone(), expectation.sample_interval_ns);
                            match seen.get(&canonical) {
                                Some(prev) if *prev != sig => {
                                    errors.push(format!(
                                        "'{}': conflicting duplicate of an earlier selection",
                                        selected.path
                                    ));
                                    continue;
                                }
                                Some(_) => continue, // exact duplicate — collapse
                                None => {
                                    seen.insert(canonical, sig.clone());
                                    entries.push((
                                        expectation.origin,
                                        expectation.path,
                                        expectation.mode,
                                        expectation.sample_interval_ns,
                                    ));
                                }
                            }
                        }
                        subscriptions.push(sub);
                    }
                    Err(error) => errors.push(format!("'{}': {}", selected.path, error)),
                }
            }
            let plan_id = acquisition_plan_id(acquisition, &entries);
            if !errors.is_empty() {
                return AcquisitionPlan::Rejected { plan_id, errors };
            }
            if subscriptions.is_empty() {
                return AcquisitionPlan::Disabled { plan_id };
            }
            AcquisitionPlan::Ready {
                subscriptions,
                plan_id,
            }
        }
        AcquisitionMode::Automatic => {
            let selected_subscriptions = build_selected_subscriptions(selected_paths);
            let subscriptions = if selected_subscriptions.is_empty() {
                if !selected_paths.is_empty() {
                    warn!(
                        selected_paths = selected_paths.len(),
                        "configured subscription path selection produced no valid paths; falling back to capabilities"
                    );
                }
                build_subscriptions(caps)
            } else {
                selected_subscriptions
            };
            let entries = subscriptions
                .iter()
                .filter_map(subscription_expectation)
                .map(|e| (e.origin, e.path, e.mode, e.sample_interval_ns))
                .collect::<Vec<_>>();
            AcquisitionPlan::Ready {
                plan_id: acquisition_plan_id(acquisition, &entries),
                subscriptions,
            }
        }
    }
}

/// Legacy/automatic helper retained for capability-preview callers: builds
/// subscriptions from selected paths, skipping invalid entries (the strict
/// explicit path uses `subscription_from_selected_path` instead).
fn build_selected_subscriptions(selected_paths: &[SelectedSubscriptionPath]) -> Vec<Subscription> {
    selected_paths
        .iter()
        .filter_map(|s| subscription_from_selected_path(s).ok().map(|(sub, _)| sub))
        .collect()
}

/// Mode-aware SAMPLE idle watchdog. Pure ON_CHANGE streams have no cadence to
/// violate, so they keep the previous no-app-timeout behavior; SAMPLE streams
/// get a floor plus headroom scaled to the slowest requested interval so long
/// sampling periods cannot be declared dead between legitimate updates.
fn sample_stream_idle_timeout(subscriptions: &[Subscription]) -> Duration {
    let max_interval_ns = subscriptions
        .iter()
        .filter(|sub| sub.mode == SubscriptionMode::Sample as i32)
        .map(|sub| sub.sample_interval)
        .max();
    let Some(interval_ns) = max_interval_ns else {
        return Duration::ZERO;
    };
    if interval_ns == 0 {
        return SAMPLE_STREAM_IDLE_TIMEOUT;
    }
    let scaled_ns = (interval_ns as u128)
        .saturating_mul(3)
        .saturating_add(30_000_000_000)
        .min(u64::MAX as u128) as u64;
    Duration::from_nanos(scaled_ns).max(SAMPLE_STREAM_IDLE_TIMEOUT)
}

fn subscription_from_selected_path(
    selected: &SelectedSubscriptionPath,
) -> Result<(Subscription, String), String> {
    let path = selected_path_to_gnmi(&selected.origin, &selected.path)
        .ok_or_else(|| "invalid gNMI path syntax".to_string())?;
    let canonical = crate::gnmi_path::canonical_key(&path);
    match selected.mode.trim().to_ascii_uppercase().as_str() {
        "SAMPLE" => {
            if selected.sample_interval_ns == 0 {
                return Err("SAMPLE mode requires a positive sample_interval_ns".to_string());
            }
            Ok((sub_sample(path, selected.sample_interval_ns), canonical))
        }
        "ON_CHANGE" => Ok((sub_on_change(path), canonical)),
        other => Err(format!("unsupported subscription mode '{}'", other)),
    }
}

fn selected_path_to_gnmi(origin: &str, raw_path: &str) -> Option<Path> {
    crate::gnmi_path::parse_path(origin, raw_path).ok()
}

/// Park the subscriber until shutdown for plans that emit no Subscribe RPC
/// (disabled or rejected). Prevents the reconnect loop from spinning.
async fn idle_until_shutdown(shutdown: &mut tokio::sync::watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

fn sub_sample(path: Path, interval_ns: u64) -> Subscription {
    Subscription {
        path: Some(path),
        mode: SubscriptionMode::Sample as i32,
        sample_interval: interval_ns,
        ..Default::default()
    }
}

fn sub_on_change(path: Path) -> Subscription {
    Subscription {
        path: Some(path),
        mode: SubscriptionMode::OnChange as i32,
        ..Default::default()
    }
}

// ── path builders ─────────────────────────────────────────────────────────────

fn subscription_expectation(sub: &Subscription) -> Option<SubscriptionPathExpectation> {
    let path = sub.path.as_ref()?;
    let mode = if sub.mode == SubscriptionMode::Sample as i32 {
        "SAMPLE"
    } else if sub.mode == SubscriptionMode::OnChange as i32 {
        "ON_CHANGE"
    } else {
        "UNKNOWN"
    };
    Some(SubscriptionPathExpectation {
        path: path_to_string(path),
        origin: path.origin.clone(),
        mode: mode.to_string(),
        sample_interval_ns: sub.sample_interval,
    })
}

/// OpenConfig path: sets origin="openconfig" so devices can resolve the schema tree.
fn oc_path(elems: &[&str]) -> Path {
    Path {
        origin: "openconfig".to_string(),
        elem: elems
            .iter()
            .map(|name| PathElem {
                name: name.to_string(),
                key: Default::default(),
            })
            .collect(),
        ..Default::default()
    }
}

/// SR Linux native path — no origin, key-value pairs per element.
/// elems: &[("name", &[("key", "val"), ...])]
fn srl_path(elems: &[(&str, &[(&str, &str)])]) -> Path {
    Path {
        elem: elems
            .iter()
            .map(|(name, keys)| PathElem {
                name: name.to_string(),
                key: keys
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            })
            .collect(),
        ..Default::default()
    }
}

// Helper to extend a Path with a key-less tail element.
trait PathExt {
    fn with_tail(self, name: &str) -> Path;
}

impl PathExt for Path {
    fn with_tail(mut self, name: &str) -> Path {
        self.elem.push(PathElem {
            name: name.to_string(),
            key: Default::default(),
        });
        self
    }
}

fn srl_bgp_neighbors_path() -> Path {
    srl_path(&[
        ("network-instance", &[("name", "default")]),
        ("protocols", &[]),
        ("bgp", &[]),
        ("neighbor", &[("peer-address", "*")]),
    ])
}

fn srl_isis_adjacency_path() -> Path {
    // Subscribe to the whole IS-IS instance container — SRL sends per-interface/adjacency
    // ON_CHANGE leaves as individual path updates when adjacency state changes.
    srl_path(&[
        ("network-instance", &[("name", "default")]),
        ("protocols", &[]),
        ("isis", &[]),
        ("instance", &[("name", "main")]),
    ])
}

fn srl_lldp_neighbors_path() -> Path {
    srl_path(&[
        ("system", &[]),
        ("lldp", &[]),
        ("interface", &[("name", "*")]),
        ("neighbor", &[("id", "*")]),
    ])
}

/// Cisco IOS-XR native LLDP neighbor details.
fn xr_native_lldp_path() -> Path {
    Path {
        origin: String::new(),
        elem: vec![
            PathElem {
                name: "Cisco-IOS-XR-ethernet-lldp-oper:lldp".to_string(),
                key: Default::default(),
            },
            PathElem {
                name: "nodes".to_string(),
                key: Default::default(),
            },
            PathElem {
                name: "node".to_string(),
                key: Default::default(),
            },
            PathElem {
                name: "neighbors".to_string(),
                key: Default::default(),
            },
            PathElem {
                name: "details".to_string(),
                key: Default::default(),
            },
            PathElem {
                name: "detail".to_string(),
                key: Default::default(),
            },
        ],
        ..Default::default()
    }
}

/// Cisco IOS-XR native interface counters.
/// Empty origin; first element carries the module-qualified container name.
/// Key is `interface-name` (not `name` used by OC). No `latest` container —
/// `generic-counters` is a direct child of `interface` on XRd 24.x.
fn xr_native_stats_path() -> Path {
    use std::collections::HashMap;
    let mut key = HashMap::new();
    key.insert("interface-name".to_string(), "*".to_string());
    Path {
        origin: String::new(),
        elem: vec![
            PathElem {
                name: "Cisco-IOS-XR-infra-statsd-oper:infra-statistics".to_string(),
                key: Default::default(),
            },
            PathElem {
                name: "interfaces".to_string(),
                key: Default::default(),
            },
            PathElem {
                name: "interface".to_string(),
                key,
            },
            PathElem {
                name: "generic-counters".to_string(),
                key: Default::default(),
            },
        ],
        ..Default::default()
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Resolve the structured evidence path for one gNMI update: the
/// update path is relative to the notification prefix, so the full
/// path is the prefix elements followed by the update elements.
/// Origin prefers the prefix's origin, falling back to the update
/// path's. `None` when neither side carries a path — such observations
/// carry no structured evidence and cannot satisfy coverage.
fn resolve_notification_path(prefix: Option<&Path>, update: Option<&Path>) -> Option<Path> {
    match (prefix, update) {
        (None, None) => None,
        (Some(p), None) => Some(p.clone()),
        (None, Some(u)) => Some(u.clone()),
        (Some(p), Some(u)) => {
            let mut elem = p.elem.clone();
            elem.extend(u.elem.iter().cloned());
            Some(Path {
                origin: if p.origin.is_empty() {
                    u.origin.clone()
                } else {
                    p.origin.clone()
                },
                elem,
                ..Default::default()
            })
        }
    }
}

/// Origin provenance for non-echoing NOS notifications (M6F-D). Some
/// devices (verified: cEOS 4.37.0F) honor a requested subscription origin
/// but omit `Path.origin` on streamed notifications. The effective
/// subscription origin is trusted SESSION context — it is never read from
/// the telemetry payload. An empty observed origin is filled ONLY when
/// exactly one non-empty origin among this session's authorized
/// subscriptions structurally covers the observed path; conflicting or
/// absent coverage leaves the origin empty (fail closed). A non-empty
/// observed origin is never overwritten.
fn apply_session_origins(paths: Vec<Path>, subscriptions: &[Subscription]) -> Vec<Path> {
    paths
        .into_iter()
        .map(|mut path| {
            if path.origin.is_empty()
                && let Some(origin) = unique_session_origin(&path, subscriptions)
            {
                path.origin = origin.to_string();
            }
            path
        })
        .collect()
}

fn unique_session_origin<'a>(
    observation: &Path,
    subscriptions: &'a [Subscription],
) -> Option<&'a str> {
    let mut origins = std::collections::BTreeSet::new();
    for sub in subscriptions {
        let Some(expected) = sub.path.as_ref() else {
            continue;
        };
        if expected.origin.is_empty() {
            continue;
        }
        // Reuse the exact coverage matcher with the candidate origin
        // stamped onto a probe: element/key/wildcard semantics stay
        // identical to coverage evaluation, only origin identity is
        // being resolved here.
        let mut probe = observation.clone();
        probe.origin = expected.origin.clone();
        if crate::gnmi_path::observation_covers(expected, &probe) {
            origins.insert(expected.origin.as_str());
        }
    }
    if origins.len() == 1 {
        origins.into_iter().next()
    } else {
        None
    }
}

pub(crate) fn path_to_string(path: &Path) -> String {
    path.elem
        .iter()
        .map(|e| {
            if e.key.is_empty() {
                e.name.clone()
            } else {
                let keys: String = e.key.iter().map(|(k, v)| format!("[{k}={v}]")).collect();
                format!("{}{}", e.name, keys)
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn typed_value_to_json(val: &crate::proto::gnmi::TypedValue) -> serde_json::Value {
    use crate::proto::gnmi::typed_value::Value;
    match &val.value {
        Some(Value::JsonIetfVal(b)) => serde_json::from_slice(b).unwrap_or(
            serde_json::Value::String(String::from_utf8_lossy(b).into_owned()),
        ),
        Some(Value::JsonVal(b)) => serde_json::from_slice(b).unwrap_or(serde_json::Value::String(
            String::from_utf8_lossy(b).into_owned(),
        )),
        Some(Value::StringVal(s)) => serde_json::Value::String(s.clone()),
        Some(Value::IntVal(i)) => serde_json::json!(i),
        Some(Value::UintVal(u)) => serde_json::json!(u),
        Some(Value::BoolVal(b)) => serde_json::Value::Bool(*b),
        Some(Value::FloatVal(f)) => serde_json::json!(f),
        _ => serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_path_parser_handles_keys_and_origin() {
        let path = selected_path_to_gnmi(
            "",
            "network-instance[name=default]/protocols/bgp/neighbor[peer-address=*]",
        )
        .expect("parse selected path");

        assert_eq!(path.origin, "");
        assert_eq!(path.elem[0].name, "network-instance");
        assert_eq!(path.elem[0].key.get("name"), Some(&"default".to_string()));
        assert_eq!(path.elem[3].name, "neighbor");
        assert_eq!(path.elem[3].key.get("peer-address"), Some(&"*".to_string()));
    }

    #[test]
    fn selected_subscription_preserves_mode_and_interval() {
        let selected = SelectedSubscriptionPath {
            provenance: String::new(),
            path: "interfaces".to_string(),
            origin: "openconfig".to_string(),
            mode: "SAMPLE".to_string(),
            sample_interval_ns: 10_000_000_000,
            rationale: "operator selected".to_string(),
            optional: false,
        };

        let (sub, _) = subscription_from_selected_path(&selected).expect("subscription");
        assert_eq!(sub.mode, SubscriptionMode::Sample as i32);
        assert_eq!(sub.sample_interval, 10_000_000_000);
        assert_eq!(sub.path.as_ref().expect("path").origin, "openconfig");
    }

    fn sel(path: &str, mode: &str, interval_ns: u64) -> SelectedSubscriptionPath {
        SelectedSubscriptionPath {
            provenance: String::new(),
            path: path.to_string(),
            origin: String::new(),
            mode: mode.to_string(),
            sample_interval_ns: interval_ns,
            rationale: String::new(),
            optional: false,
        }
    }

    #[test]
    fn explicit_plan_id_is_order_independent() {
        let caps = ModelCapabilities::fallback(None);
        let a = build_acquisition_plan(
            AcquisitionMode::Explicit,
            &[
                sel("interfaces", "ON_CHANGE", 0),
                sel("system", "SAMPLE", 5_000_000_000),
            ],
            &caps,
        );
        let b = build_acquisition_plan(
            AcquisitionMode::Explicit,
            &[
                sel("system", "SAMPLE", 5_000_000_000),
                sel("interfaces", "ON_CHANGE", 0),
            ],
            &caps,
        );
        let id = |p: &AcquisitionPlan| match p {
            AcquisitionPlan::Ready { plan_id, .. } => plan_id.clone(),
            _ => panic!("expected ready plan"),
        };
        assert_eq!(id(&a), id(&b));
    }

    #[test]
    fn explicit_plan_rejects_conflicting_duplicate() {
        let caps = ModelCapabilities::fallback(None);
        let plan = build_acquisition_plan(
            AcquisitionMode::Explicit,
            &[
                sel("interfaces", "ON_CHANGE", 0),
                sel("interfaces", "SAMPLE", 5_000_000_000),
            ],
            &caps,
        );
        assert!(matches!(plan, AcquisitionPlan::Rejected { .. }));
    }

    #[test]
    fn explicit_plan_collapses_identical_duplicate() {
        let caps = ModelCapabilities::fallback(None);
        let plan = build_acquisition_plan(
            AcquisitionMode::Explicit,
            &[
                sel("interfaces", "ON_CHANGE", 0),
                sel("/interfaces/", "ON_CHANGE", 0),
            ],
            &caps,
        );
        match plan {
            AcquisitionPlan::Ready { subscriptions, .. } => assert_eq!(subscriptions.len(), 1),
            _ => panic!("expected ready plan"),
        }
    }

    #[test]
    fn sample_mode_requires_positive_interval() {
        let caps = ModelCapabilities::fallback(None);
        let plan = build_acquisition_plan(
            AcquisitionMode::Explicit,
            &[sel("interfaces", "SAMPLE", 0)],
            &caps,
        );
        assert!(matches!(plan, AcquisitionPlan::Rejected { .. }));
    }

    fn bound_target(
        binding: crate::config::SchemaBindingConfig,
        acquisition: AcquisitionMode,
        collector_id: Option<&str>,
    ) -> TargetConfig {
        TargetConfig {
            address: "10.0.0.9:57400".to_string(),
            enabled: true,
            acquisition,
            collector_id: collector_id.map(str::to_string),
            schema_binding: Some(binding),
            ..Default::default()
        }
    }

    fn valid_binding() -> crate::config::SchemaBindingConfig {
        crate::config::SchemaBindingConfig {
            mode: "schema_bound".to_string(),
            schema_id: "a".repeat(64),
            snapshot_id: 7,
            content_id: Some("content-1".to_string()),
            datastore: Some("ietf-datastores:operational".to_string()),
            evidence_fingerprint: "ev-fp".to_string(),
            plan_id: "approved-plan".to_string(),
            binding_fingerprint: "bind-fp".to_string(),
            approval_revision: 2,
            collector_id: Some("c1".to_string()),
            approved_by: "admin".to_string(),
            approved_at_ns: 123,
        }
    }

    #[test]
    fn schema_bound_guard_accepts_exact_approved_binding() {
        let target = bound_target(valid_binding(), AcquisitionMode::Explicit, Some("c1"));
        assert_eq!(schema_binding_block_reason(&target), None);
    }

    #[test]
    fn schema_bound_guard_rejects_invalid_and_incomplete_bindings() {
        let mut invalid = valid_binding();
        invalid.mode = "schema_bound_invalid".to_string();
        let target = bound_target(invalid, AcquisitionMode::Explicit, Some("c1"));
        assert_eq!(
            schema_binding_block_reason(&target),
            Some("invalid_schema_binding")
        );

        let mut incomplete = valid_binding();
        incomplete.binding_fingerprint.clear();
        let target = bound_target(incomplete, AcquisitionMode::Explicit, Some("c1"));
        assert_eq!(
            schema_binding_block_reason(&target),
            Some("incomplete_schema_binding")
        );
    }

    #[test]
    fn schema_bound_guard_rejects_collector_and_mode_drift() {
        let target = bound_target(valid_binding(), AcquisitionMode::Explicit, Some("c2"));
        assert_eq!(
            schema_binding_block_reason(&target),
            Some("schema_binding_collector_mismatch")
        );
        let target = bound_target(valid_binding(), AcquisitionMode::Automatic, Some("c1"));
        assert_eq!(
            schema_binding_block_reason(&target),
            Some("schema_binding_requires_explicit")
        );
    }

    /// M5.2B review B1: the execution-counter book outlives subscriber
    /// entries — stop/remove/respawn cycles must return the SAME
    /// monotonic counter so recreated executions never restart below
    /// the core's per-target high-water mark.
    #[tokio::test]
    async fn exec_seq_survives_entry_removal_and_respawn() {
        let mut map = SubscriberHandleMap::new();
        let addr = "10.0.0.1:9339".to_string();

        let seq1 = map.exec_seq(&addr);
        seq1.fetch_add(1, Ordering::SeqCst); // simulate minted executions
        seq1.fetch_add(1, Ordering::SeqCst);
        assert_eq!(seq1.load(Ordering::SeqCst), 2);

        // Stop bookkeeping (entry removed) — counter retained.
        map.insert(
            addr.clone(),
            SubscriberEntry {
                shutdown_tx: watch::channel(false).0,
                handle: tokio::spawn(async {}),
                assignment_fingerprint: "fp1".to_string(),
                execution_seq: Arc::clone(&seq1),
            },
        );
        let removed = map.remove(&addr).expect("entry present");
        assert!(map.is_empty());
        drop(removed);

        // Respawn: same counter, same Arc, value preserved.
        let seq2 = map.exec_seq(&addr);
        assert!(Arc::ptr_eq(&seq1, &seq2), "counter must be retained");
        assert_eq!(seq2.load(Ordering::SeqCst), 2);
        seq2.fetch_add(1, Ordering::SeqCst);
        assert_eq!(seq2.load(Ordering::SeqCst), 3);

        // A distinct target gets an independent counter.
        let other = map.exec_seq("10.0.0.2:9339");
        assert_eq!(other.load(Ordering::SeqCst), 0);
        assert!(!Arc::ptr_eq(&seq1, &other));
    }

    fn emitter_with_tx(tx: mpsc::Sender<SessionLifecycleEvent>) -> SessionEmitter {
        let handle = LifecycleHandle {
            tx,
            authority_epoch: 1,
            assignment_generation: 2,
            collector_session_generation: 3,
        };
        SessionEmitter::new(Some(&handle), "dut", "plan", "sess", None)
    }

    #[tokio::test]
    async fn shutdown_emit_waits_bounded_for_channel_capacity() {
        let (tx, mut rx) = mpsc::channel(1);
        let mut emitter = emitter_with_tx(tx);
        emitter.emit("submitted", "fill").await;

        let started = std::time::Instant::now();
        emitter.emit_shutdown("disconnected", "shutdown").await;
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(400) && elapsed < Duration::from_secs(2),
            "bounded shutdown wait should use the configured grace, got {elapsed:?}"
        );
        assert_eq!(rx.recv().await.expect("queued event").state, "submitted");
        assert!(
            rx.try_recv().is_err(),
            "timed-out shutdown event must not be queued"
        );

        emitter.emit_shutdown("disconnected", "shutdown").await;
        let terminal = rx.recv().await.expect("terminal after drain");
        assert_eq!(terminal.state, "disconnected");
        assert_eq!(terminal.transition_seq, 3);
    }

    #[tokio::test]
    async fn shutdown_emit_on_closed_channel_returns_promptly() {
        let (tx, rx) = mpsc::channel::<SessionLifecycleEvent>(1);
        drop(rx);
        let mut emitter = emitter_with_tx(tx);
        let started = std::time::Instant::now();
        emitter.emit_shutdown("disconnected", "shutdown").await;
        assert!(
            started.elapsed() < LIFECYCLE_SHUTDOWN_SEND_TIMEOUT,
            "closed channel must not consume the full shutdown grace"
        );
    }

    #[test]
    fn sample_idle_timeout_scales_with_slowest_sample_interval() {
        let sample = |interval_ns: u64| Subscription {
            mode: SubscriptionMode::Sample as i32,
            sample_interval: interval_ns,
            ..Default::default()
        };
        let on_change = Subscription {
            mode: SubscriptionMode::OnChange as i32,
            ..Default::default()
        };

        assert_eq!(
            sample_stream_idle_timeout(&[on_change.clone()]),
            Duration::ZERO
        );
        assert_eq!(
            sample_stream_idle_timeout(&[sample(10_000_000_000)]),
            SAMPLE_STREAM_IDLE_TIMEOUT
        );
        assert_eq!(
            sample_stream_idle_timeout(&[sample(60_000_000_000)]),
            Duration::from_secs(210)
        );
        assert_eq!(
            sample_stream_idle_timeout(&[sample(5_000_000_000), sample(120_000_000_000)]),
            Duration::from_secs(390)
        );
        assert_eq!(
            sample_stream_idle_timeout(&[sample(0)]),
            SAMPLE_STREAM_IDLE_TIMEOUT
        );
    }

    fn gnmi_sub(origin: &str, raw: &str) -> Subscription {
        Subscription {
            path: Some(crate::gnmi_path::parse_path(origin, raw).expect("sub path")),
            mode: SubscriptionMode::Sample as i32,
            sample_interval: 1,
            ..Default::default()
        }
    }

    #[test]
    fn session_origin_fills_only_unique_authorized_origin() {
        let obs = crate::gnmi_path::parse_path(
            "",
            "interfaces/interface[name=Ethernet1]/state/counters/in-octets",
        )
        .unwrap();
        let openconfig = gnmi_sub("openconfig", "interfaces/interface[name=Ethernet1]");
        let ietf = gnmi_sub("ietf", "interfaces/interface[name=Ethernet1]");

        // Unique authorized origin covering the observed path → filled.
        let out = apply_session_origins(vec![obs.clone()], &[openconfig.clone()]);
        assert_eq!(out[0].origin, "openconfig");

        // Two different authorized origins cover the same path →
        // ambiguous, stays empty (fail closed, never merged).
        let out = apply_session_origins(vec![obs.clone()], &[openconfig.clone(), ietf]);
        assert!(out[0].origin.is_empty());

        // A non-empty observed origin is never overwritten by session
        // context.
        let mut echoed = obs.clone();
        echoed.origin = "vendor-native".to_string();
        let out = apply_session_origins(vec![echoed], &[openconfig.clone()]);
        assert_eq!(out[0].origin, "vendor-native");

        // No authorized subscription covers this path → stays empty.
        let other = crate::gnmi_path::parse_path("", "system/state/hostname").unwrap();
        let out = apply_session_origins(vec![other], &[openconfig]);
        assert!(out[0].origin.is_empty());
    }
}
