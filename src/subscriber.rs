use std::{collections::HashMap, time::Duration};
use std::sync::Arc;
use tokio::sync::{watch, mpsc};
use tokio::task::JoinHandle;

use anyhow::{Context, Result};
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, Channel, ClientTlsConfig};
use tracing::{debug, info, warn};

const BACKOFF_INITIAL: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const BACKOFF_RESET_THRESHOLD: Duration = Duration::from_secs(60);

use crate::proto::gnmi::g_nmi_client::GNmiClient;
use crate::proto::gnmi::{
    CapabilityRequest, Path, PathElem, SubscribeRequest, Subscription, SubscriptionList,
    SubscriptionMode, subscribe_request, subscription_list,
};

use crate::config::SelectedSubscriptionPath;
use crate::config::TargetConfig;
use crate::event_bus::InProcessBus;
use crate::subscription_status::{SubscriptionPathExpectation, SubscriptionPlan};
use crate::telemetry::TelemetryUpdate;

pub struct GnmiSubscriber {
    target: String,
    username: Option<String>,
    password: Option<String>,
    /// Overrides the vendor label in logs; model detection still uses Capabilities.
    vendor_hint: Option<String>,
    /// Human-readable hostname for this device (e.g. "srl1"); stored on Device node.
    hostname: Option<String>,
    /// None = plaintext gRPC.
    ca_cert_pem: Option<Vec<u8>>,
    tls_domain: String,
    bus: Arc<InProcessBus>,
    subscription_plan_tx: Option<tokio::sync::mpsc::Sender<SubscriptionPlan>>,
    selected_paths: Vec<SelectedSubscriptionPath>,
}

impl GnmiSubscriber {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: impl Into<String>,
        username: Option<String>,
        password: Option<String>,
        vendor_hint: Option<String>,
        hostname: Option<String>,
        tls_domain: impl Into<String>,
        ca_cert_pem: Option<Vec<u8>>,
        bus: Arc<InProcessBus>,
        subscription_plan_tx: Option<tokio::sync::mpsc::Sender<SubscriptionPlan>>,
        selected_paths: Vec<SelectedSubscriptionPath>,
    ) -> Self {
        Self {
            target: target.into(),
            username,
            password,
            vendor_hint,
            hostname,
            ca_cert_pem,
            tls_domain: tls_domain.into(),
            bus,
            subscription_plan_tx,
            selected_paths,
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
        let channel = self.connect().await?;
        let target = self.target.clone();

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

        let selected_subscriptions = build_selected_subscriptions(&self.selected_paths);
        let using_selected_paths = !selected_subscriptions.is_empty();
        let subscriptions = if using_selected_paths {
            selected_subscriptions
        } else {
            if !self.selected_paths.is_empty() {
                warn!(
                    target = %target,
                    selected_paths = self.selected_paths.len(),
                    "configured subscription path selection produced no valid paths; falling back to capabilities"
                );
            }
            build_subscriptions(&caps)
        };
        let plan_paths = subscriptions
            .iter()
            .filter_map(subscription_expectation)
            .collect::<Vec<_>>();
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
            selected     = using_selected_paths,
            "subscribing"
        );

        let req = SubscribeRequest {
            request: Some(subscribe_request::Request::Subscribe(SubscriptionList {
                subscription: subscriptions,
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

        if let Some(tx) = &self.subscription_plan_tx
            && let Err(error) = tx
                .send(SubscriptionPlan {
                    target: target.clone(),
                    paths: plan_paths,
                })
                .await
        {
            warn!(target = %target, %error, "failed to publish subscription plan");
        }

        loop {
            let next_message = tokio::select! {
                _ = shutdown.changed() => {
                    info!(target = %target, "shutdown signal received during telemetry stream");
                    return Ok(());
                }
                message = stream.message() => message,
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
                                let mut blobs: Vec<(String, serde_json::Value)> = Vec::new();
                                let mut leaf_groups: HashMap<
                                    String,
                                    serde_json::Map<String, serde_json::Value>,
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
                                        leaf_groups
                                            .entry(path[..slash].to_string())
                                            .or_default()
                                            .insert(path[slash + 1..].to_string(), val);
                                        continue;
                                    }
                                    blobs.push((path, val));
                                }

                                let all_updates = blobs.into_iter().chain(
                                    leaf_groups
                                        .into_iter()
                                        .map(|(p, obj)| (p, serde_json::Value::Object(obj))),
                                );
                                for (path, val) in all_updates {
                                    debug!(target = %target, path = %path, "update");
                                    let msg = TelemetryUpdate {
                                        target: target.clone(),
                                        vendor: caps.vendor_label.clone(),
                                        hostname: self.hostname.clone().unwrap_or_default(),
                                        timestamp_ns: notif.timestamp,
                                        path,
                                        value: val,
                                    };
                                    self.bus.publish(msg);
                                }
                            }
                            Response::SyncResponse(sync) => {
                                debug!(target = %target, sync = sync, "sync response");
                            }
                            Response::Error(e) => {
                                warn!(target = %target, code = e.code, message = %e.message, "gNMI error");
                            }
                        }
                    }
                }
                Ok(None) => {
                    warn!(target = %target, "stream ended");
                    break;
                }
                Err(e) => {
                    warn!(target = %target, error = %e, "stream error");
                    break;
                }
            }
        }

        Ok(())
    }

    async fn connect(&self) -> Result<Channel> {
        let use_tls = self.ca_cert_pem.is_some();
        let scheme = if use_tls { "https" } else { "http" };
        let endpoint = format!("{scheme}://{}", self.target);

        let mut builder = Channel::from_shared(endpoint.clone())
            .context("invalid endpoint")?
            .timeout(Duration::from_secs(30));

        if let Some(cert_pem) = &self.ca_cert_pem {
            let ca_cert = Certificate::from_pem(cert_pem.clone());
            let tls = ClientTlsConfig::new()
                .ca_certificate(ca_cert)
                .domain_name(self.tls_domain.clone());
            builder = builder.tls_config(tls).context("TLS config failed")?;
        }

        let channel = builder
            .connect()
            .await
            .with_context(|| format!("failed to connect to {endpoint}"))?;

        info!(target = %self.target, tls = %use_tls, "connected");
        Ok(channel)
    }
}

