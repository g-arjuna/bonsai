use std::time::Duration;

use anyhow::{Context, Result};
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, Channel, ClientTlsConfig};
use tonic::Request;
use tracing::{debug, info, warn};

const BACKOFF_INITIAL: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
// Reset backoff if the connection stayed up longer than this
const BACKOFF_RESET_THRESHOLD: Duration = Duration::from_secs(60);

use crate::proto::gnmi::g_nmi_client::GNmiClient;
use crate::proto::gnmi::{
    subscribe_request, subscription_list, Path, PathElem, SubscribeRequest, SubscriptionList,
    Subscription, SubscriptionMode,
};
use crate::telemetry::TelemetryUpdate;

pub struct GnmiSubscriber {
    target: String,
    username: String,
    password: String,
    /// DER or PEM bytes of the CA certificate used to verify the server's TLS cert.
    ca_cert_pem: Vec<u8>,
    /// TLS server name — must match the CN/SAN in the server cert.
    tls_domain: String,
    /// Channel to the graph writer task.
    tx: tokio::sync::mpsc::Sender<TelemetryUpdate>,
}

impl GnmiSubscriber {
    pub fn new(
        target: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
        ca_cert_pem: Vec<u8>,
        tls_domain: impl Into<String>,
        tx: tokio::sync::mpsc::Sender<TelemetryUpdate>,
    ) -> Self {
        Self {
            target: target.into(),
            username: username.into(),
            password: password.into(),
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
        let username = self.username.clone();
        let password = self.password.clone();
        let target = self.target.clone();

        #[allow(clippy::result_large_err)]
        let mut client = GNmiClient::with_interceptor(channel, move |mut req: Request<()>| {
            req.metadata_mut().insert(
                "username",
                MetadataValue::try_from(username.as_str()).unwrap(),
            );
            req.metadata_mut().insert(
                "password",
                MetadataValue::try_from(password.as_str()).unwrap(),
            );
            Ok(req)
        });

        let req = SubscribeRequest {
            request: Some(subscribe_request::Request::Subscribe(SubscriptionList {
                subscription: vec![
                    Subscription {
                        path: Some(interface_counters_path()),
                        mode: SubscriptionMode::Sample as i32,
                        sample_interval: 10_000_000_000, // 10s in nanoseconds
                        ..Default::default()
                    },
                    Subscription {
                        path: Some(bgp_neighbors_path()),
                        mode: SubscriptionMode::OnChange as i32,
                        ..Default::default()
                    },
                    Subscription {
                        path: Some(lldp_neighbors_path()),
                        mode: SubscriptionMode::OnChange as i32,
                        ..Default::default()
                    },
                ],
                mode: subscription_list::Mode::Stream as i32,
                encoding: crate::proto::gnmi::Encoding::JsonIetf as i32,
                ..Default::default()
            })),
            ..Default::default()
        };

        info!(target = %target, "subscribing to interface counters, BGP neighbors, and LLDP");

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
        let ca_cert = Certificate::from_pem(self.ca_cert_pem.clone());

        let tls = ClientTlsConfig::new()
            .ca_certificate(ca_cert)
            .domain_name(self.tls_domain.clone());

        let endpoint = format!("https://{}", self.target);
        let channel = Channel::from_shared(endpoint.clone())
            .context("invalid endpoint")?
            .tls_config(tls)
            .context("TLS config failed")?
            .timeout(Duration::from_secs(30))
            .connect()
            .await
            .with_context(|| format!("failed to connect to {endpoint}"))?;

        info!(target = %self.target, "connected");
        Ok(channel)
    }
}

fn interface_counters_path() -> Path {
    // SR Linux uses "interface" (singular) not "interfaces" (OpenConfig canonical).
    // This is a known SR Linux path deviation — normalization lives here.
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

fn bgp_neighbors_path() -> Path {
    // SR Linux path for BGP neighbor state under the default network-instance.
    // ON_CHANGE subscription — fires when session state transitions (Idle/Active/Established).
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

fn lldp_neighbors_path() -> Path {
    // SR Linux LLDP neighbor discovery path.
    // ON_CHANGE fires when neighbors are added/removed.
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

fn path_to_string(path: &Path) -> String {
    path.elem
        .iter()
        .map(|e| {
            if e.key.is_empty() {
                e.name.clone()
            } else {
                let keys: String = e
                    .key
                    .iter()
                    .map(|(k, v)| format!("[{k}={v}]"))
                    .collect();
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
