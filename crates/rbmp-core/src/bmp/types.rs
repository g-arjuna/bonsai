use std::net::{IpAddr, Ipv4Addr};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use crate::bgp::types::{BgpCapability, BgpUpdate, AfiSafi, Prefix};

// ─── BMP message type (RFC 7854 §4) ──────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum BmpMsgType {
    RouteMonitoring  = 0,
    StatsReport      = 1,
    PeerDown         = 2,
    PeerUp           = 3,
    Initiation       = 4,
    Termination      = 5,
    RouteMirroring   = 6,
}

impl TryFrom<u8> for BmpMsgType {
    type Error = crate::Error;
    fn try_from(v: u8) -> crate::Result<Self> {
        match v {
            0 => Ok(Self::RouteMonitoring),
            1 => Ok(Self::StatsReport),
            2 => Ok(Self::PeerDown),
            3 => Ok(Self::PeerUp),
            4 => Ok(Self::Initiation),
            5 => Ok(Self::Termination),
            6 => Ok(Self::RouteMirroring),
            _ => Err(crate::Error::InvalidMessageType(v)),
        }
    }
}

// ─── Peer type (RFC 7854 §4.2) ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum PeerType {
    GlobalInstance = 0,  // Default — global BGP instance
    RdInstance     = 1,  // VRF-specific (RD identifies the instance)
    LocalInstance  = 2,  // Local (collector-local) BGP instance
    LocRib         = 3,  // Loc-RIB (RFC 9069)
}

impl TryFrom<u8> for PeerType {
    type Error = crate::Error;
    fn try_from(v: u8) -> crate::Result<Self> {
        match v {
            0 => Ok(Self::GlobalInstance),
            1 => Ok(Self::RdInstance),
            2 => Ok(Self::LocalInstance),
            3 => Ok(Self::LocRib),
            _ => Err(crate::Error::BmpParse(format!("unknown peer type {v}"))),
        }
    }
}

// ─── Peer flags (RFC 7854 §4.2 bits) ─────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PeerFlags(pub u8);

impl PeerFlags {
    pub fn ipv6(self)          -> bool { self.0 & 0x80 != 0 }
    pub fn post_policy(self)   -> bool { self.0 & 0x40 != 0 }
    pub fn as2(self)           -> bool { self.0 & 0x20 != 0 }
    pub fn four_byte_asn(self) -> bool { !self.as2() }
    pub fn adj_rib_out(self)   -> bool { self.0 & 0x10 != 0 }  // RFC 8671 §4
    pub fn filtered(self)      -> bool { self.0 & 0x80 != 0 }  // RFC 9069 §4.2 Loc-RIB F flag
}

// ─── RIB type derived from peer_type + peer_flags ────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RibType {
    AdjRibInPrePolicy,
    AdjRibInPostPolicy,
    AdjRibOutPrePolicy,
    AdjRibOutPostPolicy,
    LocRib,
    LocRibFiltered,
}

impl RibType {
    pub fn from_peer(peer_type: PeerType, flags: PeerFlags) -> Self {
        match peer_type {
            PeerType::LocRib => {
                if flags.filtered() { Self::LocRibFiltered } else { Self::LocRib }
            }
            PeerType::GlobalInstance | PeerType::RdInstance | PeerType::LocalInstance => {
                if flags.adj_rib_out() {
                    if flags.post_policy() { Self::AdjRibOutPostPolicy } else { Self::AdjRibOutPrePolicy }
                } else {
                    if flags.post_policy() { Self::AdjRibInPostPolicy } else { Self::AdjRibInPrePolicy }
                }
            }
        }
    }
}

// ─── Common peer header (RFC 7854 §4.2, 42 bytes) ────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerHeader {
    pub peer_type:          PeerType,
    pub peer_flags:         PeerFlags,
    /// 8-byte Route Distinguisher (zero for global instance)
    pub peer_distinguisher: [u8; 8],
    /// IPv4 or IPv6 peer address
    pub peer_address:       IpAddr,
    pub peer_as:            u32,
    pub peer_bgp_id:        Ipv4Addr,
    pub timestamp:          DateTime<Utc>,
    pub rib_type:           RibType,
}

// ─── B04 Typed Routing Identities (ADR-001) ──────────────────────────────────

pub const EVENT_SCHEMA_VERSION: u16 = 2;

