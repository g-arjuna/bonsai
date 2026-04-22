use std::collections::HashMap;
use std::fs;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{info, warn};

use bonsai::{
    api::{
        BonsaiGraphServer, BonsaiService,
        pb::{
            AddDeviceRequest, ListManagedDevicesRequest, ManagedDevice, RemoveDeviceRequest,
            UpdateDeviceRequest, bonsai_graph_client::BonsaiGraphClient,
        },
    },
    archive, config,
    config::TargetConfig,
    credentials::{CredentialVault, ResolvedCredential},
    event_bus::InProcessBus,
    graph, ingest,
    registry::{ApiRegistry, DeviceRegistry, RegistryChange},
    retention, subscriber,
    subscription_status::{self, SubscriptionPlan},
    telemetry::TelemetryEvent,
};
use metrics_exporter_prometheus::PrometheusBuilder;
use tonic::codec::CompressionEncoding;
use tonic::transport::{Certificate, Identity, ServerTlsConfig};

const CONFIG_PATH: &str = "bonsai.toml";
const GRAPH_PATH_DEFAULT: &str = "bonsai.db";
const REGISTRY_PATH: &str = "bonsai-registry.json";

type SubscriberHandleMap = HashMap<
    String,
    (
        tokio::sync::watch::Sender<bool>,
        tokio::task::JoinHandle<()>,
    ),
>;

