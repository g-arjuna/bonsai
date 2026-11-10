use crate::event::{
    RetiredRoute, RibEvent, RibEventPayload, RibEventPayload::*, RouteAction, RouteChange,
};
use crate::filter::{FilterEngine, FilterVerdict};
use crate::roto_ctx::RouteCtx as RotoCtx;
use crate::roto_filter::RotoFilterEngine;
use crate::session::{BmpSession, PeerSession};
use crate::table::{RibEntry, RibTable};
use chrono::{DateTime, Utc};
use metrics::counter;
use rbmp_core::bgp::types::BgpCapability;
use rbmp_core::bmp::types::{
    BmpMessage, BmpPayload, CanonicalNlri, EVENT_SCHEMA_VERSION, EventKey,
    LEGACY_EXPORTER_INSTANCE, ObservationSource, PeerKey, RouteKey, SpeakerKey, ViewKey,
};
use std::collections::HashMap;
use std::net::IpAddr;
use tokio::sync::broadcast;
use tracing::{debug, info};

/// Central RIB manager: owns all per-speaker sessions and per-(speaker, peer_type, distinguisher, peer_addr) route tables.
/// Driven by BmpMessage from the receiver; emits versioned RibEvents to subscribers.
///
/// BN-021c: speakers and peer/RIB state are keyed by [`SpeakerKey`]
/// (transport address + logical-exporter instance), never by transport
/// IP alone, so independent BMP exporters behind one NAT address keep
/// fully isolated session, peer, route, decode-context and gap state.
pub struct RibManager {
    speakers: HashMap<SpeakerKey, BmpSession>,
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
    ///
    /// `exporter_instance` is the logical-exporter scope (`exp:*` resolved,
    /// `conn:*` connection-isolated, `""`/`"default"` legacy); `exporter_id`
    /// and `device_id` carry trusted-binding resolution when known.
    pub fn open_speaker_session(
        &mut self,
        speaker_addr: IpAddr,
        exporter_instance: &str,
        exporter_id: Option<&str>,
        device_id: Option<&str>,
        at: DateTime<Utc>,
    ) -> u64 {
        let epoch = self.next_session_epoch;
        self.next_session_epoch += 1;
        let key = SpeakerKey::new(speaker_addr, exporter_instance);
        let source = Self::source_for(
            speaker_addr,
            exporter_instance,
            exporter_id,
            device_id,
            epoch,
        );
        match self.speakers.get_mut(&key) {
            Some(sess) => {
                sess.source = source;
                sess.connected_at = at;
                for peer in sess.peers.values_mut() {
                    peer.eor_families.clear();
                }
            }
            None => {
                self.speakers
                    .insert(key, BmpSession::with_source(source, at));
            }
        }
        epoch
    }

    /// Build the `ObservationSource` for a speaker scope. `exporter_id`
    /// falls back to the transport address when no canonical/provisional
    /// exporter id is known; `device_id` stays `None` unless a trusted
    /// binding resolved it.
    fn source_for(
        speaker_addr: IpAddr,
        exporter_instance: &str,
        exporter_id: Option<&str>,
        device_id: Option<&str>,
        session_epoch: u64,
    ) -> ObservationSource {
        let instance = if exporter_instance.trim().is_empty() {
            LEGACY_EXPORTER_INSTANCE
        } else {
            exporter_instance
        };
        ObservationSource::with_ids(
            "default",
            exporter_id
                .map(str::to_string)
                .unwrap_or_else(|| speaker_addr.to_string()),
            speaker_addr,
            instance,
            session_epoch,
        )
        .with_device_id(device_id.map(str::to_string))
    }

    /// Ensure the next emitted `RibEvent` sequence is at least `min_next_seq`.
    pub fn ensure_min_event_seq(&mut self, min_next_seq: u64) {
        if min_next_seq > self.next_event_seq {
            self.next_event_seq = min_next_seq;
        }
    }

    /// Retire a speaker scope entirely: removes the session and all of
    /// its `PeerKey` RIB tables without emitting withdrawals. Used for
    /// isolated connection-scoped (`conn:*`) exporters whose TCP
    /// connection ended — the scope can never receive new data, so
    /// retaining last-known state would leak unboundedly. Durable
    /// history remains in the journals.
    pub fn retire_speaker(&mut self, key: SpeakerKey) -> bool {
        let Some(sess) = self.speakers.remove(&key) else {
            return false;
        };
        for peer_key in sess.peers.keys() {
            self.ribs.remove(peer_key);
        }
        self.ribs.retain(|k, _| {
            !(k.speaker_addr == key.speaker_addr && k.speaker_instance == key.speaker_instance)
        });
        true
    }

