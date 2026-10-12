use std::sync::Arc;
use std::time::Duration;
use anyhow::Result;
use tokio::sync::broadcast;
use tracing::{error, warn};
use metrics::counter;
use rbmp_rib::event::{RibEvent, RibEventPayload, RouteAction};
use rbmp_core::bgp::evpn::EvpnRoute;
use crate::duck::RouteStore;

const BATCH_SIZE:    usize        = 500;
const BATCH_TIMEOUT: Duration     = Duration::from_millis(50);

/// Drives a RibEvent stream into DuckDB using batched inserts.
/// Collects up to BATCH_SIZE events or flushes after BATCH_TIMEOUT — whichever first.
/// Target throughput: ≥ 1500 msg/sec.
pub async fn run_store_writer(store: Arc<std::sync::Mutex<RouteStore>>, mut rx: broadcast::Receiver<RibEvent>) {
    let mut batch: Vec<RibEvent> = Vec::with_capacity(BATCH_SIZE);

    loop {
        // Try to fill the batch up to BATCH_SIZE within BATCH_TIMEOUT
        let deadline = tokio::time::sleep(BATCH_TIMEOUT);
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                biased;
                result = rx.recv() => {
                    match result {
                        Ok(ev) => {
                            batch.push(ev);
                            if batch.len() >= BATCH_SIZE { break; }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!(%n, "Store writer lagged; {n} events dropped");
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            // Flush remaining before exit
                            if !batch.is_empty() {
                                match flush_batch(&store, &batch) {
                                    Ok(written) => {
                                        counter!("rib_events_written_total").increment(written as u64);
                                    }
                                    Err(e) => {
                                        error!(?e, "Failed to flush final RibEvent batch before exit");
                                        counter!("rib_events_write_errors_total").increment(batch.len() as u64);
                                    }
                                }
                                batch.clear();
                            }
                            return;
                        }
                    }
                }
                _ = &mut deadline => { break; }
            }
        }

        if !batch.is_empty() {
            let n = batch.len();
            match flush_batch(&store, &batch) {
                Ok(written) => {
                    counter!("rib_events_written_total").increment(written as u64);
                }
                Err(e) => {
                    error!(?e, "Failed to flush RibEvent batch to DuckDB");
                    counter!("rib_events_write_errors_total").increment(n as u64);
                }
            }
            batch.clear();
        }
    }
}

