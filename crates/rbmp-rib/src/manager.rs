use crate::event::{RibEvent, RibEventPayload, RibEventPayload::*, RouteAction, RouteChange};
use crate::filter::{FilterEngine, FilterVerdict};
use crate::roto_ctx::RouteCtx as RotoCtx;
use crate::roto_filter::RotoFilterEngine;
use crate::session::{BmpSession, PeerSession};
use crate::table::{RibEntry, RibTable};
use chrono::{DateTime, Utc};
use metrics::counter;
use rbmp_core::bgp::types::BgpCapability;
use rbmp_core::bmp::types::{
    BmpMessage, BmpPayload, CanonicalNlri, EVENT_SCHEMA_VERSION, EventKey, ObservationSource,
    PeerKey, RouteKey, ViewKey,
};
use std::collections::HashMap;
use std::net::IpAddr;
use tokio::sync::broadcast;
use tracing::{debug, info};

/// Central RIB manager: owns all per-speaker sessions and per-(speaker, peer_type, distinguisher, peer_addr) route tables.
/// Driven by BmpMessage from the receiver; emits versioned RibEvents to subscribers.
pub struct RibManager {
    speakers: HashMap<IpAddr, BmpSession>,
    /// Typed `PeerKey` → RIB table (isolates multiple exporters and multiple VRF distinguishers).
    ribs: HashMap<PeerKey, RibTable>,
    next_session_epoch: u64,
    next_event_seq: u64,
    event_tx: broadcast::Sender<RibEvent>,
    /// Optional programmable route filter chain (YAML DSL)
    filter: Option<FilterEngine>,
    /// RV7-F T3: Optional Roto JIT filter engine (takes priority over YAML DSL)
    roto_filter: Option<RotoFilterEngine>,
}