#[tokio::main]
async fn main() -> Result<()> {
    install_rustls_crypto_provider();

    if let Some(command) = DeviceCliCommand::parse()? {
        return run_device_cli(command).await;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("bonsai=debug".parse()?),
        )
        .init();

    info!("bonsai starting - distributed runtime capable");

    let config_path = config_path();
    let cfg = config::load(&config_path).await?;
    let runtime_mode = cfg.runtime.parsed_mode()?;
    let run_core = runtime_mode.runs_core();
    let run_collector = runtime_mode.runs_collector();
    info!(
        mode = ?runtime_mode,
        run_core,
        run_collector,
        "runtime mode selected"
    );

    if run_core && !cfg.metrics_addr.is_empty() {
        let metrics_addr: std::net::SocketAddr = cfg
            .metrics_addr
            .parse()
            .with_context(|| format!("invalid metrics_addr '{}'", cfg.metrics_addr))?;
        PrometheusBuilder::new()
            .with_http_listener(metrics_addr)
            .install()
            .context("failed to install Prometheus metrics exporter")?;
        info!(%metrics_addr, "Prometheus metrics listening");
    }

    let bus = InProcessBus::new(cfg.event_bus.capacity);
    let debounce_secs = cfg.event_bus.counter_debounce_secs;

    let graph = if run_core {
        let graph_path = if cfg.graph_path.is_empty() {
            GRAPH_PATH_DEFAULT
        } else {
            cfg.graph_path.as_str()
        };

        Some(std::sync::Arc::new(
            tokio::task::spawn_blocking({
                let p = graph_path.to_string();
                move || graph::GraphStore::open(&p)
            })
            .await
            .context("graph open panicked")?
            .context("graph open failed")?,
        ))
    } else {
        info!("collector-only mode selected; graph store and local API are disabled");
        None
    };

    if let Some(graph) = &graph {
        let graph_writer = std::sync::Arc::clone(graph);
        let mut rx = bus.subscribe();
        tokio::spawn(async move {
            let mut last_counter_write: HashMap<String, Instant> = HashMap::new();
            let debounce = Duration::from_secs(debounce_secs);

            loop {
                match rx.recv().await {
                    Ok(update) => {
                        let classified = update.classify();
                        let is_counter =
                            matches!(classified, TelemetryEvent::InterfaceStats { .. });

                        if is_counter {
                            let key = format!(
                                "{}:{}",
                                update.target,
                                if let TelemetryEvent::InterfaceStats { ref if_name } = classified {
                                    if_name.clone()
                                } else {
                                    String::new()
                                }
                            );
                            let now = Instant::now();
                            let skip = last_counter_write
                                .get(&key)
                                .is_some_and(|t| now.duration_since(*t) < debounce);
                            if skip {
                                continue;
                            }
                            last_counter_write.insert(key, now);
                        }

                        if let Err(error) = graph_writer.write(update).await {
                            warn!(%error, "graph write failed");
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped)) => {
                        warn!(dropped, "graph writer lagged on event bus");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        info!("event bus closed - graph writer stopping");
                        break;
                    }
                }
            }
        });
    }

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    if cfg.archive.enabled && run_collector {
        let archive_root = std::path::PathBuf::from(&cfg.archive.path);
        let flush_interval = Duration::from_secs(cfg.archive.flush_interval_seconds);
        let max_batch_rows = cfg.archive.max_batch_rows;
        let bus_for_archive = std::sync::Arc::clone(&bus);
        let archive_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            if let Err(error) = archive::run_archiver(
                bus_for_archive,
                archive_root,
                flush_interval,
                max_batch_rows,
                archive_shutdown,
            )
            .await
            {
                warn!(%error, "archive consumer stopped");
            }
        });
    } else if cfg.archive.enabled {
        info!(
            "archive enabled but runtime mode has no collector role; skipping collector-local archive"
        );
    }

    let registry = std::sync::Arc::new(ApiRegistry::open(REGISTRY_PATH, cfg.target.clone())?);
    let credentials = std::sync::Arc::new(CredentialVault::open(
        &cfg.credentials.path,
        &cfg.credentials.passphrase_env,
    )?);
    if credentials.is_unlocked()? {
        info!(
            path = %cfg.credentials.path,
            "credential vault unlocked"
        );
    } else {
        info!(
            path = %cfg.credentials.path,
            passphrase_env = %cfg.credentials.passphrase_env,
            "credential vault locked; alias-based credentials are unavailable until restart with passphrase"
        );
    }
    if let Some(graph) = &graph {
        match registry.list_active() {
            Ok(targets) => {
                graph
                    .sync_sites_from_targets(targets)
                    .await
                    .context("failed to sync registry sites into graph")?;
                info!("registry site labels synced into graph");
            }
            Err(error) => warn!(%error, "failed to list managed devices for site graph sync"),
        }
    }

    let subscription_plan_tx = if let Some(graph) = &graph {
        let (subscription_plan_tx, subscription_plan_rx) =
            tokio::sync::mpsc::channel::<SubscriptionPlan>(128);
        let verifier_store = std::sync::Arc::clone(graph);
        let verifier_bus = std::sync::Arc::clone(&bus);
        let verifier_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            subscription_status::run_subscription_verifier(
                verifier_store,
                verifier_bus,
                subscription_plan_rx,
                verifier_shutdown,
            )
            .await;
        });
        Some(subscription_plan_tx)
    } else {
        None
    };

    let subscriber_manager = if run_collector {
        let registry = std::sync::Arc::clone(&registry);
        let credentials = std::sync::Arc::clone(&credentials);
        let bus = std::sync::Arc::clone(&bus);
        let subscription_plan_tx = subscription_plan_tx.clone();
        let mut shutdown = shutdown_rx.clone();
        Some(tokio::spawn(async move {
            let mut change_rx = registry.subscribe_changes();
            let mut subscribers: SubscriberHandleMap = HashMap::new();

            match registry.list_active() {
                Ok(targets) => {
                    for target in targets {
                        if !target.enabled {
                            info!(address = %target.address, "subscriber disabled by registry");
                            continue;
                        }
                        if let Err(error) = spawn_subscriber(
                            target,
                            &credentials,
                            &bus,
                            subscription_plan_tx.as_ref(),
                            &mut subscribers,
                        )
                        .await
                        {
                            warn!(%error, "initial subscriber start failed");
                        }
                    }
                }
                Err(error) => warn!(%error, "failed to list managed devices at startup"),
            }

            loop {
                tokio::select! {
                    _ = shutdown.changed() => {
                        info!("subscriber manager received shutdown");
                        break;
                    }
                    maybe_change = change_rx.recv() => {
                        let Some(change) = maybe_change else {
                            info!("registry change channel closed");
                            break;
                        };

                        match change {
                            RegistryChange::Added(target) => {
                                if let Err(error) = spawn_subscriber(target, &credentials, &bus, subscription_plan_tx.as_ref(), &mut subscribers).await {
                                    warn!(%error, "failed to start subscriber for added device");
                                }
                            }
                            RegistryChange::Updated(target) => {
                                if target.enabled {
                                    if let Err(error) = restart_subscriber(target, &credentials, &bus, subscription_plan_tx.as_ref(), &mut subscribers).await {
                                        warn!(%error, "failed to restart subscriber for updated device");
                                    }
                                } else {
                                    stop_subscriber(&target.address, &mut subscribers).await;
                                }
                            }
                            RegistryChange::Removed(address) => {
                                stop_subscriber(&address, &mut subscribers).await;
                            }
                        }
                    }
                }
            }

            stop_all_subscribers(&mut subscribers).await;
        }))
    } else {
        None
    };

    if run_collector && !run_core {
        let forwarder_bus = std::sync::Arc::clone(&bus);
        let core_endpoint = cfg.runtime.core_ingest_endpoint.clone();
        let collector_id = cfg.runtime.collector_id.clone();
        let queue_config = cfg.collector.queue.clone();
        let tls_config = cfg.runtime.tls.clone();
        let forwarder_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            ingest::run_core_forwarder(
                forwarder_bus,
                core_endpoint,
                collector_id,
                queue_config,
                tls_config,
                forwarder_shutdown,
            )
            .await;
        });
    }

    if let Some(graph) = &graph {
        let api_addr = cfg
            .api_addr
            .parse()
            .with_context(|| format!("invalid api_addr '{}'", cfg.api_addr))?;
        let svc = BonsaiGraphServer::new(BonsaiService::new(
            std::sync::Arc::clone(graph),
            std::sync::Arc::clone(&registry),
            std::sync::Arc::clone(&credentials),
            std::sync::Arc::clone(&bus),
        ))
        .accept_compressed(CompressionEncoding::Zstd);
        let mut server = tonic::transport::Server::builder();
        if cfg.runtime.tls.enabled {
            server = server
                .tls_config(server_tls_config(&cfg.runtime.tls)?)
                .context("failed to configure runtime.tls for gRPC server")?;
            info!(
                %api_addr,
                ingest_compression = "zstd",
                mtls = true,
                "gRPC API and telemetry ingest server listening"
            );
        } else {
            info!(%api_addr, ingest_compression = "zstd", mtls = false, "gRPC API and telemetry ingest server listening");
        }
        tokio::spawn(async move {
            if let Err(error) = server.add_service(svc).serve(api_addr).await {
                warn!(%error, "gRPC server error");
            }
        });

        let http_store = std::sync::Arc::clone(graph);
        let http_addr: std::net::SocketAddr = "0.0.0.0:3000".parse().unwrap();
        info!(%http_addr, "HTTP UI server listening");
        tokio::spawn(async move {
            let listener = tokio::net::TcpListener::bind(http_addr)
                .await
                .expect("failed to bind HTTP port 3000");
            axum::serve(
                listener,
                bonsai::http_server::router(
                    http_store,
                    std::sync::Arc::clone(&registry),
                    std::sync::Arc::clone(&credentials),
                ),
            )
            .await
            .expect("HTTP server error");
        });

        if cfg.retention.enabled {
            let store = std::sync::Arc::clone(graph);
            let max_age_h = cfg.retention.max_age_hours;
            let max_count = cfg.retention.max_state_change_events;
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(3600));
                loop {
                    interval.tick().await;
                    let cutoff =
                        time::OffsetDateTime::now_utc() - time::Duration::hours(max_age_h as i64);
                    if let Err(error) =
                        retention::prune_events(std::sync::Arc::clone(&store), cutoff).await
                    {
                        warn!(%error, "retention age-prune failed");
                    }
                    if let Err(error) =
                        retention::prune_events_by_count(std::sync::Arc::clone(&store), max_count)
                            .await
                    {
                        warn!(%error, "retention count-prune failed");
                    }
                }
            });
        }
    }

    tokio::signal::ctrl_c().await?;
    info!("Ctrl+C received - shutting down");
    let _ = shutdown_tx.send(true);
    if let Some(subscriber_manager) = subscriber_manager {
        let _ = subscriber_manager.await;
    }

    if let Some(graph) = &graph {
        graph::log_graph_summary(graph.db().as_ref());
    }
    info!("bonsai stopped");
    Ok(())
}

