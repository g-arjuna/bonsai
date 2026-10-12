/// RV6-1: Route context structure presented to filter scripts.
///
/// `RouteCtx` is a flat, owned representation of everything a filter
/// might need to know about a BGP route event.  It is built once per
/// route and then evaluated against the active filter engine.
///
/// When Roto embedding is active the same struct is registered with the
/// Roto runtime and passed by reference into every filtermap invocation
/// (zero serialization cost — Roto operates on a raw pointer).
///
/// For now the struct is also used by our pest-based fallback evaluator.
use std::collections::HashSet;
use std::net::IpAddr;
use serde::{Deserialize, Serialize};
use rbmp_core::bgp::types::{PathAttributes, Prefix};
use rbmp_core::bmp::types::RibType;

/// Flat route context — built once per BMP route event, evaluated by filters.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RouteCtx {
    // ── Prefix ────────────────────────────────────────────────────────────────
    pub prefix:       String,   // "203.0.113.0/24"
    pub prefix_len:   u8,
    pub afi:          String,   // "ipv4" | "ipv6"

    // ── Session context ───────────────────────────────────────────────────────
    pub peer_as:      u32,
    pub peer_addr:    String,
    pub rib_type:     String,   // "pre-policy" | "post-policy" | "loc-rib"
    pub action:       String,   // "announce" | "withdraw"

    // ── BGP attributes ────────────────────────────────────────────────────────
    pub as_path:      String,   // space-separated ASN list
    pub as_path_len:  u32,
    pub origin_asn:   u32,
    pub has_prepend:  bool,
    pub next_hop:     String,
    pub local_pref:   u32,      // 0 when absent
    pub med:          u32,
    pub origin_attr:  String,   // "igp" | "egp" | "incomplete"

    // ── Communities ───────────────────────────────────────────────────────────
    pub community_set: HashSet<String>,    // "64512:100"
    pub ext_community_set: HashSet<String>,
    pub large_community_set: HashSet<String>,

    // ── Security ──────────────────────────────────────────────────────────────
    pub rpki:         String,   // "valid" | "invalid" | "not-found" | "unknown"
    pub aspa:         String,   // "valid" | "invalid" | "unknown" (RV6-2)
    pub otc_asn:      u32,      // 0 when OTC attr absent

    // ── SR/EVPN context ───────────────────────────────────────────────────────
    pub is_evpn:      bool,
    pub is_bgpls:     bool,
    pub is_srpolicy:  bool,
    pub is_unnumbered: bool,    // RFC 5549 IPv6 link-local NH for IPv4 NLRI
    pub evpn_type:    u8,       // 0 when not EVPN
}