impl RibManager {
    pub fn new(event_capacity: usize) -> (Self, broadcast::Receiver<RibEvent>) {
        let (tx, rx) = broadcast::channel(event_capacity);
        (
            Self {
                speakers: HashMap::new(),
                ribs: HashMap::new(),
                next_session_epoch: 1,
                next_event_seq: 1,
                event_tx: tx,
                filter: None,
                roto_filter: None,
            },
            rx,
        )
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RibEvent> {
        self.event_tx.subscribe()
    }

    /// Replace the active filter engine (called on config reload).
    pub fn set_filter(&mut self, engine: FilterEngine) {
        self.filter = Some(engine);
    }

    /// Remove the filter engine (all routes are default-accepted).
    pub fn clear_filter(&mut self) {
        self.filter = None;
    }

    /// RV7-F T3: Replace the active Roto JIT filter engine.
    /// When set, Roto takes priority over the YAML DSL engine.
    pub fn set_roto_filter(&mut self, engine: RotoFilterEngine) {
        self.roto_filter = Some(engine);
    }

    /// Allocate a new session epoch when a BMP speaker establishes a TCP connection.
    /// Older epochs closing later will be fenced and cannot clear this session's state.
    pub fn open_speaker_session(&mut self, speaker: IpAddr, at: DateTime<Utc>) -> u64 {
        let epoch = self.next_session_epoch;
        self.next_session_epoch += 1;
        let source = ObservationSource::new(speaker, epoch);
        match self.speakers.get_mut(&speaker) {
            Some(sess) => {
                sess.source = source;
                sess.connected_at = at;
                for peer in sess.peers.values_mut() {
                    peer.eor_families.clear();
                }
            }
            None => {
                self.speakers
                    .insert(speaker, BmpSession::with_source(source, at));
            }
        }
        epoch
    }

    /// Ensure the next emitted `RibEvent` sequence is at least `min_next_seq`.
    pub fn ensure_min_event_seq(&mut self, min_next_seq: u64) {
        if min_next_seq > self.next_event_seq {
            self.next_event_seq = min_next_seq;
        }
    }

    /// Clear End-of-RIB markers for all peers of `speaker` (used on BMP transport disconnect
    /// so that a subsequent reconnect requires fresh synchronization before reporting complete).
    pub fn clear_speaker_eor(&mut self, speaker: IpAddr) {
        if let Some(sess) = self.speakers.get_mut(&speaker) {
            for peer in sess.peers.values_mut() {
                peer.eor_families.clear();
            }
        }
    }

    /// Expire LLGR-stale routes for a specific `PeerKey` whose stale timer has elapsed.
    pub fn expire_stale_for_peer_key(&mut self, peer_key: &PeerKey, stale_secs: u32) -> usize {
        let now = Utc::now();
        let removed = self
            .ribs
            .get_mut(peer_key)
            .map(|rib| rib.drain_deleted_stale(now, stale_secs))
            .unwrap_or(0);
        self.sync_peer_route_counts(peer_key);
        removed
    }

    /// Expire LLGR-stale routes across all `PeerKey`s matching `peer_addr`.
    pub fn expire_stale_for_peer(&mut self, peer_addr: IpAddr, stale_secs: u32) -> usize {
        let keys: Vec<PeerKey> = self
            .ribs
            .keys()
            .filter(|k| k.peer_address == peer_addr)
            .copied()
            .collect();
        keys.iter()
            .map(|k| self.expire_stale_for_peer_key(k, stale_secs))
            .sum()
    }

    fn ensure_speaker_source(&mut self, speaker: IpAddr, now: DateTime<Utc>) -> ObservationSource {
        if let Some(sess) = self.speakers.get(&speaker) {
            return sess.source.clone();
        }
        let epoch = self.next_session_epoch;
        self.next_session_epoch += 1;
        let source = ObservationSource::new(speaker, epoch);
        self.speakers
            .insert(speaker, BmpSession::with_source(source.clone(), now));
        source
    }

    fn sync_peer_route_counts(&mut self, peer_key: &PeerKey) {
        let counts = self
            .ribs
            .get(peer_key)
            .map(|r| r.rib_counts())
            .unwrap_or_default();
        if let Some(sess) = self.speakers.get_mut(&peer_key.speaker_addr) {
            if let Some(peer) = sess.peers.get_mut(peer_key) {
                peer.route_counts = counts;
            }
        }
    }

    /// Feed a parsed BmpMessage into the RIB with an explicit connection epoch for disconnect fencing.
    /// Returns all emitted `RibEvent`s directly so durable history never depends on a bounded broadcast channel.
    pub fn process_for_epoch(
        &mut self,
        msg: BmpMessage,
        session_epoch: Option<u64>,
    ) -> Vec<RibEvent> {
        let msg_id = msg.id;
        let speaker = msg.speaker_addr;
        let now = msg.received_at;
        let mut emitted = Vec::new();

        if matches!(msg.payload, BmpPayload::Termination { .. }) {
            if let (Some(incoming_epoch), Some(active_sess)) =
                (session_epoch, self.speakers.get(&speaker))
            {
                if incoming_epoch < active_sess.source.session_epoch {
                    debug!(
                        %speaker,
                        incoming_epoch,
                        active_epoch = active_sess.source.session_epoch,
                        "Ignoring stale BMP Termination from older session epoch"
                    );
                    return emitted;
                }
            }
        }

        let source = match session_epoch {
            Some(epoch) => {
                if epoch >= self.next_session_epoch {
                    self.next_session_epoch = epoch + 1;
                }
                let sess = self.speakers.entry(speaker).or_insert_with(|| {
                    BmpSession::with_source(ObservationSource::new(speaker, epoch), now)
                });
                if epoch > sess.source.session_epoch {
                    sess.source.session_epoch = epoch;
                    sess.connected_at = now;
                    for peer in sess.peers.values_mut() {
                        peer.eor_families.clear();
                    }
                }
                sess.source.clone()
            }
            None => self.ensure_speaker_source(speaker, now),
        };

        match msg.payload {
            BmpPayload::Initiation {
                sys_name,
                sys_descr,
                ..
            } => {
                if let Some(session) = self.speakers.get_mut(&speaker) {
                    session.sys_name = sys_name.clone();
                    session.sys_descr = sys_descr.clone();
                }
                info!(%speaker, "BMP speaker connected");
                self.emit_with_source(
                    msg_id,
                    speaker,
                    source,
                    now,
                    SpeakerUp {
                        sys_name,
                        sys_descr,
                    },
                    &mut emitted,
                );
            }
            BmpPayload::Termination {
                reason_code,
                reason_text,
            } => {
                if let Some(sess) = self.speakers.remove(&speaker) {
                    // Clear only this speaker's PeerKey RIB tables; never touch other speakers
                    for peer_key in sess.peers.keys() {
                        self.ribs.remove(peer_key);
                    }
                    info!(%speaker, %reason_code, "BMP speaker disconnected");
                }
                // Also remove any RIB tables created directly by RouteMonitoring before PeerUp for this speaker
                self.ribs.retain(|k, _| k.speaker_addr != speaker);
                let reason = reason_text.unwrap_or_else(|| format!("code={reason_code}"));
                self.emit_with_source(
                    msg_id,
                    speaker,
                    source,
                    now,
                    SpeakerDown { reason },
                    &mut emitted,
                );
            }
            BmpPayload::PeerUp(pu) => {
                let peer_addr = pu.peer_header.peer_address;
                let peer_key = PeerKey::from_header(speaker, &pu.peer_header);
                let mut caps: Vec<BgpCapability> = pu.recv_open.capabilities.clone();
                for sc in &pu.sent_open.capabilities {
                    if !caps.contains(sc) {
                        caps.push(sc.clone());
                    }
                }
                let cap_names: Vec<String> =
                    caps.iter().map(|c| format!("{:?}", c.code())).collect();

                if let Some(speaker_sess) = self.speakers.get_mut(&speaker) {
                    let peer = speaker_sess
                        .peers
                        .entry(peer_key)
                        .or_insert_with(|| PeerSession::new_for_speaker(speaker, &pu.peer_header));
                    peer.on_up_with_header(
                        &pu.peer_header,
                        pu.sent_open.asn,
                        pu.recv_open.hold_time,
                        caps,
                        now,
                    );
                }
                self.sync_peer_route_counts(&peer_key);

                info!(%speaker, %peer_addr, peer_as = pu.peer_header.peer_as, "BGP peer up");
                self.emit_with_source(
                    msg_id,
                    speaker,
                    source,
                    now,
                    PeerUp {
                        peer_header: pu.peer_header,
                        local_asn: pu.sent_open.asn,
                        remote_asn: pu.recv_open.asn,
                        hold_time: pu.recv_open.hold_time,
                        capabilities: cap_names,
                    },
                    &mut emitted,
                );
            }
            BmpPayload::PeerDown {
                peer_header,
                reason,
            } => {
                let peer_addr = peer_header.peer_address;
                let peer_key = PeerKey::from_header(speaker, &peer_header);
                let reason_str = format!("{:?}", reason);

                let llgr_stale_secs = self
                    .speakers
                    .get(&speaker)
                    .and_then(|s| s.peers.get(&peer_key))
                    .filter(|p| p.llgr_active)
                    .map(|p| p.llgr_stale_secs);

                if let Some(sess) = self.speakers.get_mut(&speaker) {
                    if let Some(peer) = sess.peers.get_mut(&peer_key) {
                        peer.on_down(now);
                    }
                }

                if let Some(stale_secs) = llgr_stale_secs {
                    if let Some(rib) = self.ribs.get_mut(&peer_key) {
                        rib.mark_stale_all(now);
                    }
                    self.sync_peer_route_counts(&peer_key);
                    info!(%speaker, %peer_addr, %reason_str, stale_secs, "BGP peer down — LLGR stale marking");
                } else {
                    if let Some(rib) = self.ribs.get_mut(&peer_key) {
                        rib.clear_all();
                    }
                    self.sync_peer_route_counts(&peer_key);
                    info!(%speaker, %peer_addr, %reason_str, "BGP peer down");
                }
                self.emit_with_source(
                    msg_id,
                    speaker,
                    source,
                    now,
                    PeerDown {
                        peer_header,
                        reason: reason_str,
                    },
                    &mut emitted,
                );
            }
            BmpPayload::RouteMonitoring {
                peer_header,
                update,
            } => {
                let peer_addr = peer_header.peer_address;
                let rib_type = peer_header.rib_type;
                let peer_key = PeerKey::from_header(speaker, &peer_header);

                // Ensure peer session exists so speaker.peers tracks all active PeerKeys
                if let Some(speaker_sess) = self.speakers.get_mut(&speaker) {
                    speaker_sess
                        .peers
                        .entry(peer_key)
                        .or_insert_with(|| PeerSession::new_for_speaker(speaker, &peer_header));
                }

                // Check for End-of-RIB (RFC 4724 §2)
                if let Some(eor_family) = update.eor_afi_safi() {
                    if let Some(speaker_sess) = self.speakers.get_mut(&speaker) {
                        if let Some(peer) = speaker_sess.peers.get_mut(&peer_key) {
                            peer.mark_eor(rib_type, eor_family);
                        }
                    }
                    let afi_safi = eor_family.to_string();
                    debug!(%speaker, %peer_addr, %afi_safi, "End-of-RIB received");
                    self.emit_with_source(
                        msg_id,
                        speaker,
                        source,
                        now,
                        EndOfRib {
                            peer_header,
                            afi_safi,
                        },
                        &mut emitted,
                    );
                    return emitted;
                }

                // Process withdrawals — carry path_id for Add-Path compound key
                for (prefix, path_id) in update.all_withdrawn_with_path_id() {
                    self.ribs
                        .entry(peer_key)
                        .or_default()
                        .remove_with_path_id(rib_type, prefix, path_id);
                    counter!("bgp_routes_withdrawn_total", "speaker" => speaker.to_string())
                        .increment(1);
                    let view_key = ViewKey::from_peer(
                        peer_key,
                        prefix.afi_safi(),
                        rib_type,
                        peer_header.peer_flags,
                    );
                    let route_key = RouteKey {
                        view: view_key,
                        nlri: CanonicalNlri::from_prefix(prefix),
                        path_id,
                    };
                    let ev = RouteChange {
                        action: RouteAction::Withdraw,
                        peer_header: peer_header.clone(),
                        peer_key,
                        view_key,
                        route_key,
                        rib_type,
                        prefix: prefix.clone(),
                        path_id,
                        attributes: None,
                    };
                    self.emit_with_source(
                        msg_id,
                        speaker,
                        source.clone(),
                        now,
                        RibEventPayload::RouteChange(ev),
                        &mut emitted,
                    );
                }

                // Process announcements — carry path_id from NLRI (RFC 7911)
                for (prefix, path_id) in update.all_announced_with_path_id() {
                    let filter_verdict = if let Some(roto) = &self.roto_filter {
                        let ctx = RotoCtx::from_bmp(
                            prefix,
                            peer_header.peer_as,
                            peer_addr,
                            rib_type,
                            "announce",
                            &update.attributes,
                            "unknown",
                        );
                        roto.evaluate(&ctx)
                    } else if let Some(engine) = &self.filter {
                        let (v, _) = engine.apply(
                            &prefix,
                            peer_header.peer_as,
                            peer_addr,
                            &update.attributes,
                        );
                        v
                    } else {
                        FilterVerdict::Default
                    };
                    if filter_verdict == FilterVerdict::Deny {
                        debug!(%speaker, %peer_addr, %prefix, "route denied by filter");
                        counter!("bgp_routes_filtered_total", "speaker" => speaker.to_string())
                            .increment(1);
                        continue;
                    }

                    counter!("bgp_routes_announced_total", "speaker" => speaker.to_string())
                        .increment(1);
                    let entry = RibEntry {
                        prefix: prefix.clone(),
                        path_id,
                        attributes: update.attributes.clone(),
                        received_at: now,
                        peer_addr,
                        peer_as: peer_header.peer_as,
                        is_best: true,
                        llgr_state: rbmp_core::bgp::types::LlgrState::Normal,
                        stale_at: None,
                    };
                    {
                        let rib = self.ribs.entry(peer_key).or_default();
                        rib.insert(rib_type, entry);
                        if path_id.is_some() {
                            rib.recompute_best_path(rib_type, prefix);
                        }
                    }
                    let view_key = ViewKey::from_peer(
                        peer_key,
                        prefix.afi_safi(),
                        rib_type,
                        peer_header.peer_flags,
                    );
                    let route_key = RouteKey {
                        view: view_key,
                        nlri: CanonicalNlri::from_prefix(prefix),
                        path_id,
                    };
                    let ev = RouteChange {
                        action: RouteAction::Announce,
                        peer_header: peer_header.clone(),
                        peer_key,
                        view_key,
                        route_key,
                        rib_type,
                        prefix: prefix.clone(),
                        path_id,
                        attributes: Some(update.attributes.clone()),
                    };
                    self.emit_with_source(
                        msg_id,
                        speaker,
                        source.clone(),
                        now,
                        RibEventPayload::RouteChange(ev),
                        &mut emitted,
                    );
                }

                self.sync_peer_route_counts(&peer_key);
            }
            BmpPayload::StatsReport { peer_header, stats } => {
                self.emit_with_source(
                    msg_id,
                    speaker,
                    source,
                    now,
                    Stats {
                        peer_header,
                        counters: stats,
                    },
                    &mut emitted,
                );
            }
            BmpPayload::RouteMirroring { .. } => {
                // TODO: forward mirrored PDUs to secondary parser
            }
        }

        emitted
    }

    /// Feed a parsed BmpMessage into the RIB. Emits versioned RibEvents on state changes.
    pub fn process(&mut self, msg: BmpMessage) -> Vec<RibEvent> {
        self.process_for_epoch(msg, None)
    }

    fn emit_with_source(
        &mut self,
        msg_id: uuid::Uuid,
        speaker: IpAddr,
        source: ObservationSource,
        occurred_at: DateTime<Utc>,
        payload: RibEventPayload,
        emitted: &mut Vec<RibEvent>,
    ) {
        let seq = self.next_event_seq;
        self.next_event_seq += 1;
        let local_idx = emitted.len() as u64;
        let ev = RibEvent {
            schema_version: EVENT_SCHEMA_VERSION,
            event_key: EventKey::new(source, seq),
            id: deterministic_event_uuid(msg_id, seq, local_idx),
            occurred_at,
            speaker,
            payload,
        };
        let _ = self.event_tx.send(ev.clone());
        emitted.push(ev);
    }

    // ─── Query surface ────────────────────────────────────────────────────────

    pub fn speakers(&self) -> Vec<&BmpSession> {
        self.speakers.values().collect()
    }

    pub fn speaker(&self, addr: IpAddr) -> Option<&BmpSession> {
        self.speakers.get(&addr)
    }

    pub fn rib_for_peer_key(&self, peer_key: &PeerKey) -> Option<&RibTable> {
        self.ribs.get(peer_key)
    }

    pub fn ribs_for_speaker(&self, speaker: IpAddr) -> impl Iterator<Item = (&PeerKey, &RibTable)> {
        self.ribs
            .iter()
            .filter(move |(k, _)| k.speaker_addr == speaker)
    }

    /// Single-peer convenience lookup by `peer_address`. Prefer `rib_for_peer_key` when
    /// multiple exporters or VRFs may monitor the same peer IP.
    pub fn rib_for_peer(&self, peer: IpAddr) -> Option<&RibTable> {
        self.ribs
            .iter()
            .find(|(k, _)| k.peer_address == peer)
            .map(|(_, table)| table)
    }

    pub fn total_routes(&self) -> usize {
        self.ribs.values().flat_map(|r| r.all_prefixes()).count()
    }

    pub fn total_peers_up(&self) -> usize {
        self.speakers.values().map(|s| s.up_peer_count()).sum()
    }

    /// Return a clone of the broadcast Sender so callers can subscribe to events.
    pub fn event_sender(&self) -> broadcast::Sender<RibEvent> {
        self.event_tx.clone()
    }
}

fn deterministic_event_uuid(msg_id: uuid::Uuid, seq: u64, local_idx: u64) -> uuid::Uuid {
    let mut b = *msg_id.as_bytes();
    let mut z1 = seq.wrapping_add(0x9e3779b97f4a7c15);
    z1 = (z1 ^ (z1 >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z1 = (z1 ^ (z1 >> 27)).wrapping_mul(0x94d049bb133111eb);
    z1 ^= z1 >> 31;

    let mut z2 = local_idx.wrapping_add(0x517cc1b727220a95);
    z2 = (z2 ^ (z2 >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z2 = (z2 ^ (z2 >> 27)).wrapping_mul(0x94d049bb133111eb);
    z2 ^= z2 >> 31;

    let mix_hi = z1.to_be_bytes();
    let mix_lo = z2.to_be_bytes();
    for i in 0..8 {
        b[i] ^= mix_hi[i];
        b[i + 8] ^= mix_lo[i];
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ipnet::Ipv4Net;
    use rbmp_core::bgp::types::{BgpUpdate, PathAttributes, Prefix};
    use rbmp_core::bmp::types::{
        BgpOpenInfo, BmpMessage, BmpPayload, PeerDownReason, PeerFlags, PeerHeader, PeerType,
        PeerUpMessage, RibType, ViewDirection,
    };
    use std::net::{IpAddr, Ipv4Addr};

    fn peer_hdr(
        peer_ip: IpAddr,
        peer_type: PeerType,
        rd: [u8; 8],
        rib_type: RibType,
    ) -> PeerHeader {
        PeerHeader {
            peer_type,
            peer_flags: PeerFlags(0),
            peer_distinguisher: rd,
            peer_address: peer_ip,
            peer_as: 65001,
            peer_bgp_id: Ipv4Addr::new(192, 0, 2, 1),
            timestamp: Utc::now(),
            rib_type,
        }
    }

    fn peer_up_msg(speaker: IpAddr, hdr: PeerHeader) -> BmpMessage {
        BmpMessage {
            id: uuid::Uuid::new_v4(),
            received_at: Utc::now(),
            speaker_addr: speaker,
            payload: BmpPayload::PeerUp(PeerUpMessage {
                peer_header: hdr,
                local_addr: speaker,
                local_port: 179,
                remote_port: 50000,
                sent_open: BgpOpenInfo {
                    version: 4,
                    asn: 65000,
                    hold_time: 90,
                    bgp_id: Ipv4Addr::new(10, 0, 0, 1),
                    capabilities: vec![],
                },
                recv_open: BgpOpenInfo {
                    version: 4,
                    asn: 65001,
                    hold_time: 90,
                    bgp_id: Ipv4Addr::new(192, 0, 2, 1),
                    capabilities: vec![],
                },
            }),
        }
    }

    fn route_announce_msg(
        speaker: IpAddr,
        hdr: PeerHeader,
        prefix: Prefix,
        path_id: Option<u32>,
        local_pref: u32,
    ) -> BmpMessage {
        let mut attrs = PathAttributes::default();
        attrs.local_pref = Some(local_pref);
        BmpMessage {
            id: uuid::Uuid::new_v4(),
            received_at: Utc::now(),
            speaker_addr: speaker,
            payload: BmpPayload::RouteMonitoring {
                peer_header: hdr,
                update: BgpUpdate {
                    withdrawn: vec![],
                    withdrawn_path_ids: vec![],
                    attributes: attrs,
                    announced: vec![prefix],
                    announced_path_ids: vec![path_id],
                },
            },
        }
    }

    #[test]
    fn b04_same_peer_on_two_exporters_remains_isolated_across_peer_down_and_termination() {
        let (mut mgr, _rx) = RibManager::new(128);
        let exporter_a: IpAddr = "10.0.0.1".parse().unwrap();
        let exporter_b: IpAddr = "10.0.0.2".parse().unwrap();
        let peer_ip: IpAddr = "192.0.2.1".parse().unwrap();
        let pfx = Prefix::V4("203.0.113.0/24".parse::<Ipv4Net>().unwrap());

        let hdr = peer_hdr(
            peer_ip,
            PeerType::GlobalInstance,
            [0; 8],
            RibType::AdjRibInPrePolicy,
        );
        mgr.process(peer_up_msg(exporter_a, hdr.clone()));
        mgr.process(peer_up_msg(exporter_b, hdr.clone()));

        mgr.process(route_announce_msg(
            exporter_a,
            hdr.clone(),
            pfx.clone(),
            None,
            100,
        ));
        mgr.process(route_announce_msg(
            exporter_b,
            hdr.clone(),
            pfx.clone(),
            None,
            200,
        ));

        assert_eq!(
            mgr.total_routes(),
            2,
            "two exporters monitoring same peer IP must not overwrite each other"
        );
        let key_a = PeerKey::from_header(exporter_a, &hdr);
        let key_b = PeerKey::from_header(exporter_b, &hdr);
        assert_eq!(
            mgr.rib_for_peer_key(&key_a)
                .unwrap()
                .get(RibType::AdjRibInPrePolicy, &pfx)
                .unwrap()
                .attributes
                .local_pref,
            Some(100)
        );
        assert_eq!(
            mgr.rib_for_peer_key(&key_b)
                .unwrap()
                .get(RibType::AdjRibInPrePolicy, &pfx)
                .unwrap()
                .attributes
                .local_pref,
            Some(200)
        );

        // Exporter B's peer goes down -> Exporter A's route must survive
        mgr.process(BmpMessage {
            id: uuid::Uuid::new_v4(),
            received_at: Utc::now(),
            speaker_addr: exporter_b,
            payload: BmpPayload::PeerDown {
                peer_header: hdr.clone(),
                reason: PeerDownReason::RemoteSystemClosed2,
            },
        });
        assert_eq!(
            mgr.total_routes(),
            1,
            "peer down on exporter B must not clear exporter A's routes"
        );
        assert_eq!(
            mgr.rib_for_peer_key(&key_a)
                .unwrap()
                .count(RibType::AdjRibInPrePolicy),
            1
        );
        assert_eq!(
            mgr.rib_for_peer_key(&key_b)
                .unwrap()
                .count(RibType::AdjRibInPrePolicy),
            0
        );

        // Re-announce on B then terminate B -> Exporter A's route still survives
        mgr.process(route_announce_msg(
            exporter_b,
            hdr.clone(),
            pfx.clone(),
            None,
            250,
        ));
        assert_eq!(mgr.total_routes(), 2);
        mgr.process(BmpMessage {
            id: uuid::Uuid::new_v4(),
            received_at: Utc::now(),
            speaker_addr: exporter_b,
            payload: BmpPayload::Termination {
                reason_code: 0,
                reason_text: Some("admin".into()),
            },
        });
        assert_eq!(mgr.total_routes(), 1);
        assert!(mgr.rib_for_peer_key(&key_b).is_none());
        assert_eq!(
            mgr.rib_for_peer_key(&key_a)
                .unwrap()
                .count(RibType::AdjRibInPrePolicy),
            1
        );
    }

    #[test]
    fn b04_same_peer_ip_across_two_vrf_distinguishers_on_same_exporter_stays_isolated() {
        let (mut mgr, _rx) = RibManager::new(128);
        let exporter: IpAddr = "10.0.0.1".parse().unwrap();
        let peer_ip: IpAddr = "192.0.2.1".parse().unwrap();
        let pfx = Prefix::V4("198.51.100.0/24".parse::<Ipv4Net>().unwrap());

        let rd_vrf_a = [0, 0, 0, 100, 0, 0, 0, 1];
        let rd_vrf_b = [0, 0, 0, 200, 0, 0, 0, 2];
        let hdr_vrf_a = peer_hdr(
            peer_ip,
            PeerType::RdInstance,
            rd_vrf_a,
            RibType::AdjRibInPrePolicy,
        );
        let hdr_vrf_b = peer_hdr(
            peer_ip,
            PeerType::RdInstance,
            rd_vrf_b,
            RibType::AdjRibInPrePolicy,
        );

        mgr.process(peer_up_msg(exporter, hdr_vrf_a.clone()));
        mgr.process(peer_up_msg(exporter, hdr_vrf_b.clone()));
        assert_eq!(mgr.speaker(exporter).unwrap().peer_count(), 2);

        mgr.process(route_announce_msg(
            exporter,
            hdr_vrf_a.clone(),
            pfx.clone(),
            Some(11),
            110,
        ));
        mgr.process(route_announce_msg(
            exporter,
            hdr_vrf_b.clone(),
            pfx.clone(),
            Some(22),
            220,
        ));
        assert_eq!(mgr.total_routes(), 2);
        assert_eq!(mgr.speaker(exporter).unwrap().total_routes(), 2);

        // PeerDown in VRF A leaves VRF B intact
        mgr.process(BmpMessage {
            id: uuid::Uuid::new_v4(),
            received_at: Utc::now(),
            speaker_addr: exporter,
            payload: BmpPayload::PeerDown {
                peer_header: hdr_vrf_a,
                reason: PeerDownReason::VrfDown,
            },
        });
        assert_eq!(mgr.total_routes(), 1);
        let key_vrf_b = PeerKey::from_header(exporter, &hdr_vrf_b);
        let surviving = mgr
            .rib_for_peer_key(&key_vrf_b)
            .unwrap()
            .get_with_path_id(RibType::AdjRibInPrePolicy, &pfx, Some(22))
            .unwrap();
        assert_eq!(surviving.attributes.local_pref, Some(220));
    }

    #[test]
    fn b04_stale_session_epoch_disconnect_does_not_clear_newer_session_state() {
        let (mut mgr, _rx) = RibManager::new(128);
        let exporter: IpAddr = "10.0.0.1".parse().unwrap();
        let peer_ip: IpAddr = "192.0.2.1".parse().unwrap();
        let pfx = Prefix::V4("203.0.113.0/24".parse::<Ipv4Net>().unwrap());
        let hdr = peer_hdr(
            peer_ip,
            PeerType::GlobalInstance,
            [0; 8],
            RibType::AdjRibInPrePolicy,
        );

        let old_epoch = mgr.open_speaker_session(exporter, Utc::now());
        mgr.process_for_epoch(peer_up_msg(exporter, hdr.clone()), Some(old_epoch));
        mgr.process_for_epoch(
            route_announce_msg(exporter, hdr.clone(), pfx.clone(), None, 100),
            Some(old_epoch),
        );

        // Reconnect arrives before old socket teardown finishes
        let new_epoch = mgr.open_speaker_session(exporter, Utc::now());
        assert!(new_epoch > old_epoch);
        mgr.process_for_epoch(peer_up_msg(exporter, hdr.clone()), Some(new_epoch));
        mgr.process_for_epoch(
            route_announce_msg(exporter, hdr.clone(), pfx.clone(), None, 300),
            Some(new_epoch),
        );

        // Old connection closes late with old_epoch -> fenced, does not delete new session
        mgr.process_for_epoch(
            BmpMessage {
                id: uuid::Uuid::new_v4(),
                received_at: Utc::now(),
                speaker_addr: exporter,
                payload: BmpPayload::Termination {
                    reason_code: 3,
                    reason_text: Some("redundant".into()),
                },
            },
            Some(old_epoch),
        );
        assert_eq!(mgr.total_routes(), 1);
        assert!(mgr.speaker(exporter).is_some());
    }

    #[test]
    fn b04_event_and_route_identity_schema_serialization_and_repeated_announcements() {
        let (mut mgr, mut rx) = RibManager::new(128);
        let exporter: IpAddr = "10.0.0.1".parse().unwrap();
        let peer_ip: IpAddr = "192.0.2.1".parse().unwrap();
        let pfx = Prefix::V4("203.0.113.0/24".parse::<Ipv4Net>().unwrap());
        let hdr = peer_hdr(
            peer_ip,
            PeerType::RdInstance,
            [0, 1, 2, 3, 4, 5, 6, 7],
            RibType::AdjRibInPostPolicy,
        );

        // Two identical repeated announcements must produce distinct monotonic EventKeys
        mgr.process(route_announce_msg(
            exporter,
            hdr.clone(),
            pfx.clone(),
            Some(7),
            150,
        ));
        mgr.process(route_announce_msg(
            exporter,
            hdr.clone(),
            pfx.clone(),
            Some(7),
            150,
        ));

        let ev1 = rx.try_recv().unwrap();
        let ev2 = rx.try_recv().unwrap();
        assert_eq!(ev1.schema_version, EVENT_SCHEMA_VERSION);
        assert_eq!(ev2.schema_version, EVENT_SCHEMA_VERSION);
        assert_ne!(ev1.event_key, ev2.event_key);
        assert_eq!(ev1.event_key.sequence + 1, ev2.event_key.sequence);

        // Round-trip JSON serialization preserves PeerKey, ViewKey, RouteKey, path_id, and EventKey
        let json = serde_json::to_string(&ev1).unwrap();
        let decoded: RibEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.event_key, ev1.event_key);
        match decoded.payload {
            RibEventPayload::RouteChange(rc) => {
                assert_eq!(rc.path_id, Some(7));
                assert_eq!(rc.peer_key.peer_distinguisher, [0, 1, 2, 3, 4, 5, 6, 7]);
                assert_eq!(rc.view_key.direction, ViewDirection::Inbound);
                assert!(rc.view_key.is_post_policy);
                assert_eq!(rc.route_key.path_id, Some(7));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }
}
