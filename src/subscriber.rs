use std::time::Duration;

use anyhow::{Context, Result};
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, Channel, ClientTlsConfig};
use tonic::Request;
use tracing::{debug, info, warn};

const BACKOFF_INITIAL: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const BACKOFF_RESET_THRESHOLD: Duration = Duration::from_secs(60);

use crate::proto::gnmi::g_nmi_client::GNmiClient;
use crate::proto::gnmi::{
    subscribe_request, subscription_list, CapabilityRequest, Path, PathElem, SubscribeRequest,
    SubscriptionList, Subscription, SubscriptionMode,
};
use crate::telemetry::TelemetryUpdate;

pub struct GnmiSubscriber {
    target: String,
    username: Option<String>,
    password: Option<String>,
    /// Overrides Capabilities detection when set.
    vendor_hint: Option<String>,
    /// None = plaintext gRPC.
    ca_cert_pem: Option<Vec<u8>>,
    tls_domain: String,
    tx: tokio::sync::mpsc::Sender<TelemetryUpdate>,
}

impl GnmiSubscriber {
    pub fn new(
        target: impl Into<String>,
        username: Option<String>,
        password: Option<String>,
        vendor_hint: Option<String>,
        tls_domain: impl Into<String>,
        ca_cert_pem: Option<Vec<u8>>,
        tx: tokio::sync::mpsc::Sender<TelemetryUpdate>,
    ) -> Self {
        Self {
            target: target.into(),
            username,
            password,
            vendor_hint,
            ca_cert_pem,
            tls_domain: tls_domain.into(),
            tx,
        }
    }