    /// Clear End-of-RIB markers for all peers of `key` (used on BMP transport disconnect
    /// so that a subsequent reconnect requires fresh synchronization before reporting complete).
    pub fn clear_speaker_eor(&mut self, key: SpeakerKey) {
        if let Some(sess) = self.speakers.get_mut(&key) {
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

    fn ensure_speaker_source(&mut self, msg: &BmpMessage, now: DateTime<Utc>) -> ObservationSource {
        let key = SpeakerKey::new(msg.speaker_addr, &msg.exporter_instance);
        if let Some(sess) = self.speakers.get(&key) {
            return sess.source.clone();
        }
        let epoch = self.next_session_epoch;
        self.next_session_epoch += 1;
        let source = Self::source_for(
            msg.speaker_addr,
            &msg.exporter_instance,
            msg.exporter_id.as_deref(),
            msg.device_id.as_deref(),
            epoch,
        );
        self.speakers
            .insert(key, BmpSession::with_source(source.clone(), now));
        source
    }

    fn sync_peer_route_counts(&mut self, peer_key: &PeerKey) {
        let counts = self
            .ribs
            .get(peer_key)
            .map(|r| r.rib_counts())
            .unwrap_or_default();
        let key = SpeakerKey {
            speaker_addr: peer_key.speaker_addr,
            speaker_instance: peer_key.speaker_instance,
        };
        if let Some(sess) = self.speakers.get_mut(&key) {
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
        // Logical-exporter scope carried on the message: resolved
        // canonical (`exp:*`), connection-isolated (`conn:*`), or legacy.
        // `""` maps to speaker instance 0 — the pre-BN-021c identity.
        let exporter_instance = msg.exporter_instance.clone();
        let key = SpeakerKey::new(speaker, &exporter_instance);
        let speaker_instance = key.speaker_instance;
        let now = msg.received_at;
        let mut emitted = Vec::new();

        let stale_epoch = session_epoch
            .zip(self.speakers.get(&key).map(|s| s.source.session_epoch))
            .is_some_and(|(incoming, active)| incoming < active);
        if matches!(msg.payload, BmpPayload::Termination { .. }) {
            if let (Some(incoming_epoch), Some(active_sess)) =
                (session_epoch, self.speakers.get(&key))
            {
                if incoming_epoch < active_sess.source.session_epoch {
                    debug!(
                        %speaker,
                        %exporter_instance,
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
                let sess = self.speakers.entry(key).or_insert_with(|| {
                    BmpSession::with_source(
                        Self::source_for(
                            speaker,
                            &exporter_instance,
                            msg.exporter_id.as_deref(),
                            msg.device_id.as_deref(),
                            epoch,
                        ),
                        now,
                    )
                });
                if epoch > sess.source.session_epoch {
                    sess.source.session_epoch = epoch;
                    sess.connected_at = now;
                    for peer in sess.peers.values_mut() {
                        peer.eor_families.clear();
                    }
                }
                if sess.source.device_id.is_none() {
                    sess.source.device_id = msg.device_id.clone();
                }
                sess.source.clone()
            }
            None => self.ensure_speaker_source(&msg, now),
        };

        match msg.payload {
            BmpPayload::Initiation {
                sys_name,
                sys_descr,
                ..
            } => {
                if let Some(session) = self.speakers.get_mut(&key) {
                    session.sys_name = sys_name.clone();
                    session.sys_descr = sys_descr.clone();
                    // BN-021c: promote the protocol-reported sysName to the
                    // provisional logical exporter identity ONLY while the
                    // source still carries the bare transport address —
                    // trusted-binding resolutions (`exp:*`) and explicit
                    // connection scopes keep their assigned identity.
                    if session.source.device_id.is_none()
                        && session.source.exporter_id == session.speaker_addr.to_string()
                    {
                        if let Some(name) =
                            sys_name.as_deref().map(str::trim).filter(|s| !s.is_empty())
                        {
                            session.source.exporter_id = format!("bmp:{name}");
                        }
                    }
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
                if let Some(sess) = self.speakers.remove(&key) {
                    // Clear only this speaker's PeerKey RIB tables; never touch other speakers
                    for peer_key in sess.peers.keys() {
                        self.ribs.remove(peer_key);
                    }
                    info!(%speaker, %exporter_instance, %reason_code, "BMP speaker disconnected");
                }
                // Also remove any RIB tables created directly by RouteMonitoring before PeerUp for this speaker
                self.ribs.retain(|k, _| {
                    !(k.speaker_addr == speaker && k.speaker_instance == speaker_instance)
                });
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
                let peer_key = PeerKey::from_header(speaker, &exporter_instance, &pu.peer_header);
                let mut caps: Vec<BgpCapability> = pu.recv_open.capabilities.clone();
                for sc in &pu.sent_open.capabilities {
                    if !caps.contains(sc) {
                        caps.push(sc.clone());
                    }
                }
                let cap_names: Vec<String> =
                    caps.iter().map(|c| format!("{:?}", c.code())).collect();

                if let Some(speaker_sess) = self.speakers.get_mut(&key) {
                    let peer = speaker_sess
                        .peers
                        .entry(peer_key)
                        .or_insert_with(|| PeerSession::new_for_key(&key, &pu.peer_header));
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
                let peer_key = PeerKey::from_header(speaker, &exporter_instance, &peer_header);
                let reason_str = format!("{:?}", reason);

                let llgr_stale_secs = self
                    .speakers
                    .get(&key)
                    .and_then(|s| s.peers.get(&peer_key))
                    .filter(|p| p.llgr_active)
                    .map(|p| p.llgr_stale_secs);

                if let Some(sess) = self.speakers.get_mut(&key) {
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
                let peer_key = PeerKey::from_header(speaker, &exporter_instance, &peer_header);

                // Ensure peer session exists so speaker.peers tracks all active PeerKeys
                if let Some(speaker_sess) = self.speakers.get_mut(&key) {
                    speaker_sess
                        .peers
                        .entry(peer_key)
                        .or_insert_with(|| PeerSession::new_for_key(&key, &peer_header));
                }

                // Check for End-of-RIB (RFC 4724 §2)
                if let Some(eor_family) = update.eor_afi_safi() {
                    if stale_epoch {
                        // Older versions emitted one EOR event here. Reserve its
                        // sequence so replay cannot renumber later durable events.
                        self.next_event_seq += 1;
                        return emitted;
                    }
                    let retired_routes = self
                        .ribs
                        .get_mut(&peer_key)
                        .map(|rib| {
                            rib.retire_older_epoch(rib_type, eor_family, source.session_epoch)
                        })
                        .unwrap_or_default()
                        .into_iter()
                        .map(|entry| RetiredRoute {
                            route_key: RouteKey {
                                view: ViewKey::from_peer(
                                    peer_key,
                                    eor_family,
                                    rib_type,
                                    peer_header.peer_flags,
                                ),
                                nlri: CanonicalNlri::from_prefix(&entry.prefix),
                                path_id: entry.path_id,
                            },
                            prefix: entry.prefix,
                        })
                        .collect();
                    self.sync_peer_route_counts(&peer_key);
                    if let Some(speaker_sess) = self.speakers.get_mut(&key) {
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
                            retired_routes,
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
                        session_epoch: source.session_epoch,
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

    pub fn speaker(&self, key: SpeakerKey) -> Option<&BmpSession> {
        self.speakers.get(&key)
    }

    /// Look up a speaker session by transport address alone. Only valid
    /// when at most one exporter instance exists for that address —
    /// prefer [`RibManager::speaker`] with an explicit [`SpeakerKey`]
    /// when NAT-shared exporters are possible.
    pub fn speaker_by_addr(&self, addr: IpAddr) -> Option<&BmpSession> {
        self.speakers.values().find(|s| s.speaker_addr == addr)
    }

    pub fn rib_for_peer_key(&self, peer_key: &PeerKey) -> Option<&RibTable> {
        self.ribs.get(peer_key)
    }

    pub fn ribs_for_speaker(&self, key: SpeakerKey) -> impl Iterator<Item = (&PeerKey, &RibTable)> {
        self.ribs.iter().filter(move |(k, _)| {
            k.speaker_addr == key.speaker_addr && k.speaker_instance == key.speaker_instance
        })
    }

    /// All RIB tables for a transport address across every exporter
    /// instance. Prefer `ribs_for_speaker` with an explicit `SpeakerKey`.
    pub fn ribs_for_addr(&self, speaker: IpAddr) -> impl Iterator<Item = (&PeerKey, &RibTable)> {
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
            exporter_instance: String::new(),
            exporter_id: None,
            device_id: None,
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
            exporter_instance: String::new(),
            exporter_id: None,
            device_id: None,
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
    fn reconnect_eor_only_retires_unrefreshed_paths_in_its_view_and_family() {
        let (mut mgr, _) = RibManager::new(128);
        let speaker: IpAddr = "10.0.0.1".parse().unwrap();
        let peer: IpAddr = "192.0.2.1".parse().unwrap();
        let hdr = peer_hdr(
            peer,
            PeerType::GlobalInstance,
            [0; 8],
            RibType::AdjRibInPrePolicy,
        );
        let mut other_view = hdr.clone();
        other_view.rib_type = RibType::AdjRibInPostPolicy;
        let mut other_rd = hdr.clone();
        other_rd.peer_distinguisher[7] = 1;
        let v4 = Prefix::V4("203.0.113.0/24".parse().unwrap());
        let v6 = Prefix::V6("2001:db8::/32".parse().unwrap());
        let old = mgr.open_speaker_session(speaker, "", None, None, Utc::now());
        for (header, prefix, id) in [
            (&hdr, &v4, 1),
            (&hdr, &v4, 2),
            (&hdr, &v6, 3),
            (&other_view, &v4, 4),
            (&other_rd, &v4, 5),
        ] {
            mgr.process_for_epoch(
                route_announce_msg(speaker, header.clone(), prefix.clone(), Some(id), 100),
                Some(old),
            );
        }
        let new = mgr.open_speaker_session(speaker, "", None, None, Utc::now());
        mgr.process_for_epoch(
            route_announce_msg(speaker, hdr.clone(), v4.clone(), Some(2), 200),
            Some(new),
        );
        assert_eq!(
            mgr.total_routes(),
            5,
            "reconnect alone must retain old routes"
        );
        let eor = BmpMessage {
            id: uuid::Uuid::new_v4(),
            received_at: Utc::now(),
            speaker_addr: speaker,
            exporter_instance: String::new(),
            exporter_id: None,
            device_id: None,
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
        };
        let reserved_sequence = mgr.next_event_seq;
        let stale = mgr.process_for_epoch(eor.clone(), Some(old));
        assert!(
            stale.is_empty(),
            "stale EOR must not claim current synchronization"
        );
        assert_eq!(mgr.next_event_seq, reserved_sequence + 1);
        assert_eq!(
            mgr.total_routes(),
            5,
            "stale EOR must not retire new snapshot state"
        );
        let events = mgr.process_for_epoch(eor, Some(new));
        assert_eq!(events[0].event_key.sequence, reserved_sequence + 1);
        assert_eq!(mgr.total_routes(), 4);
        assert!(
            matches!(&events[0].payload, RibEventPayload::EndOfRib { retired_routes, .. } if retired_routes.len() == 1 && retired_routes[0].route_key.path_id == Some(1))
        );
        let rib = mgr
            .rib_for_peer_key(&PeerKey::from_header(speaker, "", &hdr))
            .unwrap();
        assert!(rib.get_with_path_id(hdr.rib_type, &v4, Some(2)).is_some());
        assert!(rib.get_with_path_id(hdr.rib_type, &v6, Some(3)).is_some());
        assert!(
            rib.get_with_path_id(other_view.rib_type, &v4, Some(4))
                .is_some()
        );
        assert!(
            mgr.rib_for_peer_key(&PeerKey::from_header(speaker, "", &other_rd))
                .unwrap()
                .get_with_path_id(hdr.rib_type, &v4, Some(5))
                .is_some()
        );
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
        let key_a = PeerKey::from_header(exporter_a, "", &hdr);
        let key_b = PeerKey::from_header(exporter_b, "", &hdr);
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
            exporter_instance: String::new(),
            exporter_id: None,
            device_id: None,
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
            exporter_instance: String::new(),
            exporter_id: None,
            device_id: None,
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
        assert_eq!(
            mgr.speaker(SpeakerKey::new(exporter, ""))
                .unwrap()
                .peer_count(),
            2
        );

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
        assert_eq!(
            mgr.speaker(SpeakerKey::new(exporter, ""))
                .unwrap()
                .total_routes(),
            2
        );

        // PeerDown in VRF A leaves VRF B intact
        mgr.process(BmpMessage {
            id: uuid::Uuid::new_v4(),
            received_at: Utc::now(),
            speaker_addr: exporter,
            exporter_instance: String::new(),
            exporter_id: None,
            device_id: None,
            payload: BmpPayload::PeerDown {
                peer_header: hdr_vrf_a,
                reason: PeerDownReason::VrfDown,
            },
        });
        assert_eq!(mgr.total_routes(), 1);
        let key_vrf_b = PeerKey::from_header(exporter, "", &hdr_vrf_b);
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

        let old_epoch = mgr.open_speaker_session(exporter, "", None, None, Utc::now());
        mgr.process_for_epoch(peer_up_msg(exporter, hdr.clone()), Some(old_epoch));
        mgr.process_for_epoch(
            route_announce_msg(exporter, hdr.clone(), pfx.clone(), None, 100),
            Some(old_epoch),
        );

        // Reconnect arrives before old socket teardown finishes
        let new_epoch = mgr.open_speaker_session(exporter, "", None, None, Utc::now());
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
                exporter_instance: String::new(),
                exporter_id: None,
                device_id: None,
                payload: BmpPayload::Termination {
                    reason_code: 3,
                    reason_text: Some("redundant".into()),
                },
            },
            Some(old_epoch),
        );
        assert_eq!(mgr.total_routes(), 1);
        assert!(mgr.speaker(SpeakerKey::new(exporter, "")).is_some());
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
