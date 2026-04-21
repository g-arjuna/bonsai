use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::broadcast;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Request;
use tracing::{info, warn};

use crate::api::pb::{
    TelemetryIngestUpdate as ProtoTelemetryIngestUpdate, bonsai_graph_client::BonsaiGraphClient,
};
use crate::event_bus::InProcessBus;
use crate::telemetry::TelemetryUpdate;

const FORWARDER_RECONNECT_DELAY: Duration = Duration::from_secs(5);
const FORWARDER_CHANNEL_DEPTH: usize = 1024;

pub fn telemetry_to_ingest_update(
    collector_id: &str,
    update: &TelemetryUpdate,
) -> Result<ProtoTelemetryIngestUpdate> {
    Ok(ProtoTelemetryIngestUpdate {
        collector_id: collector_id.to_string(),
        target: update.target.clone(),
        vendor: update.vendor.clone(),
        hostname: update.hostname.clone(),
        timestamp_ns: update.timestamp_ns,
        path: update.path.clone(),
        value_msgpack: rmp_serde::to_vec(&update.value)
            .context("failed to serialize telemetry value as MessagePack")?,
    })
}

pub fn ingest_update_to_telemetry(update: ProtoTelemetryIngestUpdate) -> Result<TelemetryUpdate> {
    let value = rmp_serde::from_slice(&update.value_msgpack)
        .with_context(|| format!("invalid telemetry value_msgpack for path '{}'", update.path))?;

    Ok(TelemetryUpdate {
        target: update.target,
        vendor: update.vendor,
        hostname: update.hostname,
        timestamp_ns: update.timestamp_ns,
        path: update.path,
        value,
    })
}

pub async fn run_core_forwarder(
    bus: Arc<InProcessBus>,
    core_endpoint: String,
    collector_id: String,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }

        match forward_once(
            Arc::clone(&bus),
            &core_endpoint,
            &collector_id,
            shutdown.clone(),
        )
        .await
        {
            Ok(()) => return,
            Err(error) => {
                warn!(
                    %core_endpoint,
                    %collector_id,
                    %error,
                    delay = ?FORWARDER_RECONNECT_DELAY,
                    "collector forwarder disconnected"
                );
            }
        }

        tokio::select! {
            _ = shutdown.changed() => return,
            _ = tokio::time::sleep(FORWARDER_RECONNECT_DELAY) => {}
        }
    }
}

async fn forward_once(
    bus: Arc<InProcessBus>,
    core_endpoint: &str,
    collector_id: &str,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let mut client = BonsaiGraphClient::connect(core_endpoint.to_string())
        .await
        .with_context(|| format!("failed to connect to core ingest endpoint '{core_endpoint}'"))?;

    info!(
        %core_endpoint,
        %collector_id,
        "collector forwarder connected to core"
    );

    let (tx, rx) = tokio::sync::mpsc::channel(FORWARDER_CHANNEL_DEPTH);
    let sender = tokio::spawn(send_bus_updates(
        bus,
        collector_id.to_string(),
        tx,
        shutdown,
    ));

    let response = client
        .telemetry_ingest(Request::new(ReceiverStream::new(rx)))
        .await;

    sender.abort();
    let response = response
        .context("core telemetry ingest stream failed")?
        .into_inner();
    if !response.error.is_empty() {
        anyhow::bail!("core rejected telemetry ingest stream: {}", response.error);
    }

    info!(
        accepted = response.accepted,
        "collector forwarder stream closed"
    );
    Ok(())
}

async fn send_bus_updates(
    bus: Arc<InProcessBus>,
    collector_id: String,
    tx: tokio::sync::mpsc::Sender<ProtoTelemetryIngestUpdate>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let mut rx = bus.subscribe();
    loop {
        let update = tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            received = rx.recv() => received,
        };

        match update {
            Ok(update) => {
                let proto = telemetry_to_ingest_update(&collector_id, &update)?;
                if tx.send(proto).await.is_err() {
                    return Ok(());
                }
            }
            Err(broadcast::error::RecvError::Lagged(dropped)) => {
                warn!(dropped, "collector forwarder lagged on local event bus");
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use prost::Message;
    use serde_json::json;

    use super::{ingest_update_to_telemetry, telemetry_to_ingest_update};
    use crate::telemetry::TelemetryUpdate;

    #[test]
    fn telemetry_ingest_proto_round_trips_msgpack_payload() {
        let update = TelemetryUpdate {
            target: "10.0.0.1:57400".to_string(),
            vendor: "nokia_srl".to_string(),
            hostname: "srl1".to_string(),
            timestamp_ns: 123,
            path: "interface[name=ethernet-1/1]/statistics".to_string(),
            value: json!({"in-packets": "42"}),
        };

        let proto = telemetry_to_ingest_update("collector-a", &update).unwrap();
        assert_eq!(proto.collector_id, "collector-a");
        assert!(!proto.value_msgpack.is_empty());

        let round_trip = ingest_update_to_telemetry(proto).unwrap();
        assert_eq!(round_trip.target, update.target);
        assert_eq!(round_trip.vendor, update.vendor);
        assert_eq!(round_trip.hostname, update.hostname);
        assert_eq!(round_trip.timestamp_ns, update.timestamp_ns);
        assert_eq!(round_trip.path, update.path);
        assert_eq!(round_trip.value, update.value);
    }

    #[test]
    fn telemetry_ingest_msgpack_counter_payload_is_smaller_than_json_baseline() {
        let mut msgpack_wire_bytes = 0;
        let mut json_wire_bytes = 0;
        let mut msgpack_payload_bytes = 0;
        let mut json_payload_bytes = 0;

        for index in 0_u64..1_000 {
            let update = TelemetryUpdate {
                target: "10.0.0.1:57400".to_string(),
                vendor: "nokia_srl".to_string(),
                hostname: "srl1".to_string(),
                timestamp_ns: 1_777_777_000_000_000_000 + index as i64,
                path: "interfaces/interface[name=ethernet-1/1]/state/counters/in-octets"
                    .to_string(),
                value: json!(1_234_567_890_123_456_789_u64 + index),
            };

            let proto = telemetry_to_ingest_update("collector-a", &update).unwrap();
            let json_payload = serde_json::to_vec(&update.value).unwrap();

            msgpack_wire_bytes += proto.encoded_len();
            msgpack_payload_bytes += proto.value_msgpack.len();
            json_payload_bytes += json_payload.len();

            let mut old_json_proto = proto.clone();
            old_json_proto.value_msgpack.clear();
            json_wire_bytes +=
                old_json_proto.encoded_len() + len_delimited_field_size(7, json_payload.len());
        }

        let payload_reduction = 1.0 - (msgpack_payload_bytes as f64 / json_payload_bytes as f64);
        assert!(
            payload_reduction >= 0.30,
            "expected MessagePack counter payloads to be at least 30% smaller; json={json_payload_bytes}, msgpack={msgpack_payload_bytes}, reduction={payload_reduction:.2}"
        );
        assert!(
            msgpack_wire_bytes < json_wire_bytes,
            "expected MessagePack ingest wire bytes to be smaller; json={json_wire_bytes}, msgpack={msgpack_wire_bytes}"
        );
    }

    fn len_delimited_field_size(field_number: u32, payload_len: usize) -> usize {
        varint_size(((field_number << 3) | 2) as u64)
            + varint_size(payload_len as u64)
            + payload_len
    }

    fn varint_size(mut value: u64) -> usize {
        let mut size = 1;
        while value >= 0x80 {
            value >>= 7;
            size += 1;
        }
        size
    }
}
