use anyhow::{Context, Result};
use tracing::warn;

use bonsai::{
    credentials::{CredentialVault, ResolvePurpose, ResolvedCredential},
    config::TargetConfig,
    event_bus::InProcessBus,
    ingest,
    subscriber::{self, SubscriberHandleMap},
    subscription_status::SubscriptionPlan,
};

// ── Subscriber lifecycle ──────────────────────────────────────────────────────

pub(super) async fn spawn_subscriber(
    target: TargetConfig,
    credentials: &std::sync::Arc<CredentialVault>,
    bus: &std::sync::Arc<InProcessBus>,
    debouncer: Option<std::sync::Arc<ingest::TelemetryDebouncer>>,
    subscription_plan_tx: Option<&tokio::sync::mpsc::Sender<SubscriptionPlan>>,
    subscribers: &mut SubscriberHandleMap,
) -> Result<()> {
    use bonsai::subscriber::stop_subscriber;
    let _ = stop_subscriber; // keep import live — used by restart_subscriber
    let address = target.address.clone();
    if !target.enabled {
        tracing::info!(address = %address, "subscriber start skipped because target is disabled");
        return Ok(());
    }
    if subscribers.contains_key(&address) {
        tracing::info!(address = %address, "subscriber already running");
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
        target.role.clone(),
        target.site.clone(),
        target.tls_domain.clone().unwrap_or_default(),
        ca_cert_pem,
        std::sync::Arc::clone(bus),
        debouncer,
        subscription_plan_tx.cloned(),
        target.selected_paths.clone(),
    );
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(async move { subscriber.run_forever(shutdown_rx).await });
    subscribers.insert(address.clone(), (shutdown_tx, handle));
    tracing::info!(address = %address, "subscriber started");
    Ok(())
}

pub(super) async fn restart_subscriber(
    target: TargetConfig,
    credentials: &std::sync::Arc<CredentialVault>,
    bus: &std::sync::Arc<InProcessBus>,
    debouncer: Option<std::sync::Arc<ingest::TelemetryDebouncer>>,
    subscription_plan_tx: Option<&tokio::sync::mpsc::Sender<SubscriptionPlan>>,
    subscribers: &mut SubscriberHandleMap,
) -> Result<()> {
    let address = target.address.clone();
    bonsai::subscriber::stop_subscriber(&address, subscribers).await;
    spawn_subscriber(
        target,
        credentials,
        bus,
        debouncer,
        subscription_plan_tx,
        subscribers,
    )
    .await
}

pub(super) async fn load_ca_cert_pem(target: &TargetConfig) -> Result<Option<Vec<u8>>> {
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

pub(super) async fn seed_subscription_plan(
    target: TargetConfig,
    tx: &tokio::sync::mpsc::Sender<SubscriptionPlan>,
) {
    if !target.enabled {
        return;
    }
    let plan = subscriber::planned_subscription_plan_for_target(&target);
    if plan.paths.is_empty() {
        return;
    }
    if let Err(error) = tx.send(plan).await {
        warn!(%error, address = %target.address, "failed to seed subscription verifier plan");
    }
}

pub(super) fn resolve_target_credentials(
    target: &TargetConfig,
    credentials: &CredentialVault,
) -> Result<Option<ResolvedCredential>> {
    if let Some(alias) = target.credential_alias.as_deref() {
        return credentials
            .resolve(alias, ResolvePurpose::Subscribe)
            .map(Some);
    }

    Ok(
        match (target.resolved_username(), target.resolved_password()) {
            (Some(username), Some(password)) => Some(ResolvedCredential { username, password }),
            _ => None,
        },
    )
}

// ── Pre-flight disk space check ───────────────────────────────────────────────

pub(super) fn preflight_disk_check(log_dir: &std::path::Path, min_free_bytes: u64) -> Result<()> {
    let dir = if log_dir.exists() {
        log_dir.to_path_buf()
    } else {
        std::path::PathBuf::from(".")
    };

    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        let path_str = dir.to_str().unwrap_or(".");
        let c_path = CString::new(path_str).unwrap_or_default();
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if ret == 0 {
            let free_bytes = stat.f_bsize as u64 * stat.f_bavail as u64;
            if free_bytes < min_free_bytes {
                anyhow::bail!(
                    "insufficient disk space at '{}': {:.1} GiB free, {:.1} GiB required. \
                     Adjust [logging] min_free_bytes or free disk space before starting bonsai.",
                    dir.display(),
                    free_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
                    min_free_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
                );
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (dir, min_free_bytes);
    }

    Ok(())
}

// ── Log volume tracing Layer ──────────────────────────────────────────────────

/// Tracing Layer that increments a Prometheus counter for every log event.
pub(super) struct LogVolumeLayer;

impl<S> tracing_subscriber::Layer<S> for LogVolumeLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = event.metadata().level().as_str();
        metrics::counter!("bonsai_log_lines_total", "level" => level).increment(1);
    }
}
