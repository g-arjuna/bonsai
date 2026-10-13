use super::evpn::EvpnRoute;
use super::flowspec::FlowspecNlri;
use ipnet::{Ipv4Net, Ipv6Net};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr};

// ─── LLGR state machine (RFC 9494) ───────────────────────────────────────────

/// Lifecycle state of a route under Long-Lived Graceful Restart.
/// Transitions: Normal → StaleMarked (on peer Down + LLGR active)
///              StaleMarked → Deleted (after stale timer expiry)
///              Any → Normal (on route re-announcement from peer)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LlgrState {
    #[default]
    Normal,
    /// Route is stale; COMMUNITY_LLGR_STALE attached; still usable for forwarding
    StaleMarked,
    /// Stale timer expired; route should be removed
    Deleted,
}

// ─── AFI / SAFI ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Afi {
    Ipv4,
    Ipv6,
    L2Vpn,
    BgpLs,
    Unknown(u16),
}

impl From<u16> for Afi {
    fn from(v: u16) -> Self {
        match v {
            1 => Self::Ipv4,
            2 => Self::Ipv6,
            25 => Self::L2Vpn,
            16388 => Self::BgpLs,
            _ => Self::Unknown(v),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Safi {
    Unicast,
    Multicast,
    MplsVpnMulticast,
    LabeledUnicast,
    MplsVpn,
    Evpn,
    BgpLs,
    BgpLsSrv6,
    SrPolicy,
    Flowspec,
    FlowspecVpn,
    Vpls,
    RouteTargetConstraint,
    McastVpn,
    Unknown(u8),
}

impl From<u8> for Safi {
    fn from(v: u8) -> Self {
        match v {
            1 => Self::Unicast,
            2 => Self::Multicast,
            4 => Self::LabeledUnicast,
            5 => Self::MplsVpnMulticast,
            65 => Self::Vpls,
            70 => Self::Evpn,
            71 => Self::BgpLs,
            72 => Self::BgpLsSrv6,
            73 => Self::SrPolicy,
            128 => Self::MplsVpn,
            129 => Self::McastVpn,
            132 => Self::RouteTargetConstraint,
            133 => Self::Flowspec,
            134 => Self::FlowspecVpn,
            _ => Self::Unknown(v),
        }
    }
}

/// Explicit support classification for an AFI/SAFI family across the BGP/BMP pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FamilySupportStatus {
    /// Full RIB state, canonical RouteKey, Add-Path, and End-of-RIB support.
    Supported,
    /// Decoded into PathAttributes for telemetry/inspection, not stored as prefix RIB routes.
    ExperimentalParseOnly,
    /// Unrecognized AFI/SAFI combination.
    Unsupported,
}

/// Combined AFI/SAFI address family identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AfiSafi {
    pub afi: Afi,
    pub safi: Safi,
}

impl AfiSafi {
    pub fn new(afi: u16, safi: u8) -> Self {
        Self {
            afi: Afi::from(afi),
            safi: Safi::from(safi),
        }
    }
    pub fn ipv4_unicast() -> Self {
        Self::new(1, 1)
    }
    pub fn ipv6_unicast() -> Self {
        Self::new(2, 1)
    }
    pub fn ipv4_vpn() -> Self {
        Self::new(1, 128)
    }
    pub fn ipv6_vpn() -> Self {
        Self::new(2, 128)
    }
    pub fn evpn() -> Self {
        Self::new(25, 70)
    }
    pub fn ipv4_labeled() -> Self {
        Self::new(1, 4)
    }
    pub fn ipv6_labeled() -> Self {
        Self::new(2, 4)
    }

    /// Classify how this AFI/SAFI is supported in the routing state pipeline.
    pub fn support_status(self) -> FamilySupportStatus {
        match (self.afi, self.safi) {
            (
                Afi::Ipv4 | Afi::Ipv6,
                Safi::Unicast | Safi::Multicast | Safi::LabeledUnicast | Safi::MplsVpn,
            ) => FamilySupportStatus::Supported,
            (Afi::L2Vpn, Safi::Evpn | Safi::Vpls)
            | (Afi::BgpLs, Safi::BgpLs | Safi::BgpLsSrv6)
            | (
                Afi::Ipv4 | Afi::Ipv6,
                Safi::Flowspec
                | Safi::FlowspecVpn
                | Safi::SrPolicy
                | Safi::RouteTargetConstraint
                | Safi::McastVpn
                | Safi::MplsVpnMulticast,
            ) => FamilySupportStatus::ExperimentalParseOnly,
            _ => FamilySupportStatus::Unsupported,
        }
    }
}

impl fmt::Display for AfiSafi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match (self.afi, self.safi) {
            (Afi::Ipv4, Safi::Unicast) => "ipv4-unicast",
            (Afi::Ipv6, Safi::Unicast) => "ipv6-unicast",
            (Afi::Ipv4, Safi::Multicast) => "ipv4-multicast",
            (Afi::Ipv6, Safi::Multicast) => "ipv6-multicast",
            (Afi::Ipv4, Safi::LabeledUnicast) => "ipv4-labeled-unicast",
            (Afi::Ipv6, Safi::LabeledUnicast) => "ipv6-labeled-unicast",
            (Afi::Ipv4, Safi::MplsVpn) => "ipv4-vpn",
            (Afi::Ipv6, Safi::MplsVpn) => "ipv6-vpn",
            (Afi::L2Vpn, Safi::Evpn) => "l2vpn-evpn",
            (Afi::Ipv4, Safi::Flowspec) => "ipv4-flowspec",
            (Afi::Ipv6, Safi::Flowspec) => "ipv6-flowspec",
            _ => return write!(f, "afi{}-safi{}", self.afi.as_u16(), self.safi.as_u8()),
        };
        f.write_str(s)
    }
}

