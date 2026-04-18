use std::pin::Pin;
use std::sync::Arc;

use futures::{Stream, StreamExt};
use lbug::{Connection, Value};
use tokio_stream::wrappers::BroadcastStream;
use tonic::{Request, Response, Status};

use crate::gnmi_set::gnmi_set;
use crate::graph::{BonsaiEvent, GraphStore};

pub mod pb {
    #![allow(clippy::all)]
    tonic::include_proto!("bonsai.v1");
}

pub use pb::bonsai_graph_server::{BonsaiGraph, BonsaiGraphServer};
use pb::*;

/// Connection info for a managed target — passed to BonsaiService so PushRemediation
/// can open a gNMI channel without Python ever touching credentials.
#[derive(Clone)]
pub struct TargetConnInfo {
    pub address:     String,
    pub username:    Option<String>,
    pub password:    Option<String>,
    pub ca_cert_pem: Option<Vec<u8>>,
    pub tls_domain:  String,
}

pub struct BonsaiService {
    store:   Arc<GraphStore>,
    targets: Arc<Vec<TargetConnInfo>>,
}

impl BonsaiService {
    pub fn new(store: Arc<GraphStore>, targets: Vec<TargetConnInfo>) -> Self {
        Self { store, targets: Arc::new(targets) }
    }
}

type EventStream = Pin<Box<dyn Stream<Item = Result<StateEvent, Status>> + Send>>;

