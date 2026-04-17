use anyhow::{Context, Result};
use tracing::info;

mod subscriber;

pub mod proto {
    pub mod gnmi {
        #![allow(clippy::all)]
        tonic::include_proto!("gnmi");
    }
    pub mod gnmi_ext {
        #![allow(clippy::all)]
        tonic::include_proto!("gnmi_ext");
    }
}

// CA cert written by deploy.sh after each clab deployment.
const CA_CERT_PATH: &str = "lab/fast-iteration/ca.pem";

// ContainerLab assigns fixed mgmt IPs and names containers clab-<topology>-<node>.
const TARGETS: &[(&str, &str)] = &[
    ("172.100.100.11:57400", "clab-bonsai-srl-srl1"),
    ("172.100.100.12:57400", "clab-bonsai-srl-srl2"),
    ("172.100.100.13:57400", "clab-bonsai-srl-srl3"),
];

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("bonsai=debug".parse()?),
        )
        .init();

    info!("bonsai starting — Phase 1: The Heartbeat");

    let ca_cert_pem = tokio::fs::read(CA_CERT_PATH)
        .await
        .with_context(|| format!(
            "could not read CA cert from '{CA_CERT_PATH}' — run deploy.sh first"
        ))?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let mut handles = Vec::new();
    for (address, tls_domain) in TARGETS {
        let sub = subscriber::GnmiSubscriber::new(
            *address,
            "admin",
            "NokiaSrl1!",
            ca_cert_pem.clone(),
            *tls_domain,
        );
        let rx = shutdown_rx.clone();
        handles.push(tokio::spawn(async move { sub.run_forever(rx).await }));
    }

    tokio::signal::ctrl_c().await?;
    info!("Ctrl+C received — shutting down");
    let _ = shutdown_tx.send(true);

    for handle in handles {
        let _ = handle.await;
    }

    info!("bonsai stopped");
    Ok(())
}