    pub async fn run_forever(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let mut backoff = BACKOFF_INITIAL;
        loop {
            let start = std::time::Instant::now();
            tokio::select! {
                _ = shutdown.changed() => {
                    info!(target = %self.target, "shutdown signal received");
                    return;
                }
                result = self.subscribe_telemetry() => {
                    if let Err(e) = result {
                        warn!(target = %self.target, error = %e, "subscription failed");
                    }
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

    pub async fn subscribe_telemetry(&self) -> Result<()> {
        let channel = self.connect().await?;
        let target = self.target.clone();

        // Detect vendor on a bare client (no interceptor needed for Capabilities).
        let vendor = match &self.vendor_hint {
            Some(v) => {
                debug!(target = %target, vendor = %v, "using configured vendor hint");
                v.clone()
            }
            None => {
                let mut bare = GNmiClient::new(channel.clone());
                detect_vendor(&mut bare, &target).await
            }
        };

        let username = self.username.clone();
        let password = self.password.clone();

        #[allow(clippy::result_large_err)]
        let mut client = GNmiClient::with_interceptor(channel, move |mut req: Request<()>| {
            if let Some(ref u) = username {
                if let Ok(v) = MetadataValue::try_from(u.as_str()) {
                    req.metadata_mut().insert("username", v);
                }
            }
            if let Some(ref p) = password {
                if let Ok(v) = MetadataValue::try_from(p.as_str()) {
                    req.metadata_mut().insert("password", v);
                }
            }
            Ok(req)
        });

        let subscriptions = build_subscriptions(&vendor);
        let req = SubscribeRequest {
            request: Some(subscribe_request::Request::Subscribe(SubscriptionList {
                subscription: subscriptions,
                mode: subscription_list::Mode::Stream as i32,
                encoding: crate::proto::gnmi::Encoding::JsonIetf as i32,
                ..Default::default()
            })),
            ..Default::default()
        };

        info!(target = %target, vendor = %vendor, "subscribing");

        let mut stream = client
            .subscribe(tokio_stream::once(req))
            .await
            .context("Subscribe RPC failed")?
            .into_inner();

        loop {
            match stream.message().await {
                Ok(Some(response)) => {
                    use crate::proto::gnmi::subscribe_response::Response;
                    if let Some(resp) = response.response {
                        match resp {
                            Response::Update(notif) => {
                                for update in &notif.update {
                                    let path = update
                                        .path
                                        .as_ref()
                                        .map(path_to_string)
                                        .unwrap_or_default();
                                    let val = update
                                        .val
                                        .as_ref()
                                        .map(typed_value_to_json)
                                        .unwrap_or(serde_json::Value::Null);
                                    let msg = TelemetryUpdate {
                                        target: target.clone(),
                                        vendor: vendor.clone(),
                                        timestamp_ns: notif.timestamp,
                                        path: path.clone(),
                                        value: val,
                                    };
                                    if self.tx.send(msg).await.is_err() {
                                        warn!(target = %target, "graph writer channel closed — stopping subscriber");
                                        return Ok(());
                                    }
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

// ── vendor detection ──────────────────────────────────────────────────────────

async fn detect_vendor(
    client: &mut GNmiClient<tonic::transport::Channel>,
    target: &str,
) -> String {
    match client.capabilities(CapabilityRequest::default()).await {
        Ok(resp) => {
            let models = resp.into_inner().supported_models;
            let vendor = if models.iter().any(|m| m.name.starts_with("srl_nokia")) {
                "nokia_srl"
            } else if models.iter().any(|m| m.name.starts_with("Cisco-IOS-XR")) {
                "cisco_xrd"
            } else if models.iter().any(|m| m.name.to_lowercase().starts_with("junos")) {
                "juniper_crpd"
            } else if models
                .iter()
                .any(|m| m.name.to_lowercase().contains("arista") || m.name.contains("EOS"))
            {
                "arista_ceos"
            } else {
                "openconfig"
            };
            info!(target, vendor, "vendor detected via Capabilities");
            vendor.to_string()
        }
        Err(e) => {
            warn!(target, error = %e, "Capabilities RPC failed — defaulting to openconfig paths");
            "openconfig".to_string()
        }
    }
}

// ── subscription path selection ───────────────────────────────────────────────

fn build_subscriptions(vendor: &str) -> Vec<Subscription> {
    match vendor {
        "nokia_srl" => vec![
            Subscription {
                path: Some(srl_interface_counters_path()),
                mode: SubscriptionMode::Sample as i32,
                sample_interval: 10_000_000_000,
                ..Default::default()
            },
            Subscription {
                path: Some(srl_bgp_neighbors_path()),
                mode: SubscriptionMode::OnChange as i32,
                ..Default::default()
            },
            Subscription {
                path: Some(srl_lldp_neighbors_path()),
                mode: SubscriptionMode::OnChange as i32,
                ..Default::default()
            },
        ],
        _ => vec![
            Subscription {
                path: Some(oc_interface_counters_path()),
                mode: SubscriptionMode::Sample as i32,
                sample_interval: 10_000_000_000,
                ..Default::default()
            },
            Subscription {
                path: Some(oc_bgp_neighbors_path()),
                mode: SubscriptionMode::OnChange as i32,
                ..Default::default()
            },
        ],
    }
}

// ── SR Linux native paths ─────────────────────────────────────────────────────

fn srl_interface_counters_path() -> Path {
    Path {
        elem: vec![
            PathElem {
                name: "interface".into(),
                key: [("name".to_string(), "*".to_string())].into(),
            },
            PathElem { name: "statistics".into(), key: Default::default() },
        ],
        ..Default::default()
    }
}

fn srl_bgp_neighbors_path() -> Path {
    Path {
        elem: vec![
            PathElem {
                name: "network-instance".into(),
                key: [("name".to_string(), "default".to_string())].into(),
            },
            PathElem { name: "protocols".into(), key: Default::default() },
            PathElem { name: "bgp".into(), key: Default::default() },
            PathElem {
                name: "neighbor".into(),
                key: [("peer-address".to_string(), "*".to_string())].into(),
            },
        ],
        ..Default::default()
    }
}

fn srl_lldp_neighbors_path() -> Path {
    Path {
        elem: vec![
            PathElem { name: "system".into(), key: Default::default() },
            PathElem { name: "lldp".into(), key: Default::default() },
            PathElem {
                name: "interface".into(),
                key: [("name".to_string(), "*".to_string())].into(),
            },
            PathElem {
                name: "neighbor".into(),
                key: [("id".to_string(), "*".to_string())].into(),
            },
        ],
        ..Default::default()
    }
}

// ── OpenConfig paths (XRd, cRPD, cEOS) ───────────────────────────────────────

fn oc_interface_counters_path() -> Path {
    Path {
        elem: vec![
            PathElem { name: "interfaces".into(), key: Default::default() },
            PathElem {
                name: "interface".into(),
                key: [("name".to_string(), "*".to_string())].into(),
            },
            PathElem { name: "state".into(), key: Default::default() },
            PathElem { name: "counters".into(), key: Default::default() },
        ],
        ..Default::default()
    }
}

fn oc_bgp_neighbors_path() -> Path {
    Path {
        elem: vec![
            PathElem { name: "network-instances".into(), key: Default::default() },
            PathElem {
                name: "network-instance".into(),
                key: [("name".to_string(), "*".to_string())].into(),
            },
            PathElem { name: "protocols".into(), key: Default::default() },
            PathElem {
                name: "protocol".into(),
                key: [
                    ("identifier".to_string(), "BGP".to_string()),
                    ("name".to_string(), "*".to_string()),
                ]
                .into(),
            },
            PathElem { name: "bgp".into(), key: Default::default() },
            PathElem { name: "neighbors".into(), key: Default::default() },
            PathElem {
                name: "neighbor".into(),
                key: [("neighbor-address".to_string(), "*".to_string())].into(),
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
                let keys: String =
                    e.key.iter().map(|(k, v)| format!("[{k}={v}]")).collect();
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
        Some(Value::JsonVal(b)) => serde_json::from_slice(b).unwrap_or(
            serde_json::Value::String(String::from_utf8_lossy(b).into_owned()),
        ),
        Some(Value::StringVal(s)) => serde_json::Value::String(s.clone()),
        Some(Value::IntVal(i)) => serde_json::json!(i),
        Some(Value::UintVal(u)) => serde_json::json!(u),
        Some(Value::BoolVal(b)) => serde_json::Value::Bool(*b),
        Some(Value::FloatVal(f)) => serde_json::json!(f),
        _ => serde_json::Value::Null,
    }
}