#[tonic::async_trait]
impl BonsaiGraph for BonsaiService {
    async fn query(&self, req: Request<QueryRequest>) -> Result<Response<QueryResponse>, Status> {
        let cypher = req.into_inner().cypher;
        let db = self.store.db();

        let result = tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).map_err(|e| e.to_string())?;
            let mut rows = conn.query(&cypher).map_err(|e| e.to_string())?;
            let mut out: Vec<Vec<serde_json::Value>> = Vec::new();
            while let Some(row) = rows.next() {
                out.push(row.iter().map(value_to_json).collect());
            }
            serde_json::to_string(&out).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| Status::internal(e.to_string()))?;

        match result {
            Ok(json_rows) => Ok(Response::new(QueryResponse { json_rows, error: String::new() })),
            Err(e)        => Ok(Response::new(QueryResponse { json_rows: String::new(), error: e })),
        }
    }

    async fn get_devices(&self, _req: Request<GetDevicesRequest>) -> Result<Response<GetDevicesResponse>, Status> {
        let db = self.store.db();
        let devices = tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).map_err(|e| e.to_string())?;
            let mut rows = conn
                .query("MATCH (d:Device) RETURN d.address, d.vendor, d.hostname, d.updated_at")
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            while let Some(row) = rows.next() {
                out.push(Device {
                    address:       str_val(&row[0]),
                    vendor:        str_val(&row[1]),
                    hostname:      str_val(&row[2]),
                    updated_at_ns: ts_val(&row[3]),
                });
            }
            Ok::<_, String>(out)
        })
        .await
        .map_err(|e| Status::internal(e.to_string()))?
        .map_err(Status::internal)?;

        Ok(Response::new(GetDevicesResponse { devices }))
    }

    async fn get_interfaces(&self, req: Request<GetInterfacesRequest>) -> Result<Response<GetInterfacesResponse>, Status> {
        let device_address = req.into_inner().device_address;
        let db = self.store.db();

        let interfaces = tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).map_err(|e| e.to_string())?;
            let mut out = Vec::new();

            if device_address.is_empty() {
                let mut rows = conn.query(
                    "MATCH (i:Interface) RETURN i.device_address, i.name, \
                     i.in_pkts, i.out_pkts, i.in_octets, i.out_octets, \
                     i.in_errors, i.out_errors, i.updated_at",
                ).map_err(|e| e.to_string())?;
                while let Some(row) = rows.next() {
                    out.push(interface_from_row(&row));
                }
            } else {
                let mut stmt = conn.prepare(
                    "MATCH (i:Interface {device_address: $addr}) RETURN i.device_address, i.name, \
                     i.in_pkts, i.out_pkts, i.in_octets, i.out_octets, \
                     i.in_errors, i.out_errors, i.updated_at",
                ).map_err(|e| e.to_string())?;
                let mut rows = conn.execute(&mut stmt, vec![
                    ("addr", Value::String(device_address)),
                ]).map_err(|e| e.to_string())?;
                while let Some(row) = rows.next() {
                    out.push(interface_from_row(&row));
                }
            }
            Ok::<_, String>(out)
        })
        .await
        .map_err(|e| Status::internal(e.to_string()))?
        .map_err(Status::internal)?;

        Ok(Response::new(GetInterfacesResponse { interfaces }))
    }

    async fn get_bgp_neighbors(&self, req: Request<GetBgpNeighborsRequest>) -> Result<Response<GetBgpNeighborsResponse>, Status> {
        let device_address = req.into_inner().device_address;
        let db = self.store.db();

        let neighbors = tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).map_err(|e| e.to_string())?;
            let mut out = Vec::new();

            if device_address.is_empty() {
                let mut rows = conn.query(
                    "MATCH (n:BgpNeighbor) RETURN n.device_address, n.peer_address, \
                     n.peer_as, n.session_state, n.updated_at",
                ).map_err(|e| e.to_string())?;
                while let Some(row) = rows.next() {
                    out.push(bgp_neighbor_from_row(&row));
                }
            } else {
                let mut stmt = conn.prepare(
                    "MATCH (n:BgpNeighbor {device_address: $addr}) RETURN n.device_address, n.peer_address, \
                     n.peer_as, n.session_state, n.updated_at",
                ).map_err(|e| e.to_string())?;
                let mut rows = conn.execute(&mut stmt, vec![
                    ("addr", Value::String(device_address)),
                ]).map_err(|e| e.to_string())?;
                while let Some(row) = rows.next() {
                    out.push(bgp_neighbor_from_row(&row));
                }
            }
            Ok::<_, String>(out)
        })
        .await
        .map_err(|e| Status::internal(e.to_string()))?
        .map_err(Status::internal)?;

        Ok(Response::new(GetBgpNeighborsResponse { neighbors }))
    }

    async fn get_topology(&self, _req: Request<GetTopologyRequest>) -> Result<Response<GetTopologyResponse>, Status> {
        let db = self.store.db();
        let edges = tokio::task::spawn_blocking(move || {
            let conn = Connection::new(&db).map_err(|e| e.to_string())?;
            let mut rows = conn.query(
                "MATCH (li:Interface)-[:CONNECTED_TO]->(ri:Interface) \
                 RETURN li.device_address, li.name, ri.device_address, ri.name",
            ).map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            while let Some(row) = rows.next() {
                out.push(TopologyEdge {
                    src_device:    str_val(&row[0]),
                    src_interface: str_val(&row[1]),
                    dst_device:    str_val(&row[2]),
                    dst_interface: str_val(&row[3]),
                });
            }
            Ok::<_, String>(out)
        })
        .await
        .map_err(|e| Status::internal(e.to_string()))?
        .map_err(Status::internal)?;

        Ok(Response::new(GetTopologyResponse { edges }))
    }

    type StreamEventsStream = EventStream;

    async fn stream_events(&self, req: Request<StreamEventsRequest>) -> Result<Response<EventStream>, Status> {
        let inner         = req.into_inner();
        let filter_types  = inner.event_types;
        let filter_device = inner.device_address;
        let rx            = self.store.subscribe_events();

        let stream = BroadcastStream::new(rx).filter_map(move |item| {
            let filter_types  = filter_types.clone();
            let filter_device = filter_device.clone();
            async move {
                let ev: BonsaiEvent = item.ok()?;
                if !filter_device.is_empty() && ev.device_address != filter_device {
                    return None;
                }
                if !filter_types.is_empty() && !filter_types.contains(&ev.event_type) {
                    return None;
                }
                Some(Ok(StateEvent {
                    device_address:        ev.device_address,
                    event_type:            ev.event_type,
                    detail_json:           ev.detail_json,
                    occurred_at_ns:        ev.occurred_at_ns,
                    state_change_event_id: ev.state_change_event_id,
                }))
            }
        });

        Ok(Response::new(Box::pin(stream)))
    }

    async fn create_detection(&self, req: Request<CreateDetectionRequest>) -> Result<Response<CreateDetectionResponse>, Status> {
        let r = req.into_inner();
        match self.store.write_detection(
            r.device_address, r.rule_id, r.severity, r.features_json, r.fired_at_ns,
            r.state_change_event_id,
        ).await {
            Ok(id)  => Ok(Response::new(CreateDetectionResponse { id, error: String::new() })),
            Err(e)  => Ok(Response::new(CreateDetectionResponse { id: String::new(), error: format!("{:#}", e) })),
        }
    }

    async fn create_remediation(&self, req: Request<CreateRemediationRequest>) -> Result<Response<CreateRemediationResponse>, Status> {
        let r = req.into_inner();
        match self.store.write_remediation(
            r.detection_id, r.action, r.status, r.detail_json, r.attempted_at_ns, r.completed_at_ns,
        ).await {
            Ok(id)  => Ok(Response::new(CreateRemediationResponse { id, error: String::new() })),
            Err(e)  => Ok(Response::new(CreateRemediationResponse { id: String::new(), error: format!("{:#}", e) })),
        }
    }

    async fn push_remediation(&self, req: Request<PushRemediationRequest>) -> Result<Response<PushRemediationResponse>, Status> {
        let r = req.into_inner();
        let target = self.targets.iter().find(|t| t.address == r.target_address);
        let Some(t) = target else {
            return Ok(Response::new(PushRemediationResponse {
                success: false,
                error: format!("unknown target '{}'", r.target_address),
            }));
        };
        let result = gnmi_set(
            &t.address,
            t.username.as_deref(),
            t.password.as_deref(),
            t.ca_cert_pem.as_deref(),
            &t.tls_domain,
            &r.yang_path,
            &r.json_value,
        ).await;
        match result {
            Ok(())  => Ok(Response::new(PushRemediationResponse { success: true, error: String::new() })),
            Err(e)  => Ok(Response::new(PushRemediationResponse { success: false, error: e.to_string() })),
        }
    }
}

