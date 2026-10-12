use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use rbmp_core::bgp::types::{BgpCapability, AfiSafi};
use rbmp_core::bmp::types::{ObservationSource, PeerHeader, PeerKey, PeerType, RibType};

/// Lifecycle state of a BGP peer as seen via BMP
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerState {
    Up,
    Down,
    /// Session unknown — BMP speaker connected but Peer Up not yet received
    Unknown,
}

/// A BGP peer session tracked by the RIB engine
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerSession {
    pub peer_key:          PeerKey,
    pub peer_type:         PeerType,
    pub peer_distinguisher:[u8; 8],
    pub peer_address:      IpAddr,
    pub peer_as:           u32,
    pub peer_bgp_id:       Ipv4Addr,
    pub local_as:          u32,
    pub hold_time:         u16,
    pub state:             PeerState,
    pub up_at:             Option<DateTime<Utc>>,
    pub down_at:           Option<DateTime<Utc>>,
    pub capabilities:      Vec<BgpCapability>,
    /// Counts of routes per RIB type currently held for this peer
    pub route_counts:      HashMap<RibType, usize>,
    pub flap_count:        u32,
    /// AFI/SAFIs for which Add-Path is active (RFC 7911)
    /// Populated from the Add-Path capability in PeerUp OPEN.
    pub add_path_families: Vec<(AfiSafi, u8)>,
    /// LLGR stale time negotiated in OPEN (RFC 9494); 0 = LLGR not active
    pub llgr_stale_secs:   u32,
    /// True when LLGR capability was present in peer OPEN
    pub llgr_active:       bool,
    /// Per-(RIB view, AFI/SAFI) End-of-RIB completion state for the active session
    #[serde(default)]
    pub eor_families:      HashMap<RibType, std::collections::HashSet<AfiSafi>>,
}

impl PeerSession {
    pub fn new(hdr: &PeerHeader) -> Self {
        Self::new_for_speaker(IpAddr::V4(Ipv4Addr::UNSPECIFIED), hdr)
    }

    pub fn new_for_speaker(speaker_addr: IpAddr, hdr: &PeerHeader) -> Self {
        Self {
            peer_key:           PeerKey::from_header(speaker_addr, hdr),
            peer_type:          hdr.peer_type,
            peer_distinguisher: hdr.peer_distinguisher,
            peer_address:       hdr.peer_address,
            peer_as:            hdr.peer_as,
            peer_bgp_id:        hdr.peer_bgp_id,
            local_as:           0,
            hold_time:          0,
            state:              PeerState::Unknown,
            up_at:              None,
            down_at:            None,
            capabilities:       Vec::new(),
            route_counts:       HashMap::new(),
            flap_count:         0,
            add_path_families:  Vec::new(),
            llgr_stale_secs:    0,
            llgr_active:        false,
            eor_families:       HashMap::new(),
        }
    }

    pub fn on_up_with_header(
        &mut self,
        hdr: &PeerHeader,
        local_as: u32,
        hold_time: u16,
        caps: Vec<BgpCapability>,
        at: DateTime<Utc>,
    ) {
        self.peer_as = hdr.peer_as;
        self.peer_bgp_id = hdr.peer_bgp_id;
        self.on_up(local_as, hold_time, caps, at);
    }

    pub fn on_up(&mut self, local_as: u32, hold_time: u16, caps: Vec<BgpCapability>, at: DateTime<Utc>) {
        if self.state == PeerState::Up { self.flap_count += 1; }
        self.state    = PeerState::Up;
        self.up_at    = Some(at);
        self.local_as  = local_as;
        self.hold_time = hold_time;
        self.eor_families.clear();
        // Extract Add-Path families from capabilities (RFC 7911)
        let mut ap = Vec::new();
        for c in &caps {
            if let BgpCapability::AddPath(entries) = c {
                for &(fam, mode) in entries {
                    if !ap.iter().any(|(a, _)| *a == fam) {
                        ap.push((fam, mode));
                    }
                }
            }
        }
        self.add_path_families = ap;
        // Detect LLGR capability (RFC 9494)
        // Each entry: AFI(2)+SAFI(1)+flags(1)+stale_time(3) = 7 bytes
        // We take the maximum stale_time across all advertised AFI/SAFIs
        self.llgr_stale_secs = caps.iter().find_map(|c| {
            if let BgpCapability::LongLivedGracefulRestart { entries } = c {
                let mut max_secs = 0u32;
                let mut i = 0;
                while i + 7 <= entries.len() {
                    let t = u32::from_be_bytes([0, entries[i+4], entries[i+5], entries[i+6]]);
                    if t > max_secs { max_secs = t; }
                    i += 7;
                }
                Some(max_secs)
            } else { None }
        }).unwrap_or(0);
        self.llgr_active = self.llgr_stale_secs > 0;
        self.capabilities = caps;
    }

    pub fn add_path_active_for(&self, afi_safi: AfiSafi) -> bool {
        self.add_path_families.iter().any(|(a, m)| *a == afi_safi && *m > 0)
    }

    pub fn decode_context(&self, four_byte_asn: bool) -> rbmp_core::bgp::types::BgpDecodeContext {
        rbmp_core::bgp::types::BgpDecodeContext::from_capabilities(four_byte_asn, &self.capabilities)
    }

    pub fn mark_eor(&mut self, rib_type: RibType, afi_safi: AfiSafi) {
        self.eor_families.entry(rib_type).or_default().insert(afi_safi);
    }

    pub fn has_eor(&self, rib_type: RibType, afi_safi: AfiSafi) -> bool {
        self.eor_families
            .get(&rib_type)
            .is_some_and(|set| set.contains(&afi_safi))
    }

    pub fn on_down(&mut self, at: DateTime<Utc>) {
        self.state   = PeerState::Down;
        self.down_at = Some(at);
        self.route_counts.clear();
        self.eor_families.clear();
    }

    pub fn uptime_secs(&self) -> Option<i64> {
        self.up_at.map(|t| (Utc::now() - t).num_seconds())
    }
}

/// A BMP speaker (router) that has connected to us
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BmpSession {
    pub source:       ObservationSource,
    pub speaker_addr: IpAddr,
    pub sys_name:     Option<String>,
    pub sys_descr:    Option<String>,
    pub connected_at: DateTime<Utc>,
    pub peers:        HashMap<PeerKey, PeerSession>,
}

impl BmpSession {
    pub fn new(speaker_addr: IpAddr, at: DateTime<Utc>) -> Self {
        Self::with_source(ObservationSource::new(speaker_addr, 1), at)
    }

    pub fn with_source(source: ObservationSource, at: DateTime<Utc>) -> Self {
        let speaker_addr = source.exporter_addr;
        Self {
            source,
            speaker_addr,
            sys_name:     None,
            sys_descr:    None,
            connected_at: at,
            peers:        HashMap::new(),
        }
    }

    pub fn peer_by_key(&self, key: &PeerKey) -> Option<&PeerSession> {
        self.peers.get(key)
    }

    pub fn peer_by_addr(&self, addr: IpAddr) -> Option<&PeerSession> {
        self.peers.values().find(|p| p.peer_address == addr)
    }

    pub fn peer_count(&self) -> usize { self.peers.len() }

    pub fn up_peer_count(&self) -> usize {
        self.peers.values().filter(|p| p.state == PeerState::Up).count()
    }

    pub fn total_routes(&self) -> usize {
        self.peers.values().flat_map(|p| p.route_counts.values()).sum()
    }
}