/// Flush a batch of events atomically inside a single DuckDB transaction.
/// If any statement or commit fails, the entire transaction is rolled back and an `Err` is returned.
pub fn flush_batch(store: &std::sync::Mutex<RouteStore>, batch: &[RibEvent]) -> Result<usize> {
    let locked = store
        .lock()
        .map_err(|e| anyhow::anyhow!("DuckDB lock poisoned: {e}"))?;
    let conn = locked.conn();
    conn.execute_batch("BEGIN")
        .map_err(|e| anyhow::anyhow!("DuckDB BEGIN failed: {e}"))?;
    for ev in batch {
        if let Err(e) = persist_one(conn, ev) {
            error!(?e, "Failed to persist RibEvent in batch — rolling back transaction");
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    if let Err(e) = conn.execute_batch("COMMIT") {
        error!("DuckDB COMMIT failed: {e}");
        let _ = conn.execute_batch("ROLLBACK");
        return Err(e.into());
    }
    Ok(batch.len())
}

fn persist_one(conn: &duckdb::Connection, ev: &RibEvent) -> Result<()> {
    let id  = ev.id.to_string();
    let ts  = ev.occurred_at.to_rfc3339();
    let spk = ev.speaker.to_string();

    match &ev.payload {
        RibEventPayload::RouteChange(rc) => {
            let action = match rc.action {
                RouteAction::Announce => "announce",
                RouteAction::Withdraw => "withdraw",
            };
            let attrs = &rc.attributes;
            let as_path_str = attrs.as_ref().and_then(|a| a.as_path.as_ref())
                .map(|p| p.to_string());
            let as_path_len = attrs.as_ref().and_then(|a| a.as_path.as_ref())
                .map(|p| p.hop_count() as u16);
            let next_hop   = attrs.as_ref().and_then(|a| a.next_hop).map(|h| h.to_string());
            let local_pref = attrs.as_ref().and_then(|a| a.local_pref);
            let med        = attrs.as_ref().and_then(|a| a.multi_exit_disc);
            let origin     = attrs.as_ref().and_then(|a| a.origin).map(|o| format!("{}", o));
            let communities = attrs.as_ref().map(|a| {
                a.communities.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(",")
            });
            let ext_communities = attrs.as_ref().map(|a| {
                a.extended_communities.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(",")
            });
            let large_communities = attrs.as_ref().map(|a| {
                a.large_communities.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(",")
            });
            let afi = format!("{}", rc.prefix.addr_family().as_u16());

            let otc = attrs.as_ref().and_then(|a| a.only_to_customer);

            conn.execute(
                "INSERT INTO route_events VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    id, ts, spk,
                    rc.peer_header.peer_address.to_string(),
                    rc.peer_header.peer_as,
                    format!("{:?}", rc.rib_type),
                    action,
                    rc.prefix.to_string(),
                    afi,
                    origin,
                    as_path_str,
                    as_path_len,
                    next_hop,
                    local_pref,
                    med,
                    communities,
                    ext_communities,
                    large_communities,
                    attrs.as_ref().and_then(|a| a.originator_id).map(|o| o.to_string()),
                    attrs.as_ref().map(|a| a.atomic_aggregate).unwrap_or(false),
                    otc,
                    duckdb::types::Null,  // collector_id — populated by collector protocol layer
                ],
            )?;

            // RV7-P3: Write path_markings row when Path Status TLV is present
            if let Some(ps) = attrs.as_ref().and_then(|a| a.path_status) {
                conn.execute(
                    "INSERT INTO path_markings VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    duckdb::params![
                        ts,
                        spk,
                        rc.peer_header.peer_address.to_string(),
                        rc.peer_header.peer_as,
                        rc.prefix.to_string(),
                        afi,
                        ps.status,
                        ps.reason,
                        ps.label(),
                        ps.reason_label(),
                        duckdb::types::Null,  // collector_id
                    ],
                )?;
            }

            // Write per-route rows to evpn_events for EVPN announces
            if action == "announce" {
                if let Some(evpn) = attrs.as_ref().and_then(|a| a.evpn_reach.as_ref()) {
                    for route in &evpn.routes {
                        let (rd_s, etag, mac_s, ip_s, pfxlen, label, esi_s) = evpn_route_fields(route);
                        conn.execute(
                            "INSERT INTO evpn_events VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                            duckdb::params![
                                id, ts, spk,
                                rc.peer_header.peer_address.to_string(),
                                rc.peer_header.peer_as,
                                "announce",
                                route.route_type_code(),
                                route.route_type_name(),
                                rd_s, etag, mac_s, ip_s, pfxlen, label, esi_s,
                            ],
                        )?;
                    }
                }
            }
        }
        RibEventPayload::PeerUp { peer_header, local_asn, remote_asn: _, hold_time, capabilities } => {
            conn.execute(
                "INSERT INTO peer_events (
                    id, occurred_at, speaker_addr, peer_addr, peer_as,
                    event_type, local_as, hold_time, capabilities, reason, collector_id
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    id, ts, spk,
                    peer_header.peer_address.to_string(),
                    peer_header.peer_as,
                    "peer_up",
                    local_asn,
                    hold_time,
                    serde_json::to_string(capabilities).unwrap_or_default(),
                    duckdb::types::Null,
                    duckdb::types::Null,
                ],
            )?;
        }
        RibEventPayload::PeerDown { peer_header, reason } => {
            conn.execute(
                "INSERT INTO peer_events (
                    id, occurred_at, speaker_addr, peer_addr, peer_as,
                    event_type, local_as, hold_time, capabilities, reason, collector_id
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    id, ts, spk,
                    peer_header.peer_address.to_string(),
                    peer_header.peer_as,
                    "peer_down",
                    duckdb::types::Null,
                    duckdb::types::Null,
                    duckdb::types::Null,
                    reason,
                    duckdb::types::Null,
                ],
            )?;
        }
        RibEventPayload::SpeakerUp { sys_name, sys_descr } => {
            conn.execute(
                "INSERT INTO speaker_events (
                    id, occurred_at, speaker_addr, event_type,
                    sys_name, sys_descr, reason, collector_id
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    id, ts, spk, "speaker_up",
                    sys_name, sys_descr, duckdb::types::Null, duckdb::types::Null
                ],
            )?;
        }
        RibEventPayload::SpeakerDown { reason } => {
            conn.execute(
                "INSERT INTO speaker_events (
                    id, occurred_at, speaker_addr, event_type,
                    sys_name, sys_descr, reason, collector_id
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    id, ts, spk, "speaker_down",
                    duckdb::types::Null, duckdb::types::Null, reason, duckdb::types::Null
                ],
            )?;
        }
        RibEventPayload::Stats { peer_header, counters } => {
            for stat in counters {
                let afi  = stat.afi_safi.map(|a| a.afi.as_u16());
                let safi = stat.afi_safi.map(|a| a.safi.as_u8());
                conn.execute(
                    "INSERT INTO stats_events VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    duckdb::params![
                        id, ts, spk,
                        peer_header.peer_address.to_string(),
                        stat.name,
                        stat.value,
                        stat.stat_type,
                        afi,
                        safi,
                    ],
                )?;
            }
        }
        RibEventPayload::EndOfRib { peer_header, afi_safi } => {
            conn.execute(
                "INSERT INTO peer_events (
                    id, occurred_at, speaker_addr, peer_addr, peer_as,
                    event_type, local_as, hold_time, capabilities, reason, collector_id
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                duckdb::params![
                    id, ts, spk,
                    peer_header.peer_address.to_string(),
                    peer_header.peer_as,
                    "end_of_rib",
                    duckdb::types::Null,
                    duckdb::types::Null,
                    duckdb::types::Null,
                    afi_safi,
                    duckdb::types::Null,
                ],
            )?;
        }
    }
    Ok(())
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect::<Vec<_>>().join("")
}