pub type SubscriberHandleMap = HashMap<
    String,
    (
        watch::Sender<bool>,
        JoinHandle<()>,
    ),
>;

pub async fn stop_subscriber(address: &str, subscribers: &mut SubscriberHandleMap) {
    if let Some((shutdown_tx, handle)) = subscribers.remove(address) {
        let _ = shutdown_tx.send(true);
        let _ = handle.await;
        info!(address = %address, "subscriber stopped");
    }
}

pub async fn stop_all_subscribers(subscribers: &mut SubscriberHandleMap) {
    let addresses: Vec<String> = subscribers.keys().cloned().collect();
    for address in addresses {
        stop_subscriber(&address, subscribers).await;
    }
}

pub async fn load_ca_cert_pem(target: &TargetConfig) -> Result<Option<Vec<u8>>> {
    match &target.ca_cert {
        Some(path) => {
            let bytes = tokio::fs::read(path).await?;
            Ok(Some(bytes))
        }
        None => Ok(None),
    }
}

pub async fn spawn_subscriber_with_creds(
    target: TargetConfig,
    bus: &std::sync::Arc<InProcessBus>,
    subscription_plan_tx: Option<&mpsc::Sender<SubscriptionPlan>>,
    subscribers: &mut SubscriberHandleMap,
) -> Result<()> {
    let address = target.address.clone();
    if !target.enabled {
        info!(address = %address, "subscriber start skipped because target is disabled");
        return Ok(());
    }
    if subscribers.contains_key(&address) {
        info!(address = %address, "subscriber already running");
        return Ok(());
    }

    let ca_cert_pem = load_ca_cert_pem(&target).await?;

    let subscriber = GnmiSubscriber::new(
        target.address.clone(),
        target.username.clone(),
        target.password.clone(),
        target.vendor.clone(),
        target.hostname.clone(),
        target.tls_domain.clone().unwrap_or_default(),
        ca_cert_pem,
        std::sync::Arc::clone(bus),
        subscription_plan_tx.cloned(),
        target.selected_paths.clone(),
    );
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(async move { subscriber.run_forever(shutdown_rx).await });
    subscribers.insert(address.clone(), (shutdown_tx, handle));
    info!(address = %address, "subscriber started");
    Ok(())
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

fn build_selected_subscriptions(selected_paths: &[SelectedSubscriptionPath]) -> Vec<Subscription> {
    selected_paths
        .iter()
        .filter_map(subscription_from_selected_path)
        .collect()
}

fn subscription_from_selected_path(selected: &SelectedSubscriptionPath) -> Option<Subscription> {
    let Some(path) = selected_path_to_gnmi(&selected.origin, &selected.path) else {
        warn!(
            path = %selected.path,
            "skipping selected subscription path with invalid syntax"
        );
        return None;
    };
    match selected.mode.trim().to_ascii_uppercase().as_str() {
        "SAMPLE" => Some(sub_sample(path, selected.sample_interval_ns)),
        "ON_CHANGE" => Some(sub_on_change(path)),
        other => {
            warn!(
                path = %selected.path,
                mode = %other,
                "skipping selected subscription path with unsupported mode"
            );
            None
        }
    }
}

fn selected_path_to_gnmi(origin: &str, raw_path: &str) -> Option<Path> {
    let mut elem = Vec::new();
    for segment in raw_path.split('/').filter(|segment| !segment.is_empty()) {
        let (name, key) = parse_selected_path_segment(segment)?;
        elem.push(PathElem { name, key });
    }

    if elem.is_empty() {
        return None;
    }

    Some(Path {
        origin: origin.trim().to_string(),
        elem,
        ..Default::default()
    })
}

fn parse_selected_path_segment(segment: &str) -> Option<(String, HashMap<String, String>)> {
    let first_key = segment.find('[').unwrap_or(segment.len());
    let name = segment[..first_key].trim();
    if name.is_empty() {
        return None;
    }

    let mut key = HashMap::new();
    let mut rest = &segment[first_key..];
    while !rest.is_empty() {
        let after_open = rest.strip_prefix('[')?;
        let close = after_open.find(']')?;
        let pair = &after_open[..close];
        let (key_name, key_value) = pair.split_once('=')?;
        if key_name.trim().is_empty() || key_value.trim().is_empty() {
            return None;
        }
        key.insert(key_name.trim().to_string(), key_value.trim().to_string());
        rest = &after_open[close + 1..];
    }

    Some((name.to_string(), key))
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

fn path_to_string(path: &Path) -> String {
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
            path: "interfaces".to_string(),
            origin: "openconfig".to_string(),
            mode: "SAMPLE".to_string(),
            sample_interval_ns: 10_000_000_000,
            rationale: "operator selected".to_string(),
            optional: false,
        };

        let sub = subscription_from_selected_path(&selected).expect("subscription");
        assert_eq!(sub.mode, SubscriptionMode::Sample as i32);
        assert_eq!(sub.sample_interval, 10_000_000_000);
        assert_eq!(sub.path.as_ref().expect("path").origin, "openconfig");
    }
}
