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

// TLS domain name must match the CN/SAN in the SR Linux node cert.
// ContainerLab names containers: clab-<topology>-<node>.
const SRL1_TLS_DOMAIN: &str = "clab-bonsai-srl-srl1";

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

    let sub = subscriber::GnmiSubscriber::new(
        "172.100.100.11:57400",
        "admin",
        "NokiaSrl1!",
        ca_cert_pem,
        SRL1_TLS_DOMAIN,
    );

    sub.subscribe_interfaces().await?;

    Ok(())
}