impl Afi {
    pub fn as_u16(self) -> u16 {
        match self {
            Self::Ipv4 => 1,
            Self::Ipv6 => 2,
            Self::L2Vpn => 25,
            Self::BgpLs => 16388,
            Self::Unknown(v) => v,
        }
    }
}

impl Safi {
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Unicast => 1,
            Self::Multicast => 2,
            Self::LabeledUnicast => 4,
            Self::MplsVpnMulticast => 5,
            Self::Vpls => 65,
            Self::Evpn => 70,
            Self::BgpLs => 71,
            Self::BgpLsSrv6 => 72,
            Self::SrPolicy => 73,
            Self::MplsVpn => 128,
            Self::McastVpn => 129,
            Self::RouteTargetConstraint => 132,
            Self::Flowspec => 133,
            Self::FlowspecVpn => 134,
            Self::Unknown(v) => v,
        }
    }
}

// ─── Prefix types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Prefix {
    V4(Ipv4Net),
    V6(Ipv6Net),
    /// MPLS labeled (RFC 3107): prefix + MPLS label stack
    Labeled {
        prefix: Box<Prefix>,
        labels: SmallVec<[u32; 2]>,
    },
    /// VPN: Route Distinguisher + labeled prefix (RFC 4364)
    Vpn {
        rd: RouteDistinguisher,
        prefix: Box<Prefix>,
        labels: SmallVec<[u32; 2]>,
    },
}

impl Prefix {
    pub fn addr_family(&self) -> Afi {
        match self {
            Self::V4(_) => Afi::Ipv4,
            Self::V6(_) => Afi::Ipv6,
            Self::Labeled { prefix, .. } | Self::Vpn { prefix, .. } => prefix.addr_family(),
        }
    }

    pub fn afi_safi(&self) -> AfiSafi {
        match self {
            Self::V4(_) => AfiSafi::ipv4_unicast(),
            Self::V6(_) => AfiSafi::ipv6_unicast(),
            Self::Labeled { prefix, .. } => match prefix.addr_family() {
                Afi::Ipv6 => AfiSafi::ipv6_labeled(),
                _ => AfiSafi::ipv4_labeled(),
            },
            Self::Vpn { prefix, .. } => match prefix.addr_family() {
                Afi::Ipv6 => AfiSafi::ipv6_vpn(),
                _ => AfiSafi::ipv4_vpn(),
            },
        }
    }
}

impl fmt::Display for Prefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V4(n) => write!(f, "{}", n),
            Self::V6(n) => write!(f, "{}", n),
            Self::Labeled { prefix, labels } => write!(f, "{} label={}", prefix, labels[0]),
            Self::Vpn { rd, prefix, labels } => write!(f, "{}:{} label={}", rd, prefix, labels[0]),
        }
    }
}

// ─── Route Distinguisher (RFC 4364) ──────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RouteDistinguisher(pub [u8; 8]);

impl RouteDistinguisher {
    pub fn zero() -> Self {
        Self([0u8; 8])
    }
    pub fn is_zero(&self) -> bool {
        self.0 == [0u8; 8]
    }

    pub fn rd_type(&self) -> u16 {
        u16::from_be_bytes([self.0[0], self.0[1]])
    }
}

impl fmt::Display for RouteDistinguisher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.rd_type() {
            0 => {
                let admin = u16::from_be_bytes([self.0[2], self.0[3]]);
                let assigned = u32::from_be_bytes([self.0[4], self.0[5], self.0[6], self.0[7]]);
                write!(f, "{}:{}", admin, assigned)
            }
            1 => {
                let admin = Ipv4Addr::from([self.0[2], self.0[3], self.0[4], self.0[5]]);
                let assigned = u16::from_be_bytes([self.0[6], self.0[7]]);
                write!(f, "{}:{}", admin, assigned)
            }
            2 => {
                let admin = u32::from_be_bytes([self.0[2], self.0[3], self.0[4], self.0[5]]);
                let assigned = u16::from_be_bytes([self.0[6], self.0[7]]);
                write!(f, "{}:{}", admin, assigned)
            }
            _ => write!(f, "0x{}", hex::encode(self.0)),
        }
    }
}

// ─── AS Path ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AsPathSegment {
    /// Type 1: unordered set
    Set(Vec<u32>),
    /// Type 2: ordered sequence
    Sequence(Vec<u32>),
    /// Type 3: confederation sequence
    ConfedSequence(Vec<u32>),
    /// Type 4: confederation set
    ConfedSet(Vec<u32>),
}