// ── row-to-proto helpers ──────────────────────────────────────────────────────

fn interface_from_row(row: &[Value]) -> Interface {
    Interface {
        device_address: str_val(&row[0]),
        name:           str_val(&row[1]),
        in_pkts:        i64_val(&row[2]),
        out_pkts:       i64_val(&row[3]),
        in_octets:      i64_val(&row[4]),
        out_octets:     i64_val(&row[5]),
        in_errors:      i64_val(&row[6]),
        out_errors:     i64_val(&row[7]),
        updated_at_ns:  ts_val(&row[8]),
    }
}

fn bgp_neighbor_from_row(row: &[Value]) -> BgpNeighbor {
    BgpNeighbor {
        device_address: str_val(&row[0]),
        peer_address:   str_val(&row[1]),
        peer_as:        i64_val(&row[2]),
        session_state:  str_val(&row[3]),
        updated_at_ns:  ts_val(&row[4]),
    }
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::String(s)       => serde_json::Value::String(s.clone()),
        Value::Int64(n)        => serde_json::json!(*n),
        Value::TimestampNs(dt) => serde_json::json!(dt.unix_timestamp_nanos() as i64),
        _                      => serde_json::Value::Null,
    }
}

fn str_val(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _                => String::new(),
    }
}

fn i64_val(v: &Value) -> i64 {
    match v {
        Value::Int64(n) => *n,
        _               => 0,
    }
}

fn ts_val(v: &Value) -> i64 {
    match v {
        Value::TimestampNs(dt) => dt.unix_timestamp_nanos() as i64,
        _                      => 0,
    }
}