fn server_tls_config(tls: &config::RuntimeTlsConfig) -> Result<ServerTlsConfig> {
    let cert_path = required_tls_path(tls.cert.as_deref(), "runtime.tls.cert")?;
    let key_path = required_tls_path(tls.key.as_deref(), "runtime.tls.key")?;
    let ca_path = required_tls_path(tls.ca_cert.as_deref(), "runtime.tls.ca_cert")?;
    let cert = fs::read(cert_path)
        .with_context(|| format!("failed to read runtime.tls.cert '{cert_path}'"))?;
    let key = fs::read(key_path)
        .with_context(|| format!("failed to read runtime.tls.key '{key_path}'"))?;
    let ca = fs::read(ca_path)
        .with_context(|| format!("failed to read runtime.tls.ca_cert '{ca_path}'"))?;

    Ok(ServerTlsConfig::new()
        .identity(Identity::from_pem(cert, key))
        .client_ca_root(Certificate::from_pem(ca)))
}

fn required_tls_path<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("{field} is required when runtime.tls.enabled = true"))
}

enum DeviceCliCommand {
    Help,
    List,
    Add(Box<DeviceCliAdd>),
    Remove { address: String },
    SetEnabled { address: String, enabled: bool },
    Restart { address: String },
}

struct DeviceCliAdd {
    address: String,
    hostname: Option<String>,
    vendor: Option<String>,
    role: Option<String>,
    site: Option<String>,
    credential_alias: Option<String>,
    username_env: Option<String>,
    password_env: Option<String>,
    tls_domain: Option<String>,
    ca_cert: Option<String>,
    enabled: bool,
}