impl AsPathSegment {
    pub fn asns(&self) -> &[u32] {
        match self {
            Self::Set(v) | Self::Sequence(v) | Self::ConfedSequence(v) | Self::ConfedSet(v) => {
                v.as_slice()
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AsPath(pub Vec<AsPathSegment>);

impl AsPath {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Total AS hop count (counting only SEQUENCE segments)
    pub fn hop_count(&self) -> usize {
        self.0
            .iter()
            .map(|seg| match seg {
                AsPathSegment::Sequence(v) => v.len(),
                _ => 0,
            })
            .sum()
    }

    /// Originating ASN: right-most ASN in the first SEQUENCE segment (the AS that originated the route)
    pub fn origin_asn(&self) -> Option<u32> {
        self.0.iter().find_map(|seg| match seg {
            AsPathSegment::Sequence(v) => v.last().copied(),
            _ => None,
        })
    }

    /// First ASN: left-most ASN in the first SEQUENCE segment (the direct advertising neighbor)
    pub fn first_asn(&self) -> Option<u32> {
        self.0.iter().find_map(|seg| match seg {
            AsPathSegment::Sequence(v) => v.first().copied(),
            _ => None,
        })
    }

    /// Detect prepending: same ASN repeated consecutively
    pub fn has_prepending(&self) -> bool {
        for seg in &self.0 {
            if let AsPathSegment::Sequence(v) = seg {
                let mut prev = None;
                for &asn in v {
                    if Some(asn) == prev {
                        return true;
                    }
                    prev = Some(asn);
                }
            }
        }
        false
    }

    /// Check for AS path loop (ASN appearing more than once across all segments)
    pub fn has_loop(&self) -> bool {
        let mut seen = std::collections::HashSet::new();
        for seg in &self.0 {
            for &asn in seg.asns() {
                if !seen.insert(asn) {
                    return true;
                }
            }
        }
        false
    }
}

impl fmt::Display for AsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self
            .0
            .iter()
            .map(|seg| match seg {
                AsPathSegment::Sequence(v) => v
                    .iter()
                    .map(|a| a.to_string())
                    .collect::<Vec<_>>()
                    .join(" "),
                AsPathSegment::Set(v) => format!(
                    "{{{}}}",
                    v.iter()
                        .map(|a| a.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                AsPathSegment::ConfedSequence(v) => format!(
                    "({})",
                    v.iter()
                        .map(|a| a.to_string())
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
                AsPathSegment::ConfedSet(v) => format!(
                    "[{}]",
                    v.iter()
                        .map(|a| a.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            })
            .collect();
        write!(f, "{}", parts.join(" "))
    }
}

// ─── Communities ──────────────────────────────────────────────────────────────

/// Standard BGP community (RFC 1997): encoded as 32 bits (ASN:value)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StandardCommunity(pub u32);

impl StandardCommunity {
    pub fn new(asn: u16, value: u16) -> Self {
        Self(((asn as u32) << 16) | value as u32)
    }
    pub fn asn(self) -> u16 {
        (self.0 >> 16) as u16
    }
    pub fn value(self) -> u16 {
        (self.0 & 0xFFFF) as u16
    }

    // Well-known communities (RFC 1997 + RFC 8642)
    pub fn no_export() -> Self {
        Self(0xFFFFFF01)
    }
    pub fn no_advertise() -> Self {
        Self(0xFFFFFF02)
    }
    pub fn no_export_subconfed() -> Self {
        Self(0xFFFFFF03)
    }
    pub fn blackhole() -> Self {
        Self(0xFFFF029A)
    }
    pub fn graceful_shutdown() -> Self {
        Self(0xFFFF0000)
    }
    pub fn accept_own() -> Self {
        Self(0xFFFF0001)
    }

    pub fn is_well_known(self) -> bool {
        self.0 >= 0xFFFF0000
    }

    pub fn name(self) -> Option<&'static str> {
        match self.0 {
            0xFFFFFF01 => Some("NO_EXPORT"),
            0xFFFFFF02 => Some("NO_ADVERTISE"),
            0xFFFFFF03 => Some("NO_EXPORT_SUBCONFED"),
            0xFFFF029A => Some("BLACKHOLE"),
            0xFFFF0000 => Some("GRACEFUL_SHUTDOWN"),
            0xFFFF0001 => Some("ACCEPT_OWN"),
            _ => None,
        }
    }
}

impl fmt::Display for StandardCommunity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(name) = self.name() {
            write!(f, "{}", name)
        } else {
            write!(f, "{}:{}", self.asn(), self.value())
        }
    }
}

/// Extended community (RFC 4360): 8 bytes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExtendedCommunity {
    pub type_high: u8,
    pub type_low: u8,
    pub value: [u8; 6],
}

impl ExtendedCommunity {
    pub fn from_bytes(b: &[u8; 8]) -> Self {
        let mut value = [0u8; 6];
        value.copy_from_slice(&b[2..]);
        Self {
            type_high: b[0],
            type_low: b[1],
            value,
        }
    }
    pub fn is_transitive(&self) -> bool {
        self.type_high & 0x40 == 0
    }
}

impl ExtendedCommunity {
    /// Human-readable kind tag used in structured output.
    pub fn kind(&self) -> &'static str {
        match (self.type_high & 0x3F, self.type_low) {
            (0x00, 0x02) | (0x02, 0x02) => "route-target",
            (0x01, 0x02) => "route-target",
            (0x00, 0x03) | (0x02, 0x03) => "route-origin-soo",
            (0x01, 0x03) => "route-origin-soo",
            (0x03, 0x0B) => "sr-te-color",
            (0x41, 0x0C) | (0x01, 0x0C) => "vxlan-vni",
            _ => "extended-community",
        }
    }
}

impl fmt::Display for ExtendedCommunity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.type_high & 0x3F, self.type_low) {
            // Route Target — AS-specific (type 0x00/0x02, sub 0x02)
            (0x00, 0x02) | (0x02, 0x02) => {
                let admin = u16::from_be_bytes([self.value[0], self.value[1]]);
                let assigned = u32::from_be_bytes([
                    self.value[2],
                    self.value[3],
                    self.value[4],
                    self.value[5],
                ]);
                write!(f, "rt:{}:{}", admin, assigned)
            }
            // Route Target — IPv4-specific (type 0x01, sub 0x02)
            (0x01, 0x02) => {
                let admin =
                    Ipv4Addr::from([self.value[0], self.value[1], self.value[2], self.value[3]]);
                let assigned = u16::from_be_bytes([self.value[4], self.value[5]]);
                write!(f, "rt:{}:{}", admin, assigned)
            }
            // Site-of-Origin (SOO) — AS-specific (type 0x00/0x02, sub 0x03)
            (0x00, 0x03) | (0x02, 0x03) => {
                let admin = u16::from_be_bytes([self.value[0], self.value[1]]);
                let assigned = u32::from_be_bytes([
                    self.value[2],
                    self.value[3],
                    self.value[4],
                    self.value[5],
                ]);
                write!(f, "soo:{}:{}", admin, assigned)
            }
            // Site-of-Origin — IPv4-specific (type 0x01, sub 0x03)
            (0x01, 0x03) => {
                let admin =
                    Ipv4Addr::from([self.value[0], self.value[1], self.value[2], self.value[3]]);
                let assigned = u16::from_be_bytes([self.value[4], self.value[5]]);
                write!(f, "soo:{}:{}", admin, assigned)
            }
            // SR-TE Policy Color (type 0x03, sub 0x0B) — RFC 9012 / draft-ietf-idr-segment-routing-te-policy
            (0x03, 0x0B) => {
                let color = u32::from_be_bytes([
                    self.value[2],
                    self.value[3],
                    self.value[4],
                    self.value[5],
                ]);
                write!(f, "color:{}", color)
            }
            // VXLAN VNI (type 0x81/0x01, sub 0x0C)
            (0x41, 0x0C) | (0x01, 0x0C) => {
                let vni = u32::from_be_bytes([0, self.value[3], self.value[4], self.value[5]]);
                write!(f, "vni:{}", vni)
            }
            _ => write!(
                f,
                "ext:0x{:02x}{:02x}:{}",
                self.type_high,
                self.type_low,
                hex::encode(self.value)
            ),
        }
    }
}

