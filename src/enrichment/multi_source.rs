use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, Channel, ClientTlsConfig};

use crate::config::TargetConfig;
use crate::credentials::ResolvedCredential;
use crate::proto::gnmi::g_nmi_client::GNmiClient;
use crate::proto::gnmi::{Encoding, GetRequest, Path, PathElem, TypedValue};

const GET_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct MultiSourceCapture {
    pub source: String,
    pub path_count: usize,
    pub payload: String,
}

#[async_trait::async_trait]
pub trait MultiSourceEnricher: Send + Sync {
    fn name(&self) -> &str;
    async fn capture(
        &self,
        target: &TargetConfig,
        credentials: Option<&ResolvedCredential>,
    ) -> Result<MultiSourceCapture>;
}

#[derive(Clone, Debug)]
pub struct GnmiGetConfigEnricher {
    pub paths: Vec<String>,
}

#[async_trait::async_trait]
impl MultiSourceEnricher for GnmiGetConfigEnricher {
    fn name(&self) -> &str {
        "gnmi_get_config"
    }

    async fn capture(
        &self,
        target: &TargetConfig,
        credentials: Option<&ResolvedCredential>,
    ) -> Result<MultiSourceCapture> {
        let channel = connect(target).await?;
        let username = credentials.map(|creds| creds.username.clone());
        let password = credentials.map(|creds| creds.password.clone());

        #[allow(clippy::result_large_err)]
        let mut client = GNmiClient::with_interceptor(channel, move |mut req: Request<()>| {
            if let Some(ref username) = username
                && let Ok(value) = MetadataValue::try_from(username.as_str())
            {
                req.metadata_mut().insert("username", value);
            }
            if let Some(ref password) = password
                && let Ok(value) = MetadataValue::try_from(password.as_str())
            {
                req.metadata_mut().insert("password", value);
            }
            Ok(req)
        });

        let request = Request::new(GetRequest {
            path: build_paths(&self.paths)?,
            r#type: crate::proto::gnmi::get_request::DataType::Config as i32,
            encoding: Encoding::JsonIetf as i32,
            ..Default::default()
        });
        let response = tokio::time::timeout(GET_TIMEOUT, client.get(request))
            .await
            .with_context(|| format!("gNMI Get timed out for {}", target.address))?
            .with_context(|| format!("gNMI Get failed for {}", target.address))?
            .into_inner();

        let payload = notifications_to_json(response.notification);
        Ok(MultiSourceCapture {
            source: self.name().to_string(),
            path_count: if self.paths.is_empty() {
                1
            } else {
                self.paths.len()
            },
            payload,
        })
    }
}

async fn connect(target: &TargetConfig) -> Result<Channel> {
    let use_tls = target.ca_cert.is_some();
    let uri = if use_tls {
        format!("https://{}", target.address)
    } else {
        format!("http://{}", target.address)
    };

    let endpoint = Channel::from_shared(uri.clone()).context("invalid gNMI target URI")?;
    if use_tls {
        let ca_path = target
            .ca_cert
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("TLS requested without ca_cert"))?;
        let domain = target
            .tls_domain
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("TLS target is missing tls_domain"))?;
        let pem = tokio::fs::read(ca_path)
            .await
            .with_context(|| format!("could not read CA cert from '{ca_path}'"))?;
        let tls = ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(pem))
            .domain_name(domain.to_string());
        endpoint
            .tls_config(tls)
            .context("invalid TLS config for gNMI Get")?
            .connect()
            .await
            .with_context(|| format!("failed to connect to {uri}"))
    } else {
        endpoint
            .connect()
            .await
            .with_context(|| format!("failed to connect to {uri}"))
    }
}

fn build_paths(raw_paths: &[String]) -> Result<Vec<Path>> {
    if raw_paths.is_empty() {
        return Ok(vec![Path::default()]);
    }
    raw_paths.iter().map(|path| parse_path(path)).collect()
}

fn parse_path(raw_path: &str) -> Result<Path> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return Ok(Path::default());
    }

    let mut elem = Vec::new();
    for segment in trimmed.split('/').filter(|segment| !segment.is_empty()) {
        let first_key = segment.find('[').unwrap_or(segment.len());
        let name = segment[..first_key].trim();
        if name.is_empty() {
            bail!("invalid gNMI path segment '{segment}'");
        }
        let mut key = HashMap::new();
        let mut rest = &segment[first_key..];
        while !rest.is_empty() {
            let after_open = rest
                .strip_prefix('[')
                .ok_or_else(|| anyhow::anyhow!("invalid key syntax in '{segment}'"))?;
            let close = after_open
                .find(']')
                .ok_or_else(|| anyhow::anyhow!("missing closing bracket in '{segment}'"))?;
            let pair = &after_open[..close];
            let (key_name, key_value) = pair
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("invalid key/value in '{segment}'"))?;
            key.insert(key_name.trim().to_string(), key_value.trim().to_string());
            rest = &after_open[close + 1..];
        }
        elem.push(PathElem {
            name: name.to_string(),
            key,
        });
    }
    Ok(Path {
        elem,
        ..Default::default()
    })
}