impl DeviceCliCommand {
    fn parse() -> Result<Option<Self>> {
        let mut args = std::env::args().skip(1).collect::<Vec<_>>();
        if args.first().map(String::as_str) != Some("device") {
            return Ok(None);
        }
        args.remove(0);
        let Some(action) = args.first().cloned() else {
            return Ok(Some(Self::Help));
        };
        args.remove(0);

        match action.as_str() {
            "list" => Ok(Some(Self::List)),
            "add" => Ok(Some(Self::Add(Box::new(DeviceCliAdd::parse(args)?)))),
            "remove" => Ok(Some(Self::Remove {
                address: parse_address_arg(args)?,
            })),
            "stop" => Ok(Some(Self::SetEnabled {
                address: parse_address_arg(args)?,
                enabled: false,
            })),
            "start" => Ok(Some(Self::SetEnabled {
                address: parse_address_arg(args)?,
                enabled: true,
            })),
            "restart" => Ok(Some(Self::Restart {
                address: parse_address_arg(args)?,
            })),
            "help" | "--help" | "-h" => Ok(Some(Self::Help)),
            other => anyhow::bail!("unknown device command '{other}'"),
        }
    }
}

impl DeviceCliAdd {
    fn parse(args: Vec<String>) -> Result<Self> {
        let mut add = Self {
            address: String::new(),
            hostname: None,
            vendor: None,
            role: None,
            site: None,
            credential_alias: None,
            username_env: None,
            password_env: None,
            tls_domain: None,
            ca_cert: None,
            enabled: true,
        };

        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--address" => add.address = require_flag_value("--address", iter.next())?,
                "--hostname" => add.hostname = Some(require_flag_value("--hostname", iter.next())?),
                "--vendor" => add.vendor = Some(require_flag_value("--vendor", iter.next())?),
                "--role" => add.role = Some(require_flag_value("--role", iter.next())?),
                "--site" => add.site = Some(require_flag_value("--site", iter.next())?),
                "--credential-alias" => {
                    add.credential_alias =
                        Some(require_flag_value("--credential-alias", iter.next())?)
                }
                "--username-env" => {
                    add.username_env = Some(require_flag_value("--username-env", iter.next())?)
                }
                "--password-env" => {
                    add.password_env = Some(require_flag_value("--password-env", iter.next())?)
                }
                "--tls-domain" => {
                    add.tls_domain = Some(require_flag_value("--tls-domain", iter.next())?)
                }
                "--ca-cert" => add.ca_cert = Some(require_flag_value("--ca-cert", iter.next())?),
                "--disabled" => add.enabled = false,
                "--enabled" => add.enabled = true,
                other if add.address.is_empty() && !other.starts_with("--") => {
                    add.address = other.to_string();
                }
                other => anyhow::bail!("unknown device add argument '{other}'"),
            }
        }

        if add.address.trim().is_empty() {
            anyhow::bail!("device add requires --address <host:port>");
        }
        Ok(add)
    }
}