/// Large community (RFC 8092): 12 bytes (3×u32)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LargeCommunity {
    pub global_admin: u32,
    pub local_data_1: u32,
    pub local_data_2: u32,
}

impl fmt::Display for LargeCommunity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}",
            self.global_admin, self.local_data_1, self.local_data_2
        )
    }
}

// ─── BGP ORIGIN ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    Igp = 0,
    Egp = 1,
    Incomplete = 2,
}

impl TryFrom<u8> for Origin {
    type Error = crate::Error;
    fn try_from(v: u8) -> crate::Result<Self> {
        match v {
            0 => Ok(Self::Igp),
            1 => Ok(Self::Egp),
            2 => Ok(Self::Incomplete),
            _ => Err(crate::Error::BgpParse(format!("invalid ORIGIN value {v}"))),
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Igp => write!(f, "IGP"),
            Self::Egp => write!(f, "EGP"),
            Self::Incomplete => write!(f, "INCOMPLETE"),
        }
    }
}

// ─── BGP Capabilities (RFC 5492) ─────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BgpCapability {
    Multiprotocol(AfiSafi),
    RouteRefresh,
    ExtendedNextHop {
        afi: Afi,
        safi: Safi,
        next_hop_afi: Afi,
    },
    ExtendedMessage,
    /// RFC 9234: role values 0=Provider, 1=RS, 2=RS-Client, 3=Customer, 4=Peer
    BgpRole(u8),
    GracefulRestart {
        restart_time: u16,
        afi_safis: Vec<(AfiSafi, u8)>,
    },
    FourByteAsn(u32),
    AddPath(Vec<(AfiSafi, u8)>), // u8 = send(1), recv(2), both(3)
    EnhancedRouteRefresh,
    /// RFC 9494: carries per-AFI/SAFI stale-time entries
    /// Each entry: AFI(2)+SAFI(1)+flags(1)+stale_time_secs(3) = 7 bytes
    LongLivedGracefulRestart {
        entries: Vec<u8>,
    },
    Fqdn {
        hostname: String,
        domain: String,
    },
    Unknown {
        code: u8,
        data: Vec<u8>,
    },
}

