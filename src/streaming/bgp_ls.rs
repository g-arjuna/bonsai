use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, watch};
use tracing::{info, warn};

use crate::config::{BgpLsConfig, TargetConfig};
use crate::event_bus::InProcessBus;
use crate::telemetry::TelemetryUpdate;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BgpLsEvent {
    Node {
        timestamp_ns: Option<i64>,
        device_address: Option<String>,
        hostname: Option<String>,
        router_id: String,
        protocol: String,
        asn: Option<u32>,
        name: Option<String>,
        sr_node_sid: Option<u32>,
    },
    Link {
        timestamp_ns: Option<i64>,
        device_address: Option<String>,
        hostname: Option<String>,
        local_router_id: String,
        remote_router_id: String,
        protocol: String,
        local_interface: Option<String>,
        remote_interface: Option<String>,
        igp_metric: Option<u32>,
        te_metric: Option<u32>,
        unreserved_bandwidth_bps: Option<u64>,
        admin_groups: Option<Vec<String>>,
        srlgs: Option<Vec<u32>>,
    },
    SrPolicy {
        timestamp_ns: Option<i64>,
        device_address: Option<String>,
        hostname: Option<String>,
        name: String,
        endpoint: String,
        color: u32,
        preference: Option<u32>,
        binding_sid: Option<u32>,
        status: Option<String>,
    },
}

pub async fn run_bgp_ls_receiver(
    cfg: BgpLsConfig,
    targets: Vec<TargetConfig>,
    bus: Arc<InProcessBus>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let archive = JsonLineArchive::open(&cfg.archive_path).await?;
    let target_map = Arc::new(BgpLsTargetMap::new(&targets));
    let listener = TcpListener::bind(&cfg.tcp_addr)
        .await
        .with_context(|| format!("bind BGP-LS sidecar listener at {}", cfg.tcp_addr))?;
    info!(addr = %cfg.tcp_addr, "BGP-LS sidecar listener started");

    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                info!("BGP-LS sidecar listener stopping");
                break;
            }
            accept = listener.accept() => {
                match accept {
                    Ok((stream, peer)) => {
                        let target_map = Arc::clone(&target_map);
                        let archive = archive.clone();
                        let bus = Arc::clone(&bus);
                        let max_frame_bytes = cfg.max_frame_bytes;
                        tokio::spawn(async move {
                            let mut reader = BufReader::new(stream);
                            let mut line = String::new();
                            loop {
                                line.clear();
                                match reader.read_line(&mut line).await {
                                    Ok(0) => break,
                                    Ok(n) if n > max_frame_bytes => {
                                        warn!(peer = %peer, bytes = n, "BGP-LS JSON frame exceeded limit");
                                        break;
                                    }
                                    Ok(_) => {
                                        let raw = line.trim();
                                        if raw.is_empty() {
                                            continue;
                                        }
                                        match serde_json::from_str::<BgpLsEvent>(raw) {
                                            Ok(event) => {
                                                if let Err(error) = archive.append(raw).await {
                                                    warn!(%error, "failed to archive BGP-LS JSON frame");
                                                }
                                                publish_event(&bus, &target_map, event);
                                            }
                                            Err(error) => warn!(%error, peer = %peer, "failed to parse BGP-LS JSON frame"),
                                        }
                                    }
                                    Err(error) => {
                                        warn!(%error, peer = %peer, "BGP-LS sidecar read failed");
                                        break;
                                    }
                                }
                            }
                        });
                    }
                    Err(error) => warn!(%error, "BGP-LS sidecar accept failed"),
                }
            }
        }
    }

    Ok(())
}