/// Decompose an EvpnRoute into flat columns for evpn_events.
/// Returns (rd, ethernet_tag, mac, ip, prefix_len, mpls_label, esi_hex).
#[allow(clippy::type_complexity)]
fn evpn_route_fields(route: &EvpnRoute) -> (
    Option<String>, Option<u32>, Option<String>, Option<String>,
    Option<u8>, Option<u32>, Option<String>,
) {
    match route {
        EvpnRoute::EthernetAutoDiscovery { rd, esi, ethernet_tag, mpls_label } => (
            Some(hex_encode(rd)), Some(*ethernet_tag),
            None, None, None, Some(*mpls_label), Some(hex_encode(esi)),
        ),
        EvpnRoute::MacIpAdvertisement { rd, esi, ethernet_tag, mac, ip, mpls_label1, .. } => (
            Some(hex_encode(rd)), Some(*ethernet_tag),
            Some(mac.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(":")),
            ip.as_ref().map(|a| a.to_string()),
            None, Some(*mpls_label1), Some(hex_encode(esi)),
        ),
        EvpnRoute::InclusiveMulticastEthernetTag { rd, ethernet_tag, originating_router_ip } => (
            Some(hex_encode(rd)), Some(*ethernet_tag),
            None, Some(originating_router_ip.to_string()), None, None, None,
        ),
        EvpnRoute::EthernetSegment { rd, esi, originating_router_ip } => (
            Some(hex_encode(rd)), None,
            None, Some(originating_router_ip.to_string()), None, None, Some(hex_encode(esi)),
        ),
        EvpnRoute::IpPrefix { rd, esi, ethernet_tag, prefix, prefix_len, mpls_label, .. } => (
            Some(hex_encode(rd)), Some(*ethernet_tag),
            None, Some(prefix.to_string()), Some(*prefix_len), Some(*mpls_label), Some(hex_encode(esi)),
        ),
        EvpnRoute::SelectiveMulticastEthernetTag { rd, ethernet_tag, multicast_source, originating_router_ip, .. }
        | EvpnRoute::IgmpJoinSynch { rd, ethernet_tag, multicast_source, originating_router_ip, .. }
        | EvpnRoute::IgmpLeaveSynch { rd, ethernet_tag, multicast_source, originating_router_ip, .. }
        | EvpnRoute::SPmsi { rd, ethernet_tag, multicast_source, originating_router_ip, .. } => (
            Some(hex_encode(rd)), Some(*ethernet_tag),
            Some(multicast_source.to_string()),
            Some(originating_router_ip.to_string()),
            None, None, None,
        ),
        EvpnRoute::PerRegionIPmsi { rd, ethernet_tag, originating_router_ip } => (
            Some(hex_encode(rd)), Some(*ethernet_tag),
            None, Some(originating_router_ip.to_string()), None, None, None,
        ),
        EvpnRoute::LeafAD { path_id, .. } => (
            None, None, None, None, None, Some(*path_id), None,
        ),
        EvpnRoute::Unknown { .. } => (None, None, None, None, None, None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::BmpJournal;
    use chrono::Utc;
    use rbmp_core::bgp::types::{AfiSafi, AsPath, AsPathSegment, BgpUpdate, Origin, PathAttributes, Prefix};
    use rbmp_core::bmp::types::{
        BgpOpenInfo, BmpMessage, BmpPayload, EventKey, ObservationSource, PeerFlags, PeerHeader,
        PeerKey, PeerType, PeerUpMessage, RibType, StatEntry, EVENT_SCHEMA_VERSION,
    };
    use rbmp_rib::RibManager;
    use std::io::Write;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Mutex;
    use uuid::Uuid;

    fn sample_peer_header() -> PeerHeader {
        PeerHeader {
            peer_type: PeerType::GlobalInstance,
            peer_flags: PeerFlags(0),
            peer_distinguisher: [0u8; 8],
            peer_address: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)),
            peer_as: 65002,
            peer_bgp_id: Ipv4Addr::new(10, 0, 0, 2),
            timestamp: Utc::now(),
            rib_type: RibType::AdjRibInPrePolicy,
        }
    }

    fn make_event(seq: u64, payload: RibEventPayload) -> RibEvent {
        let speaker = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
        let source = ObservationSource::new(speaker, 1);
        RibEvent {
            schema_version: EVENT_SCHEMA_VERSION,
            id: Uuid::new_v4(),
            event_key: EventKey::new(source, seq),
            occurred_at: Utc::now(),
            speaker,
            payload,
        }
    }

    #[test]
    fn b06_end_of_rib_event_is_persisted_in_peer_events() {
        let store = Mutex::new(RouteStore::in_memory().unwrap());
        let eor = make_event(
            1,
            RibEventPayload::EndOfRib {
                peer_header: sample_peer_header(),
                afi_safi: "ipv6-unicast".to_string(),
            },
        );
        let written = flush_batch(&store, &[eor]).expect("flush_batch must succeed");
        assert_eq!(written, 1);

        let guard = store.lock().unwrap();
        let count: i64 = guard
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM peer_events WHERE event_type = 'end_of_rib' AND reason = 'ipv6-unicast'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "EndOfRib must be persisted to peer_events");
    }

    #[test]
    fn b06_failed_batch_rolls_back_instead_of_committing_partial_rows() {
        let store = Mutex::new(RouteStore::in_memory().unwrap());
        // Simulate mid-batch failure on stats_events while speaker_events is intact
        store
            .lock()
            .unwrap()
            .conn()
            .execute_batch("DROP TABLE stats_events;")
            .unwrap();

        let ev1 = make_event(
            1,
            RibEventPayload::SpeakerUp {
                sys_name: Some("edge-1".to_string()),
                sys_descr: Some("FRR".to_string()),
            },
        );
        let ev2 = make_event(
            2,
            RibEventPayload::Stats {
                peer_header: sample_peer_header(),
                counters: vec![StatEntry {
                    stat_type: 0,
                    name: "prefixes_rejected".to_string(),
                    value: 5,
                    afi_safi: None,
                }],
            },
        );

        assert!(
            flush_batch(&store, &[ev1, ev2]).is_err(),
            "flush_batch must return Err on statement failure"
        );

        let guard = store.lock().unwrap();
        let count: i64 = guard
            .conn()
            .query_row("SELECT COUNT(*) FROM speaker_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 0,
            "failed batch must roll back all statements in transaction"
        );
    }

    #[tokio::test]
    async fn b06_durable_journal_replay_reconstructs_identical_rib_state_after_truncated_crash_tail() {
        let tmp_path = std::env::temp_dir().join(format!("rbmp-b06-journal-{}.jsonl", Uuid::new_v4()));
        let journal = BmpJournal::open(Some(&tmp_path)).await.unwrap();

        let speaker = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
        let hdr = sample_peer_header();
        let peer_key = PeerKey::from_header(speaker, &hdr);
        let now = Utc::now();

        let mut attrs = PathAttributes::default();
        attrs.origin = Some(Origin::Igp);
        attrs.as_path = Some(AsPath(vec![AsPathSegment::Sequence(vec![65002, 65100])]));
        attrs.next_hop = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)));

        let pfx1 = Prefix::V4("10.10.1.0/24".parse().unwrap());
        let pfx2 = Prefix::V4("10.10.2.0/24".parse().unwrap());

        let messages = vec![
            BmpMessage {
                id: Uuid::new_v4(),
                received_at: now,
                speaker_addr: speaker,
                payload: BmpPayload::Initiation {
                    sys_name: Some("router-1".to_string()),
                    sys_descr: Some("FRR 9.1".to_string()),
                    labels: vec![],
                },
            },
            BmpMessage {
                id: Uuid::new_v4(),
                received_at: now,
                speaker_addr: speaker,
                payload: BmpPayload::PeerUp(PeerUpMessage {
                    peer_header: hdr.clone(),
                    local_addr: speaker,
                    local_port: 179,
                    remote_port: 50123,
                    sent_open: BgpOpenInfo {
                        version: 4,
                        asn: 65001,
                        hold_time: 90,
                        bgp_id: Ipv4Addr::new(192, 0, 2, 10),
                        capabilities: vec![],
                    },
                    recv_open: BgpOpenInfo {
                        version: 4,
                        asn: 65002,
                        hold_time: 90,
                        bgp_id: Ipv4Addr::new(10, 0, 0, 2),
                        capabilities: vec![],
                    },
                }),
            },
            BmpMessage {
                id: Uuid::new_v4(),
                received_at: now,
                speaker_addr: speaker,
                payload: BmpPayload::RouteMonitoring {
                    peer_header: hdr.clone(),
                    update: BgpUpdate {
                        withdrawn: vec![],
                        withdrawn_path_ids: vec![],
                        attributes: attrs.clone(),
                        announced: vec![pfx1.clone(), pfx2.clone()],
                        announced_path_ids: vec![None, None],
                    },
                },
            },
            BmpMessage {
                id: Uuid::new_v4(),
                received_at: now,
                speaker_addr: speaker,
                payload: BmpPayload::RouteMonitoring {
                    peer_header: hdr.clone(),
                    update: BgpUpdate {
                        withdrawn: vec![pfx1.clone()],
                        withdrawn_path_ids: vec![None],
                        attributes: PathAttributes::default(),
                        announced: vec![],
                        announced_path_ids: vec![],
                    },
                },
            },
            BmpMessage {
                id: Uuid::new_v4(),
                received_at: now,
                speaker_addr: speaker,
                payload: BmpPayload::RouteMonitoring {
                    peer_header: hdr.clone(),
                    update: BgpUpdate {
                        withdrawn: vec![],
                        withdrawn_path_ids: vec![],
                        attributes: PathAttributes::default(),
                        announced: vec![],
                        announced_path_ids: vec![],
                    },
                },
            },
        ];

        let (mut rib_live, _rx_live) = RibManager::new(64);
        rib_live.open_speaker_session(speaker, now);
        for msg in &messages {
            journal.append(msg).await.unwrap();
            rib_live.process(msg.clone());
        }
        drop(journal);

        // Append a truncated trailing line without newline to simulate power-loss mid-write
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&tmp_path).unwrap();
            f.write_all(b"{\"id\":\"truncated-crash-record").unwrap();
            f.flush().unwrap();
        }

        let (mut rib_replayed, _rx_replayed) = RibManager::new(64);
        let report = BmpJournal::replay_into(&tmp_path, &mut rib_replayed).unwrap();
        let _ = std::fs::remove_file(&tmp_path);

        assert_eq!(report.applied_messages, 5);
        assert_eq!(report.skipped_truncated_tail, 1);
        assert_eq!(rib_replayed.total_routes(), rib_live.total_routes());
        assert_eq!(rib_replayed.total_routes(), 1);

        let replayed_rib = rib_replayed.rib_for_peer_key(&peer_key).expect("peer RIB present");
        assert!(replayed_rib.get(RibType::AdjRibInPrePolicy, &pfx1).is_none());
        assert!(replayed_rib.get(RibType::AdjRibInPrePolicy, &pfx2).is_some());

        let replayed_sess = rib_replayed.speaker(speaker).unwrap().peer_by_key(&peer_key).unwrap();
        assert!(replayed_sess.has_eor(RibType::AdjRibInPrePolicy, AfiSafi::ipv4_unicast()));
    }
}