impl BgpCapability {
    pub fn code(&self) -> u8 {
        match self {
            Self::Multiprotocol(_) => 1,
            Self::RouteRefresh => 2,
            Self::ExtendedNextHop { .. } => 5,
            Self::ExtendedMessage => 6,
            Self::BgpRole(_) => 9,
            Self::GracefulRestart { .. } => 64,
            Self::FourByteAsn(_) => 65,
            Self::AddPath(_) => 69,
            Self::EnhancedRouteRefresh => 70,
            Self::LongLivedGracefulRestart { .. } => 71,
            Self::Fqdn { .. } => 73,
            Self::Unknown { code, .. } => *code,
        }
    }
}

// ─── EVPN NLRI wrappers ───────────────────────────────────────────────────────

/// EVPN NLRI carried in MP_REACH (AFI=25, SAFI=70)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvpnReachNlri {
    pub next_hops: Vec<IpAddr>,
    pub routes: Vec<EvpnRoute>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvpnUnreachNlri {
    pub routes: Vec<EvpnRoute>,
}

// ─── BGP Prefix-SID attribute (RFC 8669, type 40) ────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefixSid {
    /// TLV type 1: label index (RFC 8669 §3.1)
    pub label_index: Option<u32>,
    /// TLV type 3: originator SRGB — (flags, [(base, range)])
    pub originator_srgb: Option<(u16, Vec<(u32, u32)>)>,
    /// TLV type 5: SRv6 L3 Service
    pub srv6_l3_service: Option<Srv6L3Service>,
    /// Unrecognized TLVs preserved as (type, bytes)
    pub raw_tlvs: Vec<(u8, Vec<u8>)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Srv6L3Service {
    pub sub_sub_tlvs: Vec<Srv6SubSubTlv>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Srv6SubSubTlv {
    pub sid: [u8; 16],
    pub sid_flags: u8,
    pub endpoint_behavior: u16,
}

// ─── Tunnel Encapsulation attribute (RFC 9012, type 23) ──────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelEncapEntry {
    pub tunnel_type: u16,
    pub tunnel_type_name: String,
    pub endpoint: Option<IpAddr>,
    pub color: Option<u32>,
}

pub fn tunnel_type_name(t: u16) -> &'static str {
    match t {
        1 => "l2tpv3-over-ip",
        2 => "gre",
        3 => "transmit-tunnel-endpoint",
        4 => "ipsec-in-tunnel-mode",
        5 => "ip-in-ip-with-ipsec",
        6 => "mpls-in-ip-with-ipsec",
        7 => "ip-in-ip",
        8 => "vxlan",
        9 => "nvgre",
        10 => "mpls",
        11 => "mpls-in-gre",
        12 => "vxlan-gpe",
        13 => "mpls-in-udp",
        14 => "ipv6-tunnel",
        15 => "sr-mpls",
        16 => "geneve",
        17 => "endpoint",
        23 => "srv6",
        _ => "unknown",
    }
}

// ─── Path Attributes ──────────────────────────────────────────────────────────

/// A raw (unparsed) path attribute — preserved for unknown types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawAttribute {
    pub flags: u8,
    pub type_code: u8,
    pub value: Vec<u8>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PathAttributes {
    pub origin: Option<Origin>,
    pub as_path: Option<AsPath>,
    pub next_hop: Option<IpAddr>,
    pub multi_exit_disc: Option<u32>,
    pub local_pref: Option<u32>,
    pub atomic_aggregate: bool,
    pub aggregator: Option<(u32, Ipv4Addr)>,
    pub communities: Vec<StandardCommunity>,
    pub extended_communities: Vec<ExtendedCommunity>,
    pub large_communities: Vec<LargeCommunity>,
    pub originator_id: Option<Ipv4Addr>,
    pub cluster_list: Vec<Ipv4Addr>,
    pub mp_reach: Option<MpReachNlri>,
    pub mp_unreach: Option<MpUnreachNlri>,
    pub as4_path: Option<AsPath>,
    pub as4_aggregator: Option<(u32, Ipv4Addr)>,
    // RFC 7432: EVPN NLRI (AFI=25, SAFI=70)
    pub evpn_reach: Option<EvpnReachNlri>,
    pub evpn_unreach: Option<EvpnUnreachNlri>,
    // RFC 5575/8955: Flowspec NLRI
    pub flowspec_reach: Option<Vec<FlowspecNlri>>,
    pub flowspec_unreach: Option<Vec<FlowspecNlri>>,
    // RFC 8669: BGP Prefix-SID (type 40)
    pub prefix_sid: Option<PrefixSid>,
    // RFC 9012: Tunnel Encapsulation (type 23)
    pub tunnel_encap: Option<Vec<TunnelEncapEntry>>,
    // RFC 9234: Only-to-Customer (type 35)
    pub only_to_customer: Option<u32>,
    // RFC 7752: BGP-LS NLRI (AFI=16388, SAFI=71)
    pub bgpls_reach: Option<BgpLsReachNlri>,
    pub bgpls_unreach: Option<BgpLsUnreachNlri>,
    // RV3-2: BGP-LS path attribute (type 29) — topology details
    pub bgpls_attr: Option<super::bgpls::BgpLsAttribute>,
    // RV3-1: SR Policy NLRI (AFI=1/2, SAFI=73)
    pub sr_policy_nlris: Vec<super::srpolicy::SrPolicyNlri>,
    pub sr_policy_paths: Vec<super::srpolicy::CandidatePath>,
    // RV3-1: Route Target Constraint NLRI (AFI=1/2, SAFI=132)
    pub rtc_nlris: Vec<super::srpolicy::RtcNlri>,
    // RV6-2: BGPsec_Path attribute (type 30, RFC 8205) — parse-only
    pub bgpsec_path: Option<super::attributes::BgpsecPath>,
    // RV7-P1: Path Status TLV (draft-ietf-grow-bmp-path-marking-tlv-05)
    pub path_status: Option<crate::bmp::PathStatusTlv>,
    pub unknown: Vec<RawAttribute>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MpReachNlri {
    pub afi_safi: AfiSafi,
    pub next_hops: Vec<IpAddr>,
    pub prefixes: Vec<Prefix>,
    #[serde(default)]
    pub path_ids: Vec<Option<u32>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MpUnreachNlri {
    pub afi_safi: AfiSafi,
    pub prefixes: Vec<Prefix>,
    #[serde(default)]
    pub path_ids: Vec<Option<u32>>,
}

// ─── BGP-LS NLRI (RFC 7752) ───────────────────────────────────────────────────

/// BGP-LS NLRI type codes (RFC 7752 §3.2 + RFC 9514 §2)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BgpLsNlriType {
    Node,
    Link,
    Ipv4Prefix,
    Ipv6Prefix,
    /// SRv6 SID NLRI (RFC 9514, AFI=16388 SAFI=72)
    Srv6Sid,
    Unknown(u16),
}

impl From<u16> for BgpLsNlriType {
    fn from(v: u16) -> Self {
        match v {
            1 => Self::Node,
            2 => Self::Link,
            3 => Self::Ipv4Prefix,
            4 => Self::Ipv6Prefix,
            6 => Self::Srv6Sid,
            _ => Self::Unknown(v),
        }
    }
}

/// Protocol-ID values (RFC 7752 §3.2 Table 2)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BgpLsProtocol {
    IsIsLevel1,
    IsIsLevel2,
    Ospfv2,
    Direct,
    StaticConfig,
    Ospfv3,
    Unknown(u8),
}