/// Collector + exporter + instance + connection epoch identity.
/// TCP source IP is retained as an attribute while `session_epoch` fences stale disconnects.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ObservationSource {
    pub collector_id:      String,
    pub exporter_id:       String,
    pub exporter_addr:     IpAddr,
    pub exporter_instance: String,
    pub session_epoch:     u64,
}

impl ObservationSource {
    pub fn new(exporter_addr: IpAddr, session_epoch: u64) -> Self {
        Self {
            collector_id: "default".to_string(),
            exporter_id: exporter_addr.to_string(),
            exporter_addr,
            exporter_instance: "default".to_string(),
            session_epoch,
        }
    }

    pub fn with_ids(
        collector_id: impl Into<String>,
        exporter_id: impl Into<String>,
        exporter_addr: IpAddr,
        exporter_instance: impl Into<String>,
        session_epoch: u64,
    ) -> Self {
        Self {
            collector_id: collector_id.into(),
            exporter_id: exporter_id.into(),
            exporter_addr,
            exporter_instance: exporter_instance.into(),
            session_epoch,
        }
    }
}

/// Composite peer/instance identity preventing cross-exporter and cross-VRF collisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PeerKey {
    pub speaker_addr:       IpAddr,
    pub peer_type:          PeerType,
    pub peer_distinguisher: [u8; 8],
    pub peer_address:       IpAddr,
}

impl PeerKey {
    pub fn new(
        speaker_addr: IpAddr,
        peer_type: PeerType,
        peer_distinguisher: [u8; 8],
        peer_address: IpAddr,
    ) -> Self {
        Self {
            speaker_addr,
            peer_type,
            peer_distinguisher,
            peer_address,
        }
    }

    pub fn from_header(speaker_addr: IpAddr, hdr: &PeerHeader) -> Self {
        Self {
            speaker_addr,
            peer_type: hdr.peer_type,
            peer_distinguisher: hdr.peer_distinguisher,
            peer_address: hdr.peer_address,
        }
    }

    pub fn distinguisher_string(&self) -> String {
        let hi = u32::from_be_bytes([
            self.peer_distinguisher[0],
            self.peer_distinguisher[1],
            self.peer_distinguisher[2],
            self.peer_distinguisher[3],
        ]);
        let lo = u32::from_be_bytes([
            self.peer_distinguisher[4],
            self.peer_distinguisher[5],
            self.peer_distinguisher[6],
            self.peer_distinguisher[7],
        ]);
        format!("{hi}:{lo}")
    }
}

/// Direction of a BMP view relative to the exporting router.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewDirection {
    Inbound,
    Outbound,
    Local,
}

/// Typed BMP RIB view identity (peer/instance + AFI/SAFI + direction + policy stage + filtered).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ViewKey {
    pub peer:           PeerKey,
    pub afi_safi:       AfiSafi,
    pub rib_type:       RibType,
    pub direction:      ViewDirection,
    pub is_post_policy: bool,
    pub is_filtered:    bool,
}

impl ViewKey {
    pub fn from_peer(peer: PeerKey, afi_safi: AfiSafi, rib_type: RibType, flags: PeerFlags) -> Self {
        let (direction, is_post_policy, is_filtered) = match rib_type {
            RibType::AdjRibInPrePolicy => (ViewDirection::Inbound, false, false),
            RibType::AdjRibInPostPolicy => (ViewDirection::Inbound, true, false),
            RibType::AdjRibOutPrePolicy => (ViewDirection::Outbound, false, false),
            RibType::AdjRibOutPostPolicy => (ViewDirection::Outbound, true, false),
            RibType::LocRib => (ViewDirection::Local, true, flags.filtered()),
            RibType::LocRibFiltered => (ViewDirection::Local, true, true),
        };
        Self {
            peer,
            afi_safi,
            rib_type,
            direction,
            is_post_policy,
            is_filtered,
        }
    }
}

/// Family-specific NLRI identity, keeping L3VPN/EVPN RDs separate from BMP peer distinguishers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "family", rename_all = "snake_case")]
pub enum CanonicalNlri {
    IpUnicast(Prefix),
    L3Vpn { rd: [u8; 8], prefix: Prefix },
    Evpn { rd: String, route_type: u8, key: String },
}

