use std::time::Duration;

use anyhow::{Context, Result};

use bonsai::{
    api::pb::{
        AddDeviceRequest, ListManagedDevicesRequest, ManagedDevice, RemoveDeviceRequest,
        UpdateDeviceRequest, bonsai_graph_client::BonsaiGraphClient,
    },
    audit, catalogue, config,
    config::TargetConfig,
    registry::{ApiRegistry, DeviceRegistry},
};

use super::{CONFIG_PATH, REGISTRY_PATH};

// ── Config path helper (shared with main) ────────────────────────────────────

pub(super) fn config_path() -> String {
    std::env::var("BONSAI_CONFIG")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| CONFIG_PATH.to_string())
}

// ── CLI arg helpers ───────────────────────────────────────────────────────────

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

// ── DeviceCliCommand ──────────────────────────────────────────────────────────

pub(super) enum DeviceCliCommand {
    Help,
    List,
    Add(Box<DeviceCliAdd>),
    Remove { address: String },
    SetEnabled { address: String, enabled: bool },
    Restart { address: String },
}

pub(super) enum AuditCliCommand {
    Help,
    Export {
        since: String,
        until: String,
        output: Option<String>,
    },
}

pub(super) enum CatalogueCliCommand {
    Help,
    List,
    Install { url: String, name: Option<String> },
    Uninstall { name: String },
}

pub(super) enum YangCliCommand {
    Help,
    List,
    Search {
        query: String,
    },
    Sync {
        vendor: Option<String>,
    },
    Import {
        directory: String,
        vendor: Option<String>,
        trust: String,
    },
    Trust {
        module_name: String,
        revision: Option<String>,
        trust: String,
    },
    Bundle {
        vendor: String,
        version: Option<String>,
        output: String,
    },
    Install {
        bundle: String,
    },
}

pub(super) struct DeviceCliAdd {
    pub address: String,
    pub hostname: Option<String>,
    pub vendor: Option<String>,
    pub role: Option<String>,
    pub site: Option<String>,
    pub credential_alias: Option<String>,
    pub username_env: Option<String>,
    pub password_env: Option<String>,
    pub tls_domain: Option<String>,
    pub ca_cert: Option<String>,
    pub enabled: bool,
}