async fn run_device_cli(command: DeviceCliCommand) -> Result<()> {
    if matches!(command, DeviceCliCommand::Help) {
        print_device_cli_usage();
        return Ok(());
    }

    let config_path = config_path();
    let cfg = config::load(&config_path).await?;
    match run_device_cli_api(&command, &cfg.api_addr).await {
        Ok(()) => return Ok(()),
        Err(error) => {
            eprintln!(
                "warning: gRPC device API unavailable ({error:#}); falling back to local registry file"
            );
        }
    }
    run_device_cli_local(command, cfg).await
}

async fn run_device_cli_api(command: &DeviceCliCommand, api_addr: &str) -> Result<()> {
    let endpoint = device_cli_endpoint(api_addr);
    let channel = tonic::transport::Channel::from_shared(endpoint.clone())
        .with_context(|| format!("invalid device API endpoint '{endpoint}'"))?
        .timeout(Duration::from_secs(5))
        .connect()
        .await
        .with_context(|| format!("failed to connect to device API at {endpoint}"))?;
    let mut client = BonsaiGraphClient::new(channel);

    match command {
        DeviceCliCommand::Help => print_device_cli_usage(),
        DeviceCliCommand::List => {
            let response = client
                .list_managed_devices(ListManagedDevicesRequest {})
                .await?
                .into_inner();
            print_managed_devices(response.devices);
        }
        DeviceCliCommand::Add(add) => {
            let response = client
                .add_device(AddDeviceRequest {
                    device: Some(managed_device_from_cli_add(add)),
                })
                .await?
                .into_inner();
            ensure_device_cli_success(response.success, response.error)?;
            let address = response
                .device
                .map(|device| device.address)
                .unwrap_or_else(|| add.address.clone());
            println!("added {address}");
        }
        DeviceCliCommand::Remove { address } => {
            let response = client
                .remove_device(RemoveDeviceRequest {
                    address: address.clone(),
                })
                .await?
                .into_inner();
            ensure_device_cli_success(response.success, response.error)?;
            println!("removed {address}");
        }
        DeviceCliCommand::SetEnabled { address, enabled } => {
            let mut device = find_managed_device(&mut client, address).await?;
            device.enabled = Some(*enabled);
            let response = client
                .update_device(UpdateDeviceRequest {
                    device: Some(device),
                })
                .await?
                .into_inner();
            ensure_device_cli_success(response.success, response.error)?;
            println!(
                "{} {address}",
                if *enabled {
                    "started/enabled"
                } else {
                    "stopped/disabled"
                }
            );
        }
        DeviceCliCommand::Restart { address } => {
            let mut device = find_managed_device(&mut client, address).await?;
            device.enabled = Some(true);
            let response = client
                .update_device(UpdateDeviceRequest {
                    device: Some(device),
                })
                .await?
                .into_inner();
            ensure_device_cli_success(response.success, response.error)?;
            println!("restart requested for {address}");
        }
    }

    Ok(())
}

async fn find_managed_device(
    client: &mut BonsaiGraphClient<tonic::transport::Channel>,
    address: &str,
) -> Result<ManagedDevice> {
    client
        .list_managed_devices(ListManagedDevicesRequest {})
        .await?
        .into_inner()
        .devices
        .into_iter()
        .find(|device| device.address == address)
        .ok_or_else(|| anyhow::anyhow!("device {address} not found"))
}

fn device_cli_endpoint(api_addr: &str) -> String {
    let trimmed = api_addr.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    }
}