fn publish_event(bus: &Arc<InProcessBus>, target_map: &BgpLsTargetMap, event: BgpLsEvent) {
    let resolved = target_map.resolve(&event);
    let timestamp_ns = match &event {
        BgpLsEvent::Node { timestamp_ns, .. }
        | BgpLsEvent::Link { timestamp_ns, .. }
        | BgpLsEvent::SrPolicy { timestamp_ns, .. } => timestamp_ns.unwrap_or_else(now_ns),
    };
    let path = match &event {
        BgpLsEvent::Node { .. } => "streaming/bgp-ls/node",
        BgpLsEvent::Link { .. } => "streaming/bgp-ls/link",
        BgpLsEvent::SrPolicy { .. } => "streaming/bgp-ls/sr-policy",
    };
    let value = serde_json::to_value(event).unwrap_or_default();
    bus.publish(TelemetryUpdate {
        target: resolved.address,
        vendor: resolved.vendor,
        hostname: resolved.hostname,
        role: resolved.role,
        site: resolved.site,
        timestamp_ns,
        path: path.to_string(),
        value,
    });
}

#[derive(Clone)]
struct JsonLineArchive {
    file: Option<Arc<Mutex<tokio::fs::File>>>,
}

impl JsonLineArchive {
    async fn open(path: &str) -> Result<Self> {
        if path.trim().is_empty() {
            return Ok(Self { file: None });
        }
        if let Some(parent) = Path::new(path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create BGP-LS archive directory {}", parent.display()))?;
        }
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .with_context(|| format!("open BGP-LS archive {}", path))?;
        Ok(Self {
            file: Some(Arc::new(Mutex::new(file))),
        })
    }

    async fn append(&self, line: &str) -> Result<()> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        let mut payload = line.as_bytes().to_vec();
        payload.push(b'\n');
        let mut file = file.lock().await;
        file.write_all(&payload)
            .await
            .context("write BGP-LS archive line")?;
        Ok(())
    }
}

#[derive(Clone, Default)]
struct BgpLsTargetMap {
    entries: Vec<TargetEntry>,
}

#[derive(Clone)]
struct TargetEntry {
    address: String,
    hostname: String,
    vendor: String,
    role: String,
    site: String,
}

#[derive(Clone)]
struct ResolvedTarget {
    address: String,
    hostname: String,
    vendor: String,
    role: String,
    site: String,
}

impl BgpLsTargetMap {
    fn new(targets: &[TargetConfig]) -> Self {
        let entries = targets
            .iter()
            .map(|target| TargetEntry {
                address: target
                    .address
                    .split(':')
                    .next()
                    .unwrap_or(&target.address)
                    .to_string(),
                hostname: target.hostname.clone().unwrap_or_default(),
                vendor: target.vendor.clone().unwrap_or_default(),
                role: target.role.clone().unwrap_or_default(),
                site: target.site.clone().unwrap_or_default(),
            })
            .collect();
        Self { entries }
    }

    fn resolve(&self, event: &BgpLsEvent) -> ResolvedTarget {
        let explicit_address = match event {
            BgpLsEvent::Node { device_address, .. }
            | BgpLsEvent::Link { device_address, .. }
            | BgpLsEvent::SrPolicy { device_address, .. } => device_address.clone(),
        };
        let explicit_hostname = match event {
            BgpLsEvent::Node { hostname, .. }
            | BgpLsEvent::Link { hostname, .. }
            | BgpLsEvent::SrPolicy { hostname, .. } => hostname.clone(),
        };

        if let Some(address) = explicit_address.as_ref()
            && let Some(entry) = self.entries.iter().find(|entry| entry.address == *address)
        {
            return ResolvedTarget {
                address: entry.address.clone(),
                hostname: if entry.hostname.is_empty() {
                    address.clone()
                } else {
                    entry.hostname.clone()
                },
                vendor: entry.vendor.clone(),
                role: entry.role.clone(),
                site: entry.site.clone(),
            };
        }

        if let Some(hostname) = explicit_hostname.as_ref()
            && let Some(entry) = self
                .entries
                .iter()
                .find(|entry| entry.hostname.eq_ignore_ascii_case(hostname))
        {
            return ResolvedTarget {
                address: entry.address.clone(),
                hostname: entry.hostname.clone(),
                vendor: entry.vendor.clone(),
                role: entry.role.clone(),
                site: entry.site.clone(),
            };
        }

        ResolvedTarget {
            address: explicit_address.unwrap_or_default(),
            hostname: explicit_hostname.unwrap_or_default(),
            vendor: String::new(),
            role: String::new(),
            site: String::new(),
        }
    }
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or_default()
}