impl From<u8> for BgpLsProtocol {
    fn from(v: u8) -> Self {
        match v {
            1 => Self::IsIsLevel1,
            2 => Self::IsIsLevel2,
            3 => Self::Ospfv2,
            4 => Self::Direct,
            5 => Self::StaticConfig,
            6 => Self::Ospfv3,
            _ => Self::Unknown(v),
        }
    }
}

/// IGP router-id descriptor (flex encoding: 4/6/7/8 bytes)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterId {
    pub bytes: Vec<u8>,
}

impl fmt::Display for RouterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.bytes.len() {
            4 => write!(
                f,
                "{}.{}.{}.{}",
                self.bytes[0], self.bytes[1], self.bytes[2], self.bytes[3]
            ),
            _ => write!(
                f,
                "{}",
                self.bytes
                    .iter()
                    .map(|b| format!("{:02x}", b))
                    .collect::<Vec<_>>()
                    .join(":")
            ),
        }
    }
}

/// Node descriptor TLVs (RFC 7752 §3.2.1)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeDescriptor {
    pub asn: Option<u32>,
    pub bgp_ls_id: Option<u32>,
    pub ospf_area: Option<u32>,
    pub igp_router: Option<RouterId>,
}

/// Link descriptor TLVs (RFC 7752 §3.2.2)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LinkDescriptor {
    pub local_addr: Option<IpAddr>,
    pub remote_addr: Option<IpAddr>,
    pub local_id: Option<u32>,
    pub remote_id: Option<u32>,
}

/// A single decoded BGP-LS NLRI item
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BgpLsNlri {
    Node {
        protocol: BgpLsProtocol,
        identifier: u64,
        local_node: NodeDescriptor,
    },
    Link {
        protocol: BgpLsProtocol,
        identifier: u64,
        local_node: NodeDescriptor,
        remote_node: NodeDescriptor,
        link: LinkDescriptor,
    },
    Prefix {
        protocol: BgpLsProtocol,
        identifier: u64,
        local_node: NodeDescriptor,
        prefix: Prefix,
    },
    /// SRv6 SID NLRI (RFC 9514, AFI=16388 SAFI=72)
    Srv6Sid(Srv6SidNlri),
    Unknown {
        nlri_type: u16,
        data: Vec<u8>,
    },
}

/// SRv6 SID NLRI (RFC 9514 §2).
/// Carries a 128-bit SRv6 SID with its endpoint behavior and SID structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Srv6SidNlri {
    pub protocol_id: u8,
    pub identifier: u64,
    pub local_node: NodeDescriptor,
    /// The 128-bit SRv6 SID value
    pub srv6_sid: [u8; 16],
    /// Endpoint behavior code (RFC 8986)
    pub endpoint_behavior: Option<u16>,
    /// SID structure TLV (locator_block_len, locator_node_len, function_len, argument_len)
    pub sid_structure: Option<(u8, u8, u8, u8)>,
}