fn ensure_device_cli_success(success: bool, error: String) -> Result<()> {
    if success {
        Ok(())
    } else {
        anyhow::bail!("{}", error.trim())
    }
}

fn managed_device_from_cli_add(add: &DeviceCliAdd) -> ManagedDevice {
    ManagedDevice {
        address: add.address.clone(),
        enabled: Some(add.enabled),
        tls_domain: add.tls_domain.clone().unwrap_or_default(),
        ca_cert: add.ca_cert.clone().unwrap_or_default(),
        vendor: add.vendor.clone().unwrap_or_default(),
        credential_alias: add.credential_alias.clone().unwrap_or_default(),
        username_env: add.username_env.clone().unwrap_or_default(),
        password_env: add.password_env.clone().unwrap_or_default(),
        hostname: add.hostname.clone().unwrap_or_default(),
        role: add.role.clone().unwrap_or_default(),
        site: add.site.clone().unwrap_or_default(),
        selected_paths: Vec::new(),
    }
}

fn managed_device_from_target(target: TargetConfig) -> ManagedDevice {
    ManagedDevice {
        address: target.address,
        enabled: Some(target.enabled),
        tls_domain: target.tls_domain.unwrap_or_default(),
        ca_cert: target.ca_cert.unwrap_or_default(),
        vendor: target.vendor.unwrap_or_default(),
        credential_alias: target.credential_alias.unwrap_or_default(),
        username_env: target.username_env.unwrap_or_default(),
        password_env: target.password_env.unwrap_or_default(),
        hostname: target.hostname.unwrap_or_default(),
        role: target.role.unwrap_or_default(),
        site: target.site.unwrap_or_default(),
        selected_paths: Vec::new(),
    }
}

fn print_managed_devices(devices: Vec<ManagedDevice>) {
    println!(
        "{:<24} {:<8} {:<16} {:<12} {:<12} credential",
        "address", "state", "hostname", "vendor", "site"
    );
    for device in devices {
        println!(
            "{:<24} {:<8} {:<16} {:<12} {:<12} {}",
            device.address,
            if device.enabled.unwrap_or(true) {
                "enabled"
            } else {
                "stopped"
            },
            device.hostname,
            device.vendor,
            device.site,
            device.credential_alias,
        );
    }
}

async fn run_device_cli_local(command: DeviceCliCommand, cfg: config::Config) -> Result<()> {
    let registry = ApiRegistry::open(REGISTRY_PATH, cfg.target.clone())?;

    match command {
        DeviceCliCommand::Help => print_device_cli_usage(),
        DeviceCliCommand::List => {
            let devices = registry.list_active()?;
            print_managed_devices(
                devices
                    .into_iter()
                    .map(managed_device_from_target)
                    .collect(),
            );
        }
        DeviceCliCommand::Add(add) => {
            let device = registry.add_device(TargetConfig {
                address: add.address,
                enabled: add.enabled,
                tls_domain: add.tls_domain,
                ca_cert: add.ca_cert,
                vendor: add.vendor,
                credential_alias: add.credential_alias,
                username_env: add.username_env,
                password_env: add.password_env,
                username: None,
                password: None,
                hostname: add.hostname,
                role: add.role,
                site: add.site,
                selected_paths: Vec::new(),
            })?;
            println!("added {}", device.address);
        }
        DeviceCliCommand::Remove { address } => match registry.remove_device(&address)? {
            Some(_) => println!("removed {address}"),
            None => println!("device {address} not found"),
        },
        DeviceCliCommand::SetEnabled { address, enabled } => {
            let mut device = registry
                .get_device(&address)?
                .ok_or_else(|| anyhow::anyhow!("device {address} not found"))?;
            device.enabled = enabled;
            registry.update_device(device)?;
            println!(
                "{} {address}",
                if enabled {
                    "started/enabled"
                } else {
                    "stopped/disabled"
                }
            );
        }
        DeviceCliCommand::Restart { address } => {
            let mut device = registry
                .get_device(&address)?
                .ok_or_else(|| anyhow::anyhow!("device {address} not found"))?;
            device.enabled = true;
            registry.update_device(device)?;
            println!("restart requested for {address}");
        }
    }
    Ok(())
}