impl RouteCtx {
    /// Build from BMP route event data.
    pub fn from_bmp(
        prefix:    &Prefix,
        peer_as:   u32,
        peer_addr: IpAddr,
        rib_type:  RibType,
        action:    &str,
        attrs:     &PathAttributes,
        rpki:      &str,
    ) -> Self {
        let prefix_str = prefix.to_string();
        let prefix_len = match prefix {
            Prefix::V4(n)                    => n.prefix_len(),
            Prefix::V6(n)                    => n.prefix_len(),
            Prefix::Labeled { prefix, .. }   => match prefix.as_ref() {
                Prefix::V4(n) => n.prefix_len(),
                Prefix::V6(n) => n.prefix_len(),
                _ => 0,
            },
            Prefix::Vpn { prefix, .. } => match prefix.as_ref() {
                Prefix::V4(n) => n.prefix_len(),
                Prefix::V6(n) => n.prefix_len(),
                _ => 0,
            },
        };
        let afi = match prefix {
            Prefix::V6(_) => "ipv6",
            _ => "ipv4",
        }.to_string();

        let as_path_str = attrs.as_path.as_ref()
            .map(|p| p.to_string())
            .unwrap_or_default();
        let asns: Vec<u32> = as_path_str.split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        let origin_asn   = asns.last().copied().unwrap_or(0);
        let has_prepend  = asns.windows(2).any(|w| w[0] == w[1]);

        let community_set: HashSet<String> = attrs.communities.iter()
            .map(|c| format!("{}:{}", c.asn(), c.value()))
            .collect();
        let ext_community_set: HashSet<String> = attrs.extended_communities.iter()
            .map(|c| format!("{c:?}"))
            .collect();
        let large_community_set: HashSet<String> = attrs.large_communities.iter()
            .map(|c| format!("{c:?}"))
            .collect();

        let rib_type_str = match rib_type {
            RibType::AdjRibInPrePolicy  => "pre-policy",
            RibType::AdjRibInPostPolicy => "post-policy",
            RibType::LocRib             => "loc-rib",
            RibType::LocRibFiltered     => "loc-rib-filtered",
            RibType::AdjRibOutPrePolicy  => "adj-out-pre",
            RibType::AdjRibOutPostPolicy => "adj-out-post",
        }.to_string();

        // RFC 5549: IPv6 next-hop for IPv4 prefix = BGP unnumbered
        let is_unnumbered = matches!(prefix, Prefix::V4(_))
            && attrs.mp_reach.as_ref()
                .and_then(|mp| mp.next_hops.first())
                .map(|nh| matches!(nh, std::net::IpAddr::V6(_)))
                .unwrap_or(false);

        let evpn_type = attrs.evpn_reach.as_ref()
            .and_then(|e| e.routes.first())
            .map(|r| r.route_type_code())
            .unwrap_or(0);

        Self {
            prefix:       prefix_str,
            prefix_len,
            afi,
            peer_as,
            peer_addr:    peer_addr.to_string(),
            rib_type:     rib_type_str,
            action:       action.to_string(),
            as_path:      as_path_str,
            as_path_len:  asns.len() as u32,
            origin_asn,
            has_prepend,
            next_hop:     attrs.next_hop.map(|h| h.to_string()).unwrap_or_default(),
            local_pref:   attrs.local_pref.unwrap_or(0),
            med:          attrs.multi_exit_disc.unwrap_or(0),
            origin_attr:  attrs.origin.as_ref().map(|o| format!("{o:?}").to_lowercase()).unwrap_or_default(),
            community_set,
            ext_community_set,
            large_community_set,
            rpki:         rpki.to_string(),
            aspa:         "unknown".to_string(),
            otc_asn:      attrs.only_to_customer.unwrap_or(0),
            is_evpn:      attrs.evpn_reach.is_some(),
            is_bgpls:     attrs.bgpls_reach.is_some(),
            is_srpolicy:  !attrs.sr_policy_nlris.is_empty(),
            is_unnumbered,
            evpn_type,
        }
    }

    /// Check if a standard community string (e.g. "65512:100") is present.
    pub fn community_has(&self, c: &str) -> bool {
        self.community_set.contains(c)
    }

    /// Check if an ASN appears anywhere in the AS_PATH.
    pub fn as_path_contains(&self, asn: u32) -> bool {
        self.as_path.split_whitespace()
            .any(|s| s.parse::<u32>().ok() == Some(asn))
    }

    /// Check if the route prefix falls within the given CIDR range.
    pub fn prefix_in_range(&self, cidr: &str) -> bool {
        use ipnet::IpNet;
        let Ok(range) = cidr.parse::<IpNet>() else { return false; };
        let Ok(me)    = self.prefix.parse::<IpNet>() else { return false; };
        // me is a subnet of range if range contains me's network address
        match (range, me) {
            (IpNet::V4(r), IpNet::V4(m)) =>
                r.contains(&m.network()) && m.prefix_len() >= r.prefix_len(),
            (IpNet::V6(r), IpNet::V6(m)) =>
                r.contains(&m.network()) && m.prefix_len() >= r.prefix_len(),
            _ => false,
        }
    }
}