impl Srv6SidNlri {
    /// Human-readable endpoint behavior name per RFC 8986 §7
    pub fn behavior_name(b: u16) -> &'static str {
        match b {
            1 => "End",
            2 => "End.X",
            3 => "End.T",
            4 => "End.DX6",
            5 => "End.DX4",
            6 => "End.DT6",
            7 => "End.DT46",
            8 => "End.DT4",
            9 => "End.B6.Encaps",
            10 => "End.BM",
            0x41 => "End.X (PSP)",
            0x48 => "End.OP",
            0x49 => "End.Otp",
            _ => "Unknown",
        }
    }

    /// Format the 128-bit SID as a colon-separated hex string
    pub fn sid_string(&self) -> String {
        self.srv6_sid
            .chunks(2)
            .map(|c| format!("{:02x}{:02x}", c[0], c[1]))
            .collect::<Vec<_>>()
            .join(":")
    }
}

/// VPLS NLRI (RFC 4761 §3.2.2, AFI=25 SAFI=65).
/// Carries Layer-2 VPN signaling information for VPLS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VplsNlri {
    /// Route Distinguisher (8 bytes)
    pub rd: [u8; 8],
    /// Virtual Edge ID
    pub ve_id: u16,
    /// VE Block Offset
    pub ve_block_offset: u16,
    /// VE Block Size
    pub ve_block_size: u16,
    /// MPLS label base (20-bit label in 3 bytes)
    pub label_base: u32,
}

/// BGP-LS reachability NLRI carried in MP_REACH (AFI=16388, SAFI=71)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BgpLsReachNlri {
    pub next_hop: Option<IpAddr>,
    pub nlris: Vec<BgpLsNlri>,
}

/// BGP-LS withdrawal NLRI carried in MP_UNREACH (AFI=16388, SAFI=71)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BgpLsUnreachNlri {
    pub nlris: Vec<BgpLsNlri>,
}

// ─── BGP UPDATE ───────────────────────────────────────────────────────────────

/// Session-derived decoding context for BGP UPDATE and PathAttribute parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BgpDecodeContext {
    /// True when AS_PATH on the wire uses 4-octet ASNs (RFC 6793 / RFC 7854 §4.2 A-bit = 0).
    pub four_byte_asn: bool,
    /// Address families for which BGP Add-Path (RFC 7911) is active on this session/view.
    pub add_path_families: std::collections::HashSet<AfiSafi>,
}

impl Default for BgpDecodeContext {
    fn default() -> Self {
        Self {
            four_byte_asn: true,
            add_path_families: std::collections::HashSet::new(),
        }
    }
}

impl BgpDecodeContext {
    pub fn new(four_byte_asn: bool) -> Self {
        Self {
            four_byte_asn,
            add_path_families: std::collections::HashSet::new(),
        }
    }

    pub fn with_four_byte_asn(mut self, four_byte_asn: bool) -> Self {
        self.four_byte_asn = four_byte_asn;
        self
    }

    pub fn with_add_path_ipv4(add_path_ipv4: bool) -> Self {
        let mut ctx = Self::default();
        if add_path_ipv4 {
            ctx.add_path_families.insert(AfiSafi::ipv4_unicast());
        }
        ctx
    }

    pub fn with_add_path_family(mut self, afi_safi: AfiSafi) -> Self {
        self.add_path_families.insert(afi_safi);
        self
    }

    pub fn from_capabilities(four_byte_asn: bool, capabilities: &[BgpCapability]) -> Self {
        let mut add_path_families = std::collections::HashSet::new();
        let mut cap_four_byte = false;
        for cap in capabilities {
            match cap {
                BgpCapability::FourByteAsn(_) => cap_four_byte = true,
                BgpCapability::AddPath(entries) => {
                    for &(afi_safi, mode) in entries {
                        // RFC 7911 §5: mode 3 indicates both send & receive enabled
                        if mode == 3 {
                            add_path_families.insert(afi_safi);
                        }
                    }
                }
                _ => {}
            }
        }
        Self {
            four_byte_asn: four_byte_asn || cap_four_byte,
            add_path_families,
        }
    }