impl DeviceCliCommand {
    pub(super) fn parse() -> Result<Option<Self>> {
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

impl AuditCliCommand {
    pub(super) fn parse() -> Result<Option<Self>> {
        let mut args = std::env::args().skip(1).collect::<Vec<_>>();
        if args.first().map(String::as_str) != Some("audit") {
            return Ok(None);
        }
        args.remove(0);
        let Some(action) = args.first().cloned() else {
            return Ok(Some(Self::Help));
        };
        args.remove(0);

        match action.as_str() {
            "export" => {
                let mut since = None;
                let mut until = None;
                let mut output = None;
                let mut iter = args.into_iter();
                while let Some(arg) = iter.next() {
                    match arg.as_str() {
                        "--since" => since = Some(require_flag_value("--since", iter.next())?),
                        "--until" => until = Some(require_flag_value("--until", iter.next())?),
                        "--output" => output = Some(require_flag_value("--output", iter.next())?),
                        "--help" | "-h" => return Ok(Some(Self::Help)),
                        other => anyhow::bail!("unknown audit export argument '{other}'"),
                    }
                }
                let since =
                    since.ok_or_else(|| anyhow::anyhow!("audit export requires --since"))?;
                let until =
                    until.ok_or_else(|| anyhow::anyhow!("audit export requires --until"))?;
                Ok(Some(Self::Export {
                    since,
                    until,
                    output,
                }))
            }
            "help" | "--help" | "-h" => Ok(Some(Self::Help)),
            other => anyhow::bail!("unknown audit command '{other}'"),
        }
    }
}

impl CatalogueCliCommand {
    pub(super) fn parse() -> Result<Option<Self>> {
        let mut args = std::env::args().skip(1).collect::<Vec<_>>();
        if args.first().map(String::as_str) != Some("catalogue") {
            return Ok(None);
        }
        args.remove(0);
        let Some(action) = args.first().cloned() else {
            return Ok(Some(Self::Help));
        };
        args.remove(0);

        match action.as_str() {
            "list" => Ok(Some(Self::List)),
            "install" => {
                let mut url = None;
                let mut name = None;
                let mut iter = args.into_iter();
                while let Some(arg) = iter.next() {
                    match arg.as_str() {
                        "--name" => name = Some(require_flag_value("--name", iter.next())?),
                        "--help" | "-h" => return Ok(Some(Self::Help)),
                        other if url.is_none() && !other.starts_with("--") => {
                            url = Some(other.to_string());
                        }
                        other => anyhow::bail!("unknown catalogue install argument '{other}'"),
                    }
                }
                let url = url.ok_or_else(|| anyhow::anyhow!("catalogue install requires a URL"))?;
                Ok(Some(Self::Install { url, name }))
            }
            "uninstall" | "remove" => {
                let name = args
                    .into_iter()
                    .find(|a| !a.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("catalogue uninstall requires a plugin name"))?;
                Ok(Some(Self::Uninstall { name }))
            }
            "help" | "--help" | "-h" => Ok(Some(Self::Help)),
            other => anyhow::bail!("unknown catalogue command '{other}'"),
        }
    }
}

impl YangCliCommand {
    pub(super) fn parse() -> Result<Option<Self>> {
        let mut args = std::env::args().skip(1).collect::<Vec<_>>();
        if args.first().map(String::as_str) != Some("yang") {
            return Ok(None);
        }
        args.remove(0);
        let Some(action) = args.first().cloned() else {
            return Ok(Some(Self::Help));
        };
        args.remove(0);

        match action.as_str() {
            "list" => Ok(Some(Self::List)),
            "search" => {
                let query = args
                    .into_iter()
                    .find(|arg| !arg.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("yang search requires a query"))?;
                Ok(Some(Self::Search { query }))
            }
            "sync" => {
                let mut vendor = None;
                let mut iter = args.into_iter();
                while let Some(arg) = iter.next() {
                    match arg.as_str() {
                        "--vendor" => vendor = Some(require_flag_value("--vendor", iter.next())?),
                        "--help" | "-h" => return Ok(Some(Self::Help)),
                        other => anyhow::bail!("unknown yang sync argument '{other}'"),
                    }
                }
                Ok(Some(Self::Sync { vendor }))
            }
            "import" => {
                let mut directory = None;
                let mut vendor = None;
                let mut trust = "trusted".to_string();
                let mut iter = args.into_iter();
                while let Some(arg) = iter.next() {
                    match arg.as_str() {
                        "--vendor" => vendor = Some(require_flag_value("--vendor", iter.next())?),
                        "--trust" => trust = require_flag_value("--trust", iter.next())?,
                        "--help" | "-h" => return Ok(Some(Self::Help)),
                        other if directory.is_none() && !other.starts_with("--") => {
                            directory = Some(other.to_string());
                        }
                        other => anyhow::bail!("unknown yang import argument '{other}'"),
                    }
                }
                Ok(Some(Self::Import {
                    directory: directory
                        .ok_or_else(|| anyhow::anyhow!("yang import requires a directory"))?,
                    vendor,
                    trust,
                }))
            }
            "trust" => {
                let mut module_name = None;
                let mut revision = None;
                let mut trust = None;
                let mut iter = args.into_iter();
                while let Some(arg) = iter.next() {
                    match arg.as_str() {
                        "--revision" => {
                            revision = Some(require_flag_value("--revision", iter.next())?)
                        }
                        "--trust" => trust = Some(require_flag_value("--trust", iter.next())?),
                        "--help" | "-h" => return Ok(Some(Self::Help)),
                        other if module_name.is_none() && !other.starts_with("--") => {
                            module_name = Some(other.to_string());
                        }
                        other => anyhow::bail!("unknown yang trust argument '{other}'"),
                    }
                }
                Ok(Some(Self::Trust {
                    module_name: module_name
                        .ok_or_else(|| anyhow::anyhow!("yang trust requires a module name"))?,
                    revision,
                    trust: trust.ok_or_else(|| anyhow::anyhow!("yang trust requires --trust"))?,
                }))
            }
            "bundle" => {
                let mut vendor = None;
                let mut version = None;
                let mut output = "runtime/yang_bundle.tar".to_string();
                let mut iter = args.into_iter();
                while let Some(arg) = iter.next() {
                    match arg.as_str() {
                        "--version" => {
                            version = Some(require_flag_value("--version", iter.next())?)
                        }
                        "--output" => output = require_flag_value("--output", iter.next())?,
                        "--help" | "-h" => return Ok(Some(Self::Help)),
                        other if vendor.is_none() && !other.starts_with("--") => {
                            vendor = Some(other.to_string());
                        }
                        other => anyhow::bail!("unknown yang bundle argument '{other}'"),
                    }
                }
                Ok(Some(Self::Bundle {
                    vendor: vendor
                        .ok_or_else(|| anyhow::anyhow!("yang bundle requires a vendor"))?,
                    version,
                    output,
                }))
            }
            "install" => {
                let bundle = args
                    .into_iter()
                    .find(|arg| !arg.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("yang install requires a bundle path"))?;
                Ok(Some(Self::Install { bundle }))
            }
            "help" | "--help" | "-h" => Ok(Some(Self::Help)),
            other => anyhow::bail!("unknown yang command '{other}'"),
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

// ── run_* entry points (called from main) ────────────────────────────────────

pub(super) async fn run_device_cli(command: DeviceCliCommand) -> Result<()> {
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
        collector_id: String::new(),
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
        collector_id: target.collector_id.unwrap_or_default(),
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
            let device = registry.add_device_with_audit(
                TargetConfig {
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
                    collector_id: None,
                    selected_paths: Vec::new(),
                    created_at_ns: 0,
                    updated_at_ns: 0,
                    created_by: String::new(),
                    updated_by: String::new(),
                    last_operator_action: String::new(),
                },
                "cli",
                "cli_add_device",
            )?;
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
            registry.update_device_with_audit(device, "cli", "cli_set_enabled_device")?;
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
            registry.update_device_with_audit(device, "cli", "cli_restart_device")?;
            println!("restart requested for {address}");
        }
    }
    Ok(())
}

fn print_device_cli_usage() {
    println!(
        "usage:\n  bonsai device list\n  bonsai device add --address <host:port> [--hostname name] [--vendor label] [--role role] [--site site] [--credential-alias alias]\n  bonsai device remove <host:port>\n  bonsai device stop <host:port>\n  bonsai device start <host:port>\n  bonsai device restart <host:port>"
    );
}

pub(super) async fn run_audit_cli(command: AuditCliCommand) -> Result<()> {
    if matches!(command, AuditCliCommand::Help) {
        print_audit_cli_usage();
        return Ok(());
    }

    let config_path = config_path();
    let cfg = config::load(&config_path).await?;
    let root = std::path::Path::new(&cfg.credentials.path);

    match command {
        AuditCliCommand::Help => {
            print_audit_cli_usage();
            Ok(())
        }
        AuditCliCommand::Export {
            since,
            until,
            output,
        } => {
            let output_path = output
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("bonsai-audit-export.tar"));
            let result = audit::export_tarball(root, &since, &until, &output_path)?;
            println!(
                "exported {} audit events to {}",
                result.entry_count,
                result.output_path.display()
            );
            Ok(())
        }
    }
}

fn print_audit_cli_usage() {
    println!(
        "usage:\n  bonsai audit export --since <RFC3339> --until <RFC3339> [--output path.tar]"
    );
}

fn catalogue_plugins_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("config/path_profiles/plugins")
}

pub(super) async fn run_catalogue_cli(command: CatalogueCliCommand) -> Result<()> {
    match command {
        CatalogueCliCommand::Help => {
            print_catalogue_cli_usage();
            Ok(())
        }
        CatalogueCliCommand::List => {
            let catalogue_dir = "config/path_profiles";
            let state = catalogue::load_catalogue(std::path::Path::new(catalogue_dir));

            if !state.load_errors.is_empty() {
                for e in &state.load_errors {
                    eprintln!("warning: {e}");
                }
            }

            println!("Built-in profiles ({}):", state.profiles.len());
            for p in &state.profiles {
                let env = if p.environment.is_empty() {
                    "all".to_string()
                } else {
                    p.environment.join(", ")
                };
                let roles = if p.roles.is_empty() {
                    "all".to_string()
                } else {
                    p.roles.join(", ")
                };
                println!("  {:<30} env={:<20} roles={}", p.name, env, roles);
            }

            if state.plugins.is_empty() {
                println!("\nNo plugins installed.");
            } else {
                println!("\nInstalled plugins ({}):", state.plugins.len());
                for plugin in &state.plugins {
                    let m = &plugin.manifest;
                    println!("  {:<24} v{:<10} by {}", m.name, m.version, m.author);
                    for p in &plugin.profiles {
                        println!("    profile: {}", p.name);
                    }
                    for conflict in &plugin.conflicts {
                        println!("    conflict: {conflict}");
                    }
                }
            }
            Ok(())
        }
        CatalogueCliCommand::Install { url, name } => {
            let plugin_name = name.unwrap_or_else(|| {
                url.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("plugin")
                    .trim_end_matches(".git")
                    .to_string()
            });

            if plugin_name.is_empty() || plugin_name.contains(['/', '\\', '.']) {
                anyhow::bail!("plugin name '{plugin_name}' is invalid. Use --name to override.");
            }

            let plugins_dir = catalogue_plugins_dir();
            let dest = plugins_dir.join(&plugin_name);

            if dest.exists() {
                anyhow::bail!(
                    "plugin directory '{}' already exists. Uninstall first with: bonsai catalogue uninstall {plugin_name}",
                    dest.display()
                );
            }

            std::fs::create_dir_all(&plugins_dir).with_context(|| {
                format!("cannot create plugins dir '{}'", plugins_dir.display())
            })?;

            println!("cloning {url} → {}", dest.display());
            let status = std::process::Command::new("git")
                .args([
                    "clone",
                    "--depth=1",
                    "--quiet",
                    &url,
                    &dest.to_string_lossy(),
                ])
                .status()
                .with_context(|| "git not found — install git and retry")?;

            if !status.success() {
                anyhow::bail!("git clone failed (exit code {:?})", status.code());
            }

            let manifest_path = dest.join("MANIFEST.yaml");
            if !manifest_path.exists() {
                let _ = std::fs::remove_dir_all(&dest);
                anyhow::bail!(
                    "no MANIFEST.yaml found in the cloned repository. \
                     Bonsai plugins must have a MANIFEST.yaml at the repo root."
                );
            }

            let manifest_bytes =
                std::fs::read(&manifest_path).with_context(|| "cannot read MANIFEST.yaml")?;
            let manifest: catalogue::PluginManifest = serde_yaml::from_slice(&manifest_bytes)
                .with_context(|| "MANIFEST.yaml is not valid YAML or missing required fields")?;

            use sha2::Digest;
            let mut hasher = sha2::Sha256::new();
            hasher.update(&manifest_bytes);
            let fingerprint = hex::encode(hasher.finalize());

            println!("installed plugin: {}", manifest.name);
            println!("  version : {}", manifest.version);
            if !manifest.author.is_empty() {
                println!("  author  : {}", manifest.author);
            }
            println!("  profiles: {}", manifest.profiles.len());
            for p in &manifest.profiles {
                println!("    {p}");
            }
            println!("  manifest SHA256: {fingerprint}");
            println!(
                "\nPlugin will be active on next bonsai start (or server reload).\n\
                 To list installed plugins: bonsai catalogue list"
            );
            Ok(())
        }
        CatalogueCliCommand::Uninstall { name } => {
            let plugins_dir = catalogue_plugins_dir();
            let target = plugins_dir.join(&name);
            if !target.exists() {
                anyhow::bail!("plugin '{name}' not found in {}", plugins_dir.display());
            }
            if !target.join("MANIFEST.yaml").exists() {
                anyhow::bail!(
                    "'{name}' does not look like a bonsai plugin (no MANIFEST.yaml). \
                     Remove manually if intended."
                );
            }
            std::fs::remove_dir_all(&target)
                .with_context(|| format!("cannot remove '{}'", target.display()))?;
            println!("uninstalled plugin: {name}");
            Ok(())
        }
    }
}

pub(super) async fn run_yang_cli(command: YangCliCommand) -> Result<()> {
    if matches!(command, YangCliCommand::Help) {
        print_yang_cli_usage();
        return Ok(());
    }

    let config_path = config_path();
    let cfg = config::load(&config_path).await?;
    let library = bonsai::yang::YangLibrary::open(
        &cfg.yang.library_root,
        &cfg.yang.cache_root,
        &cfg.yang.bundle_key_env,
    )?;
    let catalogue_dir = std::path::Path::new("config/path_profiles");

    match command {
        YangCliCommand::Help => print_yang_cli_usage(),
        YangCliCommand::List => {
            let modules = library.list_modules()?;
            if modules.is_empty() {
                println!("No YANG modules imported yet.");
            } else {
                println!(
                    "{:<28} {:<14} {:<12} {:<14} trust",
                    "module", "revision", "vendor", "source"
                );
                for module in modules {
                    println!(
                        "{:<28} {:<14} {:<12} {:<14} {}",
                        module.module_name,
                        module.revision,
                        module.vendor_scope,
                        module.source_kind,
                        module.trust,
                    );
                }
            }
        }
        YangCliCommand::Search { query } => {
            let result = library.search(&query)?;
            println!("Query: {}", result.query);
            if result.modules.is_empty() {
                println!("No matching modules.");
            } else {
                println!("\nModules:");
                for module in result.modules {
                    println!(
                        "  {}@{} [{}] {}",
                        module.module_name, module.revision, module.vendor_scope, module.source_ref
                    );
                }
            }
            if result.paths.is_empty() {
                println!("\nNo matching Bonsai path mappings.");
            } else {
                println!("\nBonsai path mappings:");
                for path in result.paths {
                    println!(
                        "  {} -> {} ({})",
                        path.module_name, path.path, path.profile_name
                    );
                }
            }
        }
        YangCliCommand::Sync { vendor } => {
            let report = library.sync(vendor.as_deref(), catalogue_dir)?;
            println!(
                "synced {} source(s): imported={}, updated={}, skipped={}",
                report.sources.len(),
                report.imported,
                report.updated,
                report.skipped
            );
            for source in report.sources {
                println!("  {source}");
            }
        }
        YangCliCommand::Import {
            directory,
            vendor,
            trust,
        } => {
            let report = library.import_directory(
                std::path::Path::new(&directory),
                &bonsai::yang::YangImportOptions {
                    source_kind: "manual".to_string(),
                    source_ref: directory.clone(),
                    vendor_scope: vendor.unwrap_or_else(|| "manual".to_string()),
                    trust,
                },
                catalogue_dir,
            )?;
            println!(
                "imported={}, updated={}, skipped={}",
                report.imported, report.updated, report.skipped
            );
            for module in report.modules {
                println!("  {}@{}", module.module_name, module.revision);
            }
        }
        YangCliCommand::Trust {
            module_name,
            revision,
            trust,
        } => {
            let updated = library.set_module_trust(&module_name, revision.as_deref(), &trust)?;
            println!(
                "updated {}@{} trust={}",
                updated.module_name, updated.revision, updated.trust
            );
        }
        YangCliCommand::Bundle {
            vendor,
            version,
            output,
        } => {
            let output_path = std::path::Path::new(&output);
            if let Some(parent) = output_path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)?;
            }
            let manifest = library.create_bundle(&vendor, version.as_deref(), output_path)?;
            println!(
                "created YANG bundle '{}' with {} module(s); key env={}",
                output,
                manifest.modules.len(),
                library.bundle_key_env()
            );
        }
        YangCliCommand::Install { bundle } => {
            let report = library.install_bundle(std::path::Path::new(&bundle), catalogue_dir)?;
            println!(
                "installed bundle '{}': imported={}, updated={}, skipped={}",
                bundle, report.imported, report.updated, report.skipped
            );
        }
    }

    Ok(())
}