fn config_path() -> String {
    std::env::var("BONSAI_CONFIG")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| CONFIG_PATH.to_string())
}

fn install_rustls_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn parse_address_arg(args: Vec<String>) -> Result<String> {
    let mut iter = args.into_iter();
    let Some(arg) = iter.next() else {
        anyhow::bail!("device command requires an address");
    };
    match arg.as_str() {
        "--address" => require_flag_value("--address", iter.next()),
        other if !other.starts_with("--") => Ok(other.to_string()),
        other => anyhow::bail!("unknown address argument '{other}'"),
    }
}

fn require_flag_value(flag: &str, value: Option<String>) -> Result<String> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
}

fn print_device_cli_usage() {
    println!(
        "usage:\n  bonsai device list\n  bonsai device add --address <host:port> [--hostname name] [--vendor label] [--role role] [--site site] [--credential-alias alias]\n  bonsai device remove <host:port>\n  bonsai device stop <host:port>\n  bonsai device start <host:port>\n  bonsai device restart <host:port>"
    );
}

async fn spawn_subscriber(
    target: TargetConfig,
    credentials: &std::sync::Arc<CredentialVault>,
    bus: &std::sync::Arc<InProcessBus>,
    subscription_plan_tx: Option<&tokio::sync::mpsc::Sender<SubscriptionPlan>>,
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
    let resolved_credentials = resolve_target_credentials(&target, credentials)?;
    let (username, password) = match resolved_credentials {
        Some(credentials) => (Some(credentials.username), Some(credentials.password)),
        None => (None, None),
    };
    let subscriber = subscriber::GnmiSubscriber::new(
        target.address.clone(),
        username,
        password,
        target.vendor.clone(),
        target.hostname.clone(),
        target.tls_domain.clone().unwrap_or_default(),
        ca_cert_pem,
        std::sync::Arc::clone(bus),
        subscription_plan_tx.cloned(),
        target.selected_paths.clone(),
    );
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(async move { subscriber.run_forever(shutdown_rx).await });
    subscribers.insert(address.clone(), (shutdown_tx, handle));
    info!(address = %address, "subscriber started");
    Ok(())
}

async fn restart_subscriber(
    target: TargetConfig,
    credentials: &std::sync::Arc<CredentialVault>,
    bus: &std::sync::Arc<InProcessBus>,
    subscription_plan_tx: Option<&tokio::sync::mpsc::Sender<SubscriptionPlan>>,
    subscribers: &mut SubscriberHandleMap,
) -> Result<()> {
    let address = target.address.clone();
    stop_subscriber(&address, subscribers).await;
    spawn_subscriber(target, credentials, bus, subscription_plan_tx, subscribers).await
}

async fn stop_subscriber(address: &str, subscribers: &mut SubscriberHandleMap) {
    if let Some((shutdown_tx, handle)) = subscribers.remove(address) {
        let _ = shutdown_tx.send(true);
        let _ = handle.await;
        info!(address = %address, "subscriber stopped");
    }
}

async fn stop_all_subscribers(subscribers: &mut SubscriberHandleMap) {
    let addresses: Vec<String> = subscribers.keys().cloned().collect();
    for address in addresses {
        stop_subscriber(&address, subscribers).await;
    }
}

async fn load_ca_cert_pem(target: &TargetConfig) -> Result<Option<Vec<u8>>> {
    match &target.ca_cert {
        Some(path) => {
            let bytes = tokio::fs::read(path)
                .await
                .with_context(|| format!("could not read CA cert from '{path}'"))?;
            Ok(Some(bytes))
        }
        None => Ok(None),
    }
}

fn resolve_target_credentials(
    target: &TargetConfig,
    credentials: &CredentialVault,
) -> Result<Option<ResolvedCredential>> {
    if let Some(alias) = target.credential_alias.as_deref() {
        return credentials.resolve(alias).map(Some);
    }

    Ok(
        match (target.resolved_username(), target.resolved_password()) {
            (Some(username), Some(password)) => Some(ResolvedCredential { username, password }),
            _ => None,
        },
    )
}