impl CanonicalNlri {
    pub fn from_prefix(prefix: &Prefix) -> Self {
        match prefix {
            Prefix::Vpn { rd, prefix: inner, .. } => Self::L3Vpn {
                rd: rd.0,
                prefix: (**inner).clone(),
            },
            other => Self::IpUnicast(other.clone()),
        }
    }

    pub fn matches_prefix(&self, target: &Prefix) -> bool {
        match (self, target) {
            (Self::IpUnicast(p), t) => p == t,
            (Self::L3Vpn { rd, prefix }, Prefix::Vpn { rd: trd, prefix: tp, .. }) => {
                *rd == trd.0 && prefix == &**tp
            }
            _ => false,
        }
    }
}

/// Typed intra-table key (`CanonicalNlri` + optional RFC 7911 `path_id`) replacing string formatting.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TableRouteKey {
    pub nlri:    CanonicalNlri,
    pub path_id: Option<u32>,
}

impl TableRouteKey {
    pub fn from_prefix(prefix: &Prefix, path_id: Option<u32>) -> Self {
        Self {
            nlri: CanonicalNlri::from_prefix(prefix),
            path_id,
        }
    }

    pub fn matches_prefix(&self, prefix: &Prefix) -> bool {
        self.nlri.matches_prefix(prefix)
    }
}

/// Full route identity across exporter, peer/instance, view, NLRI, and Add-Path ID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RouteKey {
    pub view:    ViewKey,
    pub nlri:    CanonicalNlri,
    pub path_id: Option<u32>,
}

/// Versioned durable event identity (schema version + observation source + monotonic sequence).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EventKey {
    pub schema_version: u16,
    pub source:         ObservationSource,
    pub sequence:       u64,
}

impl EventKey {
    pub fn new(source: ObservationSource, sequence: u64) -> Self {
        Self {
            schema_version: EVENT_SCHEMA_VERSION,
            source,
            sequence,
        }
    }
}


// ─── BMP initiation TLV (RFC 7854 §4.4) ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitiationTlv {
    pub tlv_type: u16,
    pub value:    String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InitiationInfo {
    SysDescr(String),
    SysName(String),
    AdminLabel(String),
    Unknown { tlv_type: u16, hex: String },
}

// ─── BMP termination TLV (RFC 7854 §4.5) ─────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u16)]
pub enum TermReason {
    AdministrativelyClosed = 0,
    Unspecified            = 1,
    OutOfResources         = 2,
    RedundantConnection    = 3,
    PermanentlyAdminClosed = 4,
}

// ─── Peer Down reason (RFC 7854 §4.9) ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PeerDownReason {
    LocalSystemClosed { notification: Option<Vec<u8>> },  // 1
    LocalSystemClosed2,                                    // 2 (FSM event)
    RemoteSystemClosed { notification: Option<Vec<u8>> }, // 3
    RemoteSystemClosed2,                                   // 4 (BMP message data)
    PeerDeConfigured,                                      // 5
    VrfDown,                                               // 6
    Unknown(u8),
}

// ─── BGP OPEN info (captured at Peer Up) ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BgpOpenInfo {
    pub version:      u8,
    pub asn:          u32,
    pub hold_time:    u16,
    pub bgp_id:       Ipv4Addr,
    pub capabilities: Vec<BgpCapability>,
}

// ─── Peer Up message (RFC 7854 §4.10) ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerUpMessage {
    pub peer_header:  PeerHeader,
    pub local_addr:   IpAddr,
    pub local_port:   u16,
    pub remote_port:  u16,
    pub sent_open:    BgpOpenInfo,
    pub recv_open:    BgpOpenInfo,
}

// ─── Statistics (RFC 7854 §4.8 + RFC 9972) ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatEntry {
    pub stat_type: u16,
    pub name:      String,
    pub value:     u64,
    /// Present for per-AFI/SAFI stats (7-byte RFC 7854 or 11-byte RFC 9972 format)
    pub afi_safi:  Option<AfiSafi>,
}