    /// Negotiate Add-Path per RFC 7911 Section 5 across both OPEN message capability sets.
    pub fn from_negotiated_open(
        four_byte_asn: bool,
        sent_caps: &[BgpCapability],
        recv_caps: &[BgpCapability],
    ) -> Self {
        let mut add_path_families = std::collections::HashSet::new();
        let mut cap_four_byte = false;

        for cap in sent_caps.iter().chain(recv_caps.iter()) {
            if let BgpCapability::FourByteAsn(_) = cap {
                cap_four_byte = true;
            }
        }

        // RFC 7911 §5:
        // A speaker can only send Add-Path if it supports Send (mode 2 or 3) AND peer supports Receive (mode 1 or 3).
        // For inbound routes (Adj-RIB-In): peer is sender (recv_mode & 2 != 0), local is receiver (sent_mode & 1 != 0).
        // For outbound routes (Adj-RIB-Out): local is sender (sent_mode & 2 != 0), peer is receiver (recv_mode & 1 != 0).
        for sent_cap in sent_caps {
            if let BgpCapability::AddPath(sent_entries) = sent_cap {
                for &(sent_af, sent_mode) in sent_entries {
                    for recv_cap in recv_caps {
                        if let BgpCapability::AddPath(recv_entries) = recv_cap {
                            for &(recv_af, recv_mode) in recv_entries {
                                if sent_af == recv_af {
                                    let inbound_active =
                                        (sent_mode & 1 != 0) && (recv_mode & 2 != 0);
                                    let outbound_active =
                                        (sent_mode & 2 != 0) && (recv_mode & 1 != 0);
                                    if inbound_active || outbound_active {
                                        add_path_families.insert(sent_af);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Self {
            four_byte_asn: four_byte_asn || cap_four_byte,
            add_path_families,
        }
    }

    pub fn has_add_path(&self, afi_safi: AfiSafi) -> bool {
        self.add_path_families.contains(&afi_safi)
    }
}

/// Parsed BGP UPDATE message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BgpUpdate {
    /// Prefixes being withdrawn (IPv4 unicast NLRI, or via MP_UNREACH)
    pub withdrawn: Vec<Prefix>,
    /// Parallel path_ids for withdrawn — Some(id) when Add-Path active (RFC 7911)
    pub withdrawn_path_ids: Vec<Option<u32>>,
    pub attributes: PathAttributes,
    /// Prefixes being announced (IPv4 unicast NLRI, or via MP_REACH)
    pub announced: Vec<Prefix>,
    /// Parallel path_ids for announced — Some(id) when Add-Path active (RFC 7911)
    pub announced_path_ids: Vec<Option<u32>>,
}

impl BgpUpdate {
    /// Returns `Some(afi_safi)` when this BGP UPDATE is a true End-of-RIB marker (RFC 4724 §2),
    /// or `None` when it carries any route announcements, withdrawals (including EVPN/FlowSpec/BGP-LS/SR-Policy),
    /// or non-empty path attributes.
    pub fn eor_afi_safi(&self) -> Option<AfiSafi> {
        if !self.withdrawn.is_empty()
            || !self.announced.is_empty()
            || self.attributes.mp_reach.is_some()
            || self.attributes.origin.is_some()
            || self.attributes.as_path.is_some()
            || self.attributes.next_hop.is_some()
            || self.attributes.evpn_reach.is_some()
            || self.attributes.evpn_unreach.is_some()
            || self.attributes.flowspec_reach.is_some()
            || self.attributes.flowspec_unreach.is_some()
            || self.attributes.bgpls_reach.is_some()
            || self.attributes.bgpls_unreach.is_some()
            || !self.attributes.sr_policy_nlris.is_empty()
            || !self.attributes.rtc_nlris.is_empty()
            || !self.attributes.unknown.is_empty()
        {
            return None;
        }
        match &self.attributes.mp_unreach {
            None => Some(AfiSafi::ipv4_unicast()),
            Some(u) if u.prefixes.is_empty() => Some(u.afi_safi),
            Some(_) => None,
        }
    }

    pub fn is_eor(&self) -> bool {
        self.eor_afi_safi().is_some()
    }

    /// Returns (prefix, path_id) pairs for all announced prefixes across IPv4 and MP_REACH.
    pub fn all_announced_with_path_id(&self) -> Vec<(&Prefix, Option<u32>)> {
        let mut out: Vec<(&Prefix, Option<u32>)> = self
            .announced
            .iter()
            .zip(
                self.announced_path_ids
                    .iter()
                    .chain(std::iter::repeat(&None)),
            )
            .map(|(p, id)| (p, *id))
            .collect();
        if let Some(mp) = &self.attributes.mp_reach {
            out.extend(
                mp.prefixes
                    .iter()
                    .zip(mp.path_ids.iter().chain(std::iter::repeat(&None)))
                    .map(|(p, id)| (p, *id)),
            );
        }
        out
    }

    /// Returns (prefix, path_id) pairs for all withdrawn prefixes across IPv4 and MP_UNREACH.
    pub fn all_withdrawn_with_path_id(&self) -> Vec<(&Prefix, Option<u32>)> {
        let mut out: Vec<(&Prefix, Option<u32>)> = self
            .withdrawn
            .iter()
            .zip(
                self.withdrawn_path_ids
                    .iter()
                    .chain(std::iter::repeat(&None)),
            )
            .map(|(p, id)| (p, *id))
            .collect();
        if let Some(mp) = &self.attributes.mp_unreach {
            out.extend(
                mp.prefixes
                    .iter()
                    .zip(mp.path_ids.iter().chain(std::iter::repeat(&None)))
                    .map(|(p, id)| (p, *id)),
            );
        }
        out
    }

    pub fn all_announced(&self) -> Vec<&Prefix> {
        let mut out: Vec<&Prefix> = self.announced.iter().collect();
        if let Some(mp) = &self.attributes.mp_reach {
            out.extend(mp.prefixes.iter());
        }
        out
    }

    pub fn all_withdrawn(&self) -> Vec<&Prefix> {
        let mut out: Vec<&Prefix> = self.withdrawn.iter().collect();
        if let Some(mp) = &self.attributes.mp_unreach {
            out.extend(mp.prefixes.iter());
        }
        out
    }
}

// ─── Need hex for Display impls ────────────────────────────────────────────────
mod hex {
    pub fn encode(b: impl AsRef<[u8]>) -> String {
        b.as_ref().iter().map(|x| format!("{:02x}", x)).collect()
    }
}
