use std::collections::HashSet;
use std::net::IpAddr;
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use rbmp_core::bgp::types::{PathAttributes, Prefix};
use crate::filter_expr::{Expr, RouteCtx, parse_expr};

// ─── YAML DSL types ──────────────────────────────────────────────────────────

/// A single programmable route filter loaded from YAML.
///
/// All populated fields are ANDed together — a route must match every
/// specified criterion to be considered a match.
///
/// Example YAML:
/// ```yaml
/// name: block-bogons
/// action: deny
/// prefix_list:
///   - 10.0.0.0/8
///   - 192.168.0.0/16
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteFilter {
    /// Human-readable label for logging
    pub name:           String,
    /// What to do when the route matches
    pub action:         FilterAction,
    /// Match if route prefix falls within any of these networks (CIDR)
    #[serde(default)]
    pub prefix_list:    Vec<String>,
    /// Match if announcing peer AS is in this list
    #[serde(default)]
    pub peer_as_list:   Vec<u32>,
    /// Match if any standard community string (e.g. "65000:100") is present
    #[serde(default)]
    pub community_list: Vec<String>,
    /// Match if announcing peer address is in this list
    #[serde(default)]
    pub peer_addr_list: Vec<String>,
    /// Match if AS_PATH contains this exact AS (anywhere)
    #[serde(default)]
    pub as_path_contains: Vec<u32>,
    /// Match if LOCAL_PREF >= this value
    pub local_pref_min: Option<u32>,
    /// Match if LOCAL_PREF <= this value
    pub local_pref_max: Option<u32>,
    /// Match if MED <= this value
    pub med_max:        Option<u32>,
    /// Optional predicate expression (PEG-parsed). When present, this
    /// expression is evaluated IN ADDITION TO the field-level criteria above.
    /// Example: `"rpki == 'invalid' AND prefix_len > 24"`
    #[serde(default)]
    pub expr:           Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilterAction {
    /// Accept — keep the route and forward the RibEvent
    Accept,
    /// Deny — drop the route silently
    Deny,
    /// Tag — accept but annotate; useful for policy tagging pipelines
    Tag,
}

/// Outcome of applying a filter set to a route
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterVerdict {
    Accept,
    Deny,
    /// No filter matched — default-accept
    Default,
}

// ─── Compiled filter ─────────────────────────────────────────────────────────

/// Pre-parsed version of a `RouteFilter` with compiled prefix networks.
struct CompiledFilter {
    raw:       RouteFilter,
    nets:      Vec<IpNet>,
    peer_nets: Vec<IpAddr>,
    /// Compiled expression AST (from `raw.expr`, if present)
    compiled_expr: Option<Expr>,
}

impl CompiledFilter {
    fn compile(raw: RouteFilter) -> Self {
        let nets = raw.prefix_list.iter()
            .filter_map(|s| s.parse::<IpNet>().ok())
            .collect();
        let peer_nets = raw.peer_addr_list.iter()
            .filter_map(|s| s.parse::<IpAddr>().ok())
            .collect();
        let compiled_expr = raw.expr.as_deref().and_then(|e| {
            match parse_expr(e) {
                Ok(expr) => Some(expr),
                Err(err) => {
                    tracing::warn!(filter = %raw.name, %err, "invalid filter expr — ignored");
                    None
                }
            }
        });
        Self { raw, nets, peer_nets, compiled_expr }
    }

    /// Returns true if every populated criterion on this filter matches the route.
    fn matches(&self, prefix: &Prefix, peer_as: u32, peer_addr: IpAddr, attrs: &PathAttributes, ctx: Option<&RouteCtx>) -> bool {
        // ── Prefix list ───────────────────────────────────────────────────────
        if !self.nets.is_empty() {
            // Extract the innermost V4/V6 address regardless of label/VPN wrapping
            fn inner_addr(p: &Prefix) -> Option<std::net::IpAddr> {
                match p {
                    Prefix::V4(n) => Some(n.addr().into()),
                    Prefix::V6(n) => Some(n.addr().into()),
                    Prefix::Labeled { prefix, .. } | Prefix::Vpn { prefix, .. } => inner_addr(prefix),
                }
            }
            let Some(addr) = inner_addr(prefix) else { return false; };
            if !self.nets.iter().any(|net| net.contains(&addr)) {
                return false;
            }
        }

        // ── Peer AS ───────────────────────────────────────────────────────────
        if !self.raw.peer_as_list.is_empty() && !self.raw.peer_as_list.contains(&peer_as) {
            return false;
        }

        // ── Peer address ─────────────────────────────────────────────────────
        if !self.peer_nets.is_empty() && !self.peer_nets.contains(&peer_addr) {
            return false;
        }

        // ── Community list ────────────────────────────────────────────────────
        if !self.raw.community_list.is_empty() {
            let route_comms: Vec<String> = attrs.communities.iter()
                .map(|c| format!("{}:{}", c.asn(), c.value()))
                .collect();
            if !self.raw.community_list.iter().any(|want| route_comms.contains(want)) {
                return false;
            }
        }

        // ── AS_PATH contains ─────────────────────────────────────────────────
        if !self.raw.as_path_contains.is_empty() {
            let path_asns: Vec<u32> = attrs.as_path.as_ref()
                .map(|p| p.0.iter().flat_map(|seg| seg.asns().iter().copied()).collect())
                .unwrap_or_default();
            if !self.raw.as_path_contains.iter().all(|want| path_asns.contains(want)) {
                return false;
            }
        }

        // ── LOCAL_PREF bounds ─────────────────────────────────────────────────
        if let Some(min) = self.raw.local_pref_min {
            if attrs.local_pref.unwrap_or(0) < min { return false; }
        }
        if let Some(max) = self.raw.local_pref_max {
            if attrs.local_pref.unwrap_or(0) > max { return false; }
        }

        // ── MED max ───────────────────────────────────────────────────────────
        if let Some(max_med) = self.raw.med_max {
            if attrs.multi_exit_disc.unwrap_or(0) > max_med { return false; }
        }

        // ── Expression predicate ────────────────────────────────────────────────
        if let (Some(compiled), Some(route_ctx)) = (&self.compiled_expr, ctx) {
            if !compiled.eval(route_ctx) {
                return false;
            }
        }

        true
    }
}

// ─── RouteCtx builder ────────────────────────────────────────────────────────

/// Build a `RouteCtx` from BMP route data for expression evaluation.
pub fn build_route_ctx(
    prefix:    &Prefix,
    peer_as:   u32,
    action:    &str,
    attrs:     &PathAttributes,
    rpki:      Option<&str>,
) -> RouteCtx {
    let prefix_len = match prefix {
        Prefix::V4(n)                    => n.prefix_len(),
        Prefix::V6(n)                    => n.prefix_len(),
        Prefix::Labeled { prefix, .. }   => build_route_ctx(prefix, peer_as, action, attrs, rpki).prefix_len,
        Prefix::Vpn     { prefix, .. }   => build_route_ctx(prefix, peer_as, action, attrs, rpki).prefix_len,
    };

    let as_path_asns: Vec<u32> = attrs.as_path.as_ref()
        .map(|p| p.0.iter().flat_map(|seg| seg.asns().iter().copied()).collect())
        .unwrap_or_default();

    let origin_asn = as_path_asns.last().copied().unwrap_or(0);

    let has_prepend = as_path_asns.windows(2).any(|w| w[0] == w[1]);

    let community_set: HashSet<String> = attrs.communities.iter()
        .map(|c| format!("{}:{}", c.asn(), c.value()))
        .collect();

    RouteCtx {
        prefix_len,
        as_path_len:   as_path_asns.len(),
        origin_asn,
        has_prepend,
        rpki:          rpki.unwrap_or("unknown").to_string(),
        action:        action.to_string(),
        peer_as,
        local_pref:    attrs.local_pref,
        med:           attrs.multi_exit_disc,
        community_set,
    }
}

// ─── FilterEngine ─────────────────────────────────────────────────────────────

/// Holds an ordered list of compiled route filters.
/// Filters are evaluated in order; the first match wins (permit/deny).
#[derive(Default)]
pub struct FilterEngine {
    filters: Vec<CompiledFilter>,
}

impl FilterEngine {
    pub fn new() -> Self { Self::default() }

    /// Load filters from a YAML string (sequence of RouteFilter objects).
    pub fn load_yaml(yaml: &str) -> Result<Self, serde_yaml::Error> {
        let raw: Vec<RouteFilter> = serde_yaml::from_str(yaml)?;
        Ok(Self {
            filters: raw.into_iter().map(CompiledFilter::compile).collect(),
        })
    }

    /// Load filters from a YAML file path.
    pub fn load_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        Ok(Self::load_yaml(&content)?)
    }

    /// Return the number of loaded filters.
    pub fn len(&self) -> usize { self.filters.len() }
    pub fn is_empty(&self) -> bool { self.filters.is_empty() }

    /// Apply the filter chain to a route.  Returns the verdict and the name of
    /// the matching filter (if any).
    pub fn apply(
        &self,
        prefix:    &Prefix,
        peer_as:   u32,
        peer_addr: IpAddr,
        attrs:     &PathAttributes,
    ) -> (FilterVerdict, Option<&str>) {
        // Build RouteCtx once — only if at least one filter has an expr
        let needs_ctx = self.filters.iter().any(|f| f.compiled_expr.is_some());
        let ctx = if needs_ctx {
            Some(build_route_ctx(prefix, peer_as, "announce", attrs, None))
        } else {
            None
        };
        for cf in &self.filters {
            if cf.matches(prefix, peer_as, peer_addr, attrs, ctx.as_ref()) {
                let verdict = match cf.raw.action {
                    FilterAction::Accept | FilterAction::Tag => FilterVerdict::Accept,
                    FilterAction::Deny                       => FilterVerdict::Deny,
                };
                return (verdict, Some(&cf.raw.name));
            }
        }
        (FilterVerdict::Default, None)
    }

    /// Apply the filter chain with a caller-supplied `RouteCtx` — use this
    /// variant when the RPKI validity is already known (e.g. after VrpCache
    /// lookup) so expressions can reference `rpki == 'invalid'` accurately.
    pub fn apply_with_ctx(
        &self,
        prefix:    &Prefix,
        peer_as:   u32,
        peer_addr: IpAddr,
        attrs:     &PathAttributes,
        route_ctx: Option<&RouteCtx>,
    ) -> (FilterVerdict, Option<&str>) {
        for cf in &self.filters {
            if cf.matches(prefix, peer_as, peer_addr, attrs, route_ctx) {
                let verdict = match cf.raw.action {
                    FilterAction::Accept | FilterAction::Tag => FilterVerdict::Accept,
                    FilterAction::Deny                       => FilterVerdict::Deny,
                };
                return (verdict, Some(&cf.raw.name));
            }
        }
        (FilterVerdict::Default, None)
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rbmp_core::bgp::types::{PathAttributes, StandardCommunity};
    use ipnet::Ipv4Net;
    use std::str::FromStr;

    fn make_attrs(lp: Option<u32>, community: Option<(u16, u16)>) -> PathAttributes {
        let mut a = PathAttributes::default();
        a.local_pref = lp;
        if let Some((asn, val)) = community {
            a.communities.push(StandardCommunity::new(asn, val));
        }
        a
    }

    fn v4(s: &str) -> Prefix {
        Prefix::V4(Ipv4Net::from_str(s).unwrap())
    }

    fn peer() -> IpAddr { "10.0.0.1".parse().unwrap() }

    #[test]
    fn test_deny_bogon_prefix() {
        let yaml = r#"
- name: block-rfc1918
  action: deny
  prefix_list:
    - 10.0.0.0/8
    - 192.168.0.0/16
"#;
        let engine = FilterEngine::load_yaml(yaml).unwrap();
        assert_eq!(engine.len(), 1);

        let attrs = make_attrs(None, None);
        let (v, name) = engine.apply(&v4("10.1.2.0/24"), 65001, peer(), &attrs);
        assert_eq!(v, FilterVerdict::Deny);
        assert_eq!(name, Some("block-rfc1918"));

        // 1.2.3.0/24 is not RFC1918 — should default-accept
        let (v2, _) = engine.apply(&v4("1.2.3.0/24"), 65001, peer(), &attrs);
        assert_eq!(v2, FilterVerdict::Default);
    }

    #[test]
    fn test_accept_by_peer_as() {
        let yaml = r#"
- name: accept-transit
  action: accept
  peer_as_list:
    - 65100
"#;
        let engine = FilterEngine::load_yaml(yaml).unwrap();
        let attrs = make_attrs(None, None);

        let (v, _) = engine.apply(&v4("5.5.5.0/24"), 65100, peer(), &attrs);
        assert_eq!(v, FilterVerdict::Accept);

        let (v2, _) = engine.apply(&v4("5.5.5.0/24"), 65200, peer(), &attrs);
        assert_eq!(v2, FilterVerdict::Default);
    }

    #[test]
    fn test_filter_by_community() {
        let yaml = r#"
- name: no-export
  action: deny
  community_list:
    - "65000:100"
"#;
        let engine = FilterEngine::load_yaml(yaml).unwrap();

        let attrs_with = make_attrs(None, Some((65000, 100)));
        let (v, _) = engine.apply(&v4("8.8.8.0/24"), 65001, peer(), &attrs_with);
        assert_eq!(v, FilterVerdict::Deny);

        let attrs_without = make_attrs(None, None);
        let (v2, _) = engine.apply(&v4("8.8.8.0/24"), 65001, peer(), &attrs_without);
        assert_eq!(v2, FilterVerdict::Default);
    }

    #[test]
    fn test_local_pref_range() {
        let yaml = r#"
- name: high-pref
  action: accept
  local_pref_min: 200
"#;
        let engine = FilterEngine::load_yaml(yaml).unwrap();

        let (v, _) = engine.apply(&v4("1.0.0.0/24"), 65001, peer(), &make_attrs(Some(250), None));
        assert_eq!(v, FilterVerdict::Accept);

        let (v2, _) = engine.apply(&v4("1.0.0.0/24"), 65001, peer(), &make_attrs(Some(100), None));
        assert_eq!(v2, FilterVerdict::Default);
    }

    #[test]
    fn test_empty_engine_default_accept() {
        let engine = FilterEngine::new();
        let attrs = make_attrs(None, None);
        let (v, name) = engine.apply(&v4("0.0.0.0/0"), 1, peer(), &attrs);
        assert_eq!(v, FilterVerdict::Default);
        assert!(name.is_none());
    }

    #[test]
    fn test_filter_reject_invalid_and_too_specific() {
        let yaml = r#"
- name: rpki-invalid-too-specific
  action: deny
  expr: "prefix_len > 24"
"#;
        let engine = FilterEngine::load_yaml(yaml).unwrap();
        let attrs = make_attrs(None, None);

        // /25 — too specific — should be denied
        let (v, name) = engine.apply(&v4("203.0.113.0/25"), 65001, peer(), &attrs);
        assert_eq!(v, FilterVerdict::Deny);
        assert_eq!(name, Some("rpki-invalid-too-specific"));

        // /24 — exactly 24 — NOT > 24, should default-accept
        let (v2, _) = engine.apply(&v4("203.0.113.0/24"), 65001, peer(), &attrs);
        assert_eq!(v2, FilterVerdict::Default);
    }

    #[test]
    fn test_filter_peer_as_in_expr() {
        let yaml = r#"
- name: tag-tier1
  action: accept
  expr: "peer_as IN [701, 1299, 3356]"
"#;
        let engine = FilterEngine::load_yaml(yaml).unwrap();
        let attrs = make_attrs(None, None);

        let (v, _) = engine.apply(&v4("1.0.0.0/24"), 3356, peer(), &attrs);
        assert_eq!(v, FilterVerdict::Accept);

        let (v2, _) = engine.apply(&v4("1.0.0.0/24"), 65001, peer(), &attrs);
        assert_eq!(v2, FilterVerdict::Default);
    }

    #[test]
    fn test_bogon_10_slash_8_rejected() {
        let yaml = r#"
- name: block-bogons
  action: deny
  prefix_list:
    - 10.0.0.0/8
    - 172.16.0.0/12
    - 192.168.0.0/16
"#;
        let engine = FilterEngine::load_yaml(yaml).unwrap();
        let attrs = make_attrs(None, None);

        let (v, name) = engine.apply(&v4("10.0.0.0/8"), 65001, peer(), &attrs);
        assert_eq!(v, FilterVerdict::Deny);
        assert_eq!(name, Some("block-bogons"));
    }

    #[test]
    fn test_bogon_192_168_rejected() {
        let yaml = r#"
- name: block-bogons
  action: deny
  prefix_list:
    - 10.0.0.0/8
    - 192.168.0.0/16
"#;
        let engine = FilterEngine::load_yaml(yaml).unwrap();
        let attrs = make_attrs(None, None);

        let (v, _) = engine.apply(&v4("192.168.1.0/24"), 65001, peer(), &attrs);
        assert_eq!(v, FilterVerdict::Deny);

        // Public prefix must not match bogon filter
        let (v2, _) = engine.apply(&v4("8.8.8.0/24"), 65001, peer(), &attrs);
        assert_eq!(v2, FilterVerdict::Default);
    }

    #[test]
    fn test_filter_name_returned_on_match() {
        let yaml = r#"
- name: my-special-filter
  action: deny
  prefix_list:
    - 203.0.113.0/24
"#;
        let engine = FilterEngine::load_yaml(yaml).unwrap();
        let attrs = make_attrs(None, None);
        let (_, name) = engine.apply(&v4("203.0.113.0/24"), 65001, peer(), &attrs);
        assert_eq!(name, Some("my-special-filter"), "matched filter name must be returned");
    }

    #[test]
    fn test_as_path_contains_filter() {
        let yaml = r#"
- name: block-transit-64512
  action: deny
  as_path_contains:
    - 64512
"#;
        let engine = FilterEngine::load_yaml(yaml).unwrap();

        let mut attrs_with_asn = make_attrs(None, None);
        use rbmp_core::bgp::types::{AsPath, AsPathSegment};
        attrs_with_asn.as_path = Some(AsPath(vec![AsPathSegment::Sequence(vec![65001, 64512, 65100])]));

        let (v, _) = engine.apply(&v4("1.2.3.0/24"), 65001, peer(), &attrs_with_asn);
        assert_eq!(v, FilterVerdict::Deny, "route with AS 64512 in path must be denied");

        let attrs_clean = make_attrs(None, None);
        let (v2, _) = engine.apply(&v4("1.2.3.0/24"), 65001, peer(), &attrs_clean);
        assert_eq!(v2, FilterVerdict::Default, "route without AS 64512 must be default-accept");
    }
}