fn notifications_to_json(notifications: Vec<crate::proto::gnmi::Notification>) -> String {
    let payload: Vec<serde_json::Value> = notifications
        .into_iter()
        .map(|notification| {
            let prefix = notification
                .prefix
                .as_ref()
                .map(path_to_string)
                .unwrap_or_else(|| "/".to_string());
            let updates = notification
                .update
                .iter()
                .map(|update| {
                    let update_path = update
                        .path
                        .as_ref()
                        .map(path_to_string)
                        .unwrap_or_else(|| "/".to_string());
                    serde_json::json!({
                        "path": update_path,
                        "value": update.val.as_ref().map(typed_value_to_json).unwrap_or(serde_json::Value::Null),
                    })
                })
                .collect::<Vec<_>>();
            serde_json::json!({
                "timestamp": notification.timestamp,
                "prefix": prefix,
                "updates": updates,
            })
        })
        .collect();
    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "[]".to_string())
}

fn path_to_string(path: &Path) -> String {
    if path.elem.is_empty() {
        return "/".to_string();
    }
    let parts = path
        .elem
        .iter()
        .map(|elem| {
            let mut segment = elem.name.clone();
            let mut keys = elem.key.iter().collect::<Vec<_>>();
            keys.sort_by(|a, b| a.0.cmp(b.0));
            for (key, value) in keys {
                segment.push_str(&format!("[{key}={value}]"));
            }
            segment
        })
        .collect::<Vec<_>>();
    format!("/{}", parts.join("/"))
}

fn typed_value_to_json(value: &TypedValue) -> serde_json::Value {
    match &value.value {
        Some(crate::proto::gnmi::typed_value::Value::StringVal(v)) => {
            serde_json::Value::String(v.clone())
        }
        Some(crate::proto::gnmi::typed_value::Value::IntVal(v)) => serde_json::json!(v),
        Some(crate::proto::gnmi::typed_value::Value::UintVal(v)) => serde_json::json!(v),
        Some(crate::proto::gnmi::typed_value::Value::BoolVal(v)) => serde_json::json!(v),
        Some(crate::proto::gnmi::typed_value::Value::FloatVal(v)) => serde_json::json!(v),
        Some(crate::proto::gnmi::typed_value::Value::DoubleVal(v)) => serde_json::json!(v),
        Some(crate::proto::gnmi::typed_value::Value::AsciiVal(v)) => {
            serde_json::Value::String(v.clone())
        }
        Some(crate::proto::gnmi::typed_value::Value::JsonVal(v))
        | Some(crate::proto::gnmi::typed_value::Value::JsonIetfVal(v)) => serde_json::from_slice(v)
            .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(v).to_string())),
        Some(crate::proto::gnmi::typed_value::Value::BytesVal(v))
        | Some(crate::proto::gnmi::typed_value::Value::ProtoBytes(v)) => {
            serde_json::Value::String(hex::encode(v))
        }
        Some(crate::proto::gnmi::typed_value::Value::LeaflistVal(v)) => {
            serde_json::Value::Array(v.element.iter().map(typed_value_to_json).collect())
        }
        Some(crate::proto::gnmi::typed_value::Value::AnyVal(v)) => serde_json::json!({
            "type_url": v.type_url,
            "value_b64": hex::encode(&v.value),
        }),
        Some(crate::proto::gnmi::typed_value::Value::DecimalVal(v)) => serde_json::json!({
            "digits": v.digits,
            "precision": v.precision,
        }),
        None => serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_root_path_returns_empty_elems() {
        let path = parse_path("/").expect("root path");
        assert!(path.elem.is_empty());
    }

    #[test]
    fn parse_selected_path_with_keys() {
        let path = parse_path("/interfaces/interface[name=ethernet-1/1]/state").expect("path");
        assert_eq!(path.elem.len(), 3);
        assert_eq!(path.elem[1].name, "interface");
        assert_eq!(
            path.elem[1].key.get("name").map(String::as_str),
            Some("ethernet-1/1")
        );
    }
}