fn print_catalogue_cli_usage() {
    println!(
        "usage:\n\
         \x20 bonsai catalogue list\n\
         \x20 bonsai catalogue install <git-url> [--name <plugin-name>]\n\
         \x20 bonsai catalogue uninstall <plugin-name>\n\
         \n\
         Plugins are cloned into config/path_profiles/plugins/<name>/.\n\
         Each plugin must have a MANIFEST.yaml at its root.\n\
         \n\
         Example:\n\
         \x20 bonsai catalogue install https://github.com/example/bonsai-plugin-nokia-sr.git\n\
         \x20 bonsai catalogue list\n\
         \x20 bonsai catalogue uninstall bonsai-plugin-nokia-sr"
    );
}

fn print_yang_cli_usage() {
    println!(
        "usage:\n\
         \x20 bonsai yang list\n\
         \x20 bonsai yang search <query>\n\
         \x20 bonsai yang sync [--vendor <openconfig|cisco|juniper|arista|nokia>]\n\
         \x20 bonsai yang import <directory> [--vendor <name>] [--trust <trusted|experimental>]\n\
         \x20 bonsai yang trust <module-name> [--revision <rev>] --trust <trusted|experimental>\n\
         \x20 bonsai yang bundle <vendor> [--version <filter>] [--output <bundle.tar>]\n\
         \x20 bonsai yang install <bundle.tar>\n\
         \n\
         The local library lives under runtime/yang_catalogue by default.\n\
         Signed bundle create/install commands require the env var configured by [yang].bundle_key_env."
    );
}

