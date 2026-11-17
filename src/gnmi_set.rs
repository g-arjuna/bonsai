use anyhow::{Context, Result};
use std::time::Duration;
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;
use tracing::warn;

use crate::gnmi_security::ConnSecurity;
use crate::proto::gnmi::g_nmi_client::GNmiClient;
use crate::proto::gnmi::{Path, PathElem, SetRequest, TypedValue, Update, typed_value};

/// Execute a gNMI Set (UPDATE) against a single target.
/// Credentials are injected as gRPC metadata headers — they never leave the Rust process.
/// Transport security comes from the resolved `ConnSecurity` contract:
/// explicit TLS/mTLS or the audited plaintext lab exception — never an
/// implicit downgrade.
pub async fn gnmi_set(
    address: &str,
    username: Option<&str>,
    password: Option<&str>,
    conn_security: &ConnSecurity,
    yang_path: &str,
    json_value: &str,
) -> Result<()> {
    if !conn_security.effective.is_tls() && (username.is_some() || password.is_some()) {
        warn!(
            %address,
            security = conn_security.effective.label(),
            "SECURITY: gNMI Set is sending credentials over a plaintext (non-TLS) channel. \
             Configure security=\"tls_verify\" or \"mtls\" with ca_cert/tls_domain."
        );
    }
    let channel = open_channel(address, conn_security).await?;
    let user = username.map(str::to_owned);
    let pass = password.map(str::to_owned);

    #[allow(clippy::result_large_err)]
    let mut client = GNmiClient::with_interceptor(channel, move |mut req: Request<()>| {
        if let Some(ref u) = user
            && let Ok(v) = MetadataValue::try_from(u.as_str())
        {
            req.metadata_mut().insert("username", v);
        }
        if let Some(ref p) = pass
            && let Ok(v) = MetadataValue::try_from(p.as_str())
        {
            req.metadata_mut().insert("password", v);
        }
        Ok(req)
    });

    let path = parse_path(yang_path);
    let value = TypedValue {
        value: Some(typed_value::Value::JsonIetfVal(
            json_value.as_bytes().to_vec(),
        )),
    };

    let req = SetRequest {
        update: vec![Update {
            path: Some(path),
            val: Some(value),
            ..Default::default()
        }],
        ..Default::default()
    };

    client
        .set(req)
        .await
        .map_err(|s| anyhow::anyhow!("gNMI Set failed: {} — {}", s.code(), s.message()))?;
    Ok(())
}

/// Parse a YANG path string into a gNMI Path.
/// Handles segments like `interface[name=ethernet-1/1]` and plain `admin-state`.
pub fn parse_path(s: &str) -> Path {
    let elems = s
        .split('/')
        .filter(|seg| !seg.is_empty())
        .map(|seg| {
            if let Some(bracket) = seg.find('[') {
                let name = seg[..bracket].to_string();
                let rest = &seg[bracket + 1..seg.len() - 1]; // strip [ and ]
                let mut key = std::collections::HashMap::new();
                for kv in rest.split(',') {
                    if let Some(eq) = kv.find('=') {
                        key.insert(kv[..eq].to_string(), kv[eq + 1..].to_string());
                    }
                }
                PathElem { name, key }
            } else {
                PathElem {
                    name: seg.to_string(),
                    key: Default::default(),
                }
            }
        })
        .collect();
    Path {
        elem: elems,
        ..Default::default()
    }
}

async fn open_channel(address: &str, conn_security: &ConnSecurity) -> Result<Channel> {
    let endpoint = format!("{}://{}", conn_security.scheme(), address);
    let mut builder = Channel::from_shared(endpoint.clone())
        .context("invalid endpoint")?
        .timeout(Duration::from_secs(15));

    if let Some(tls) = conn_security.client_tls_config()? {
        builder = builder.tls_config(tls).context("TLS config failed")?;
    }

    let channel = builder
        .connect()
        .await
        .with_context(|| format!("connect failed: {endpoint}"))?;

    if !conn_security.effective.is_tls() {
        metrics::counter!(
            "bonsai_gnmi_plaintext_dial_total",
            "mode" => conn_security.effective.label()
        )
        .increment(1);
    }
    Ok(channel)
}