/// RFC 7854 §4.8 + RFC 9972 statistics counter type names
pub fn stat_name(t: u16) -> &'static str {
    match t {
        0  => "prefixes-rejected-by-inbound-policy",
        1  => "duplicate-prefix-advertisements",
        2  => "duplicate-withdrawals",
        3  => "cluster-list-loop-detections",
        4  => "as-path-loop-detections",
        5  => "originator-id-loop-detections",
        6  => "implicit-withdraw-count",
        7  => "explicit-withdraw-count",
        8  => "update-treatments-as-withdraw",
        9  => "prefixes-treated-as-withdraw",
        10 => "duplicate-update-messages",
        11 => "adj-rib-in-routes",
        12 => "loc-rib-routes",
        13 => "per-afi-safi-adj-rib-in-routes",
        14 => "per-afi-safi-loc-rib-routes",
        15 => "updates-subjected-to-treat-as-withdraw",
        16 => "prefixes-subjected-to-treat-as-withdraw",
        17 => "duplicate-update-messages-rcvd",
        // RFC 9972 (May 2026) — types 18–38
        18 => "pre-policy-adj-rib-in-routes",
        19 => "per-afi-safi-pre-policy-adj-rib-in-routes",
        20 => "post-policy-adj-rib-in-routes",
        21 => "per-afi-safi-post-policy-adj-rib-in-routes",
        22 => "per-afi-safi-pre-policy-routes-rejected-inbound",
        23 => "per-afi-safi-post-policy-routes-accepted-inbound",
        24 => "adj-rib-out-pre-policy-routes",
        25 => "per-afi-safi-adj-rib-out-pre-policy-routes",
        26 => "per-afi-safi-damped-routes",
        27 => "per-afi-safi-gr-stale-routes",
        28 => "per-afi-safi-llgr-stale-routes",
        29 => "pre-route-limit-adj-rib-in",
        30 => "per-afi-safi-pre-route-limit-adj-rib-in",
        31 => "pre-license-limit-routes",
        32 => "per-afi-safi-pre-license-limit-routes",
        33 => "max-aspath-length-rejected",
        34 => "per-afi-safi-max-aspath-length-rejected",
        35 => "per-afi-safi-rpki-invalidated-routes",
        36 => "per-afi-safi-rpki-not-valid-routes",
        37 => "adj-rib-out-post-policy-routes",
        38 => "per-afi-safi-adj-rib-out-post-policy-routes",
        _  => "unknown",
    }
}

/// RFC 9972 per-AFI/SAFI stat types use 11-byte encoding: AFI(2)+SAFI(1)+Gauge(8)
pub fn stat_is_per_afi_safi_11byte(t: u16) -> bool {
    matches!(t, 19 | 21 | 22 | 23 | 25 | 26 | 27 | 28 | 30 | 32 | 34 | 35 | 36 | 38)
}

/// RFC 7854 §4.8 per-AFI/SAFI stat types use 7-byte encoding: AFI(2)+SAFI(1)+Counter(4)
pub fn stat_is_per_afi_safi_7byte(t: u16) -> bool {
    matches!(t, 9 | 10 | 13 | 14)
}

// ─── Top-level BMP message envelope ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BmpMessage {
    /// Unique ID for this message (assigned by receiver)
    pub id:           uuid::Uuid,
    /// Wall-clock when received (UTC)
    pub received_at:  DateTime<Utc>,
    /// IP address of the BMP speaker (router) that sent this
    pub speaker_addr: IpAddr,
    pub payload:      BmpPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BmpPayload {
    Initiation {
        sys_name:  Option<String>,
        sys_descr: Option<String>,
        labels:    Vec<String>,
    },
    Termination {
        reason_code: u16,
        reason_text: Option<String>,
    },
    PeerUp(PeerUpMessage),
    PeerDown {
        peer_header: PeerHeader,
        reason:      PeerDownReason,
    },
    RouteMonitoring {
        peer_header: PeerHeader,
        update:      BgpUpdate,
    },
    StatsReport {
        peer_header: PeerHeader,
        stats:       Vec<StatEntry>,
    },
    RouteMirroring {
        peer_header: PeerHeader,
        /// Raw mirrored BGP PDU bytes
        pdu:         Vec<u8>,
    },
}

impl BmpMessage {
    pub fn msg_type(&self) -> &'static str {
        match &self.payload {
            BmpPayload::Initiation { .. }  => "initiation",
            BmpPayload::Termination { .. } => "termination",
            BmpPayload::PeerUp(_)          => "peer_up",
            BmpPayload::PeerDown { .. }    => "peer_down",
            BmpPayload::RouteMonitoring { .. } => "route_monitoring",
            BmpPayload::StatsReport { .. } => "stats_report",
            BmpPayload::RouteMirroring { .. } => "route_mirroring",
        }
    }
}