// ── SelfTest ──────────────────────────────────────────────────────────────────

pub(super) struct SelfTestCliCommand;

impl SelfTestCliCommand {
    pub(super) fn parse() -> bool {
        std::env::args().nth(1).as_deref() == Some("self-test")
    }
}

pub(super) async fn run_self_test() -> Result<()> {
    let mut passed: u32 = 0;
    let mut failed: u32 = 0;

    macro_rules! check {
        ($label:expr, $body:block) => {{
            let result: Result<()> = async {
                $body;
                Ok(())
            }
            .await;
            match result {
                Ok(()) => {
                    println!("  [✓] {}", $label);
                    passed += 1;
                }
                Err(e) => {
                    println!("  [✗] {} — {e}", $label);
                    failed += 1;
                }
            }
        }};
    }

    println!("bonsai self-test");
    println!("================");

    check!("crypto provider (rustls/ring)", {
        rustls::crypto::ring::default_provider()
            .install_default()
            .or_else(|_| {
                Ok::<(), rustls::Error>(())
            })?;
    });

    check!("tokio runtime", {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    });

    check!("config parser (TOML round-trip)", {
        let _cfg: bonsai::config::Config = toml::from_str(
            "graph_path = \"/tmp/bonsai-selftest-unused.db\"\n\
             [runtime]\nmode = \"all\"\n\
             [event_bus]\ncapacity = 512\n",
        )?;
    });

    check!("LadybugDB linkage (open temp database)", {
        let db_path = std::env::temp_dir().join(format!("bonsai-self-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&db_path);
        let path = db_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF8 temp path"))?
            .to_owned();
        let result = bonsai::graph::GraphStore::open(&path, 64 * 1024 * 1024);
        let _ = std::fs::remove_dir_all(&db_path);
        result.map(|_| ())?;
    });

    println!("================");
    println!("{passed} passed, {failed} failed");

    if failed > 0 {
        anyhow::bail!("{failed} check(s) failed");
    }
    Ok(())
}

