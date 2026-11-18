//! Structured gNMI path parsing shared by configuration validation,
//! subscription planning and the runtime subscriber.
//!
//! Path elements are separated by '/' ONLY outside bracketed key
//! expressions, so keyed names containing slashes (e.g. `name=Ethernet1/1`,
//! `name=xe-0/0/0`) survive intact. Elements support `elem[k1=v1][k2=v2]`
//! key lists. Inside key expressions a backslash escapes the next
//! character (`\]`, `\[`, `\\`), so key values may contain brackets and
//! backslashes literally. Malformed brackets, empty names/keys, stray
//! brackets in element names and duplicate keys are rejected — never
//! silently repaired. Key values are preserved byte-exact (no trimming).

use std::collections::HashMap;

use crate::proto::gnmi::{Path, PathElem};

/// gNMI model origins are simple identifiers (e.g. "openconfig", "ietf",
/// "gnmi"). Anything else — including provenance labels such as
/// "override: site-override(...)" — is not a valid wire origin.
pub fn valid_origin(origin: &str) -> bool {
    !origin.is_empty()
        && origin
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Parse a configured path string into a structured gNMI Path.
/// `origin` is the gNMI model origin (e.g. "openconfig"), NOT a provenance
/// label — callers must keep provenance separate.
pub fn parse_path(origin: &str, raw_path: &str) -> Result<Path, String> {
    if !origin.is_empty() && !valid_origin(origin) {
        return Err(format!("invalid gNMI origin '{}'", origin));
    }
    let mut elem = Vec::new();
    for segment in split_path_segments(raw_path) {
        let (name, key) = parse_segment(segment)
            .ok_or_else(|| format!("malformed path segment '{}'", segment))?;
        elem.push(PathElem { name, key });
    }
    if elem.is_empty() {
        return Err("empty path".to_string());
    }
    Ok(Path {
        origin: origin.trim().to_string(),
        elem,
        ..Default::default()
    })
}

/// Canonical identity for a structured path: origin plus elements in order.
/// Element and key names/values are backslash-escaped (`\`, `,`, `=`, `[`,
/// `]`) before joining, so key values containing separator characters
/// cannot collide with a different structured path. Equivalent textual
/// spellings of the same structured path produce the same key; different
/// origins or different keyed paths never collapse together.
pub fn canonical_key(path: &Path) -> String {
    let mut out = String::new();
    out.push_str(&escape_field(&path.origin));
    for elem in &path.elem {
        out.push('/');
        out.push_str(&escape_field(&elem.name));
        if !elem.key.is_empty() {
            let mut pairs: Vec<(&String, &String)> = elem.key.iter().collect();
            pairs.sort();
            out.push('[');
            for (i, (k, v)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&escape_field(k));
                out.push('=');
                out.push_str(&escape_field(v));
            }
            out.push(']');
        }
    }
    out
}

fn escape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '\\' | ',' | '=' | '[' | ']') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Convenience: canonical dedup/identity key directly from a configured
/// (origin, raw path) pair. Returns None for malformed input.
pub fn canonical_path_key(origin: &str, raw_path: &str) -> Option<String> {
    parse_path(origin, raw_path).ok().map(|p| canonical_key(&p))
}

/// Exact structured coverage (M5.2B): does `observation` satisfy
/// `expectation`? Origins must be compatible (a non-empty expectation
/// origin requires an exact match; empty means unspecified), the
/// observation path must equal or descend from the expectation path,
/// and every expectation element must match name-exactly. An
/// expectation element without keys matches any keyed instance (gNMI
/// list wildcard semantics); a key whose expectation VALUE is exactly
/// `*` matches any value for that key (the wildcard convention the
/// built-in automatic subscriptions already emit, e.g.
/// `interface[name=*]`). Key sets must otherwise be identical — a
/// partial-key expectation never proves coverage of a differently-keyed
/// instance. String containment is never used.
pub fn observation_covers(expectation: &Path, observation: &Path) -> bool {
    if !expectation.origin.is_empty() && expectation.origin != observation.origin {
        return false;
    }
    if observation.elem.len() < expectation.elem.len() {
        return false;
    }
    expectation
        .elem
        .iter()
        .zip(observation.elem.iter())
        .all(|(e, o)| {
            e.name == o.name
                && (e.key.is_empty()
                    || (e.key.len() == o.key.len()
                        && e.key.iter().all(|(k, v)| {
                            if v == "*" {
                                o.key.contains_key(k)
                            } else {
                                o.key.get(k) == Some(v)
                            }
                        })))
        })
}

/// Conservative structured DELETE coverage (M5.2B review B9): does a
/// `Notification.delete` for `deletion` invalidate `expectation`?
///
/// A delete is evidence of absence, so matching is deliberately
/// stricter than [`observation_covers`]:
///
/// - The deleted path must be the expectation path itself or a proper
///   ANCESTOR of it — deleting a parent list instance or container
///   invalidates every descendant expectation.
/// - Every element of the deleted prefix must match the expectation
///   element NAME- and KEY-EXACTLY. Expectation wildcards (`name=*`,
///   unkeyed elements) and partially-keyed expectations are NOT covered
///   by a single concrete delete: one deleted instance never implies
///   exhaustive absence across a wildcard/subtree expectation.
/// - A delete deeper than the expectation (one instance under a broader
///   expectation) is NOT proof of absence: it must be qualified by the
///   caller, never treated as a complete subtree delete.
pub fn delete_covers(expectation: &Path, deletion: &Path) -> bool {
    if !expectation.origin.is_empty() && expectation.origin != deletion.origin {
        return false;
    }
    if deletion.elem.len() > expectation.elem.len() {
        return false;
    }
    expectation
        .elem
        .iter()
        .zip(deletion.elem.iter())
        .all(|(e, d)| e.name == d.name && e.key == d.key)
}

/// Split on '/' separators outside '[' ... ']' key expressions. An unclosed
/// '[' keeps the remainder inside the current segment so the segment parser
/// rejects it as malformed. A backslash escapes the next character.
fn split_path_segments(raw: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut escaped = false;
    for (idx, ch) in raw.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            '/' if depth == 0 => {
                segments.push(&raw[start..idx]);
                start = idx + 1;
            }
            _ => {}
        }
    }
    segments.push(&raw[start..]);
    segments.into_iter().filter(|s| !s.is_empty()).collect()
}

fn parse_segment(segment: &str) -> Option<(String, HashMap<String, String>)> {
    let first_key = segment.find('[').unwrap_or(segment.len());
    let name = &segment[..first_key];
    // Element names must not contain stray brackets; '/' was already
    // split outside key expressions.
    if name.is_empty() || name.contains(']') || name.contains('[') {
        return None;
    }

    let mut key = HashMap::new();
    let mut rest = &segment[first_key..];
    while !rest.is_empty() {
        let after_open = rest.strip_prefix('[')?;
        // Find the closing ']' that is not backslash-escaped.
        let mut close = None;
        let mut escaped = false;
        for (i, ch) in after_open.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                ']' => {
                    close = Some(i);
                    break;
                }
                _ => {}
            }
        }
        let close = close?;
        let pair = &after_open[..close];
        let (key_name, key_value) = split_key_pair(pair)?;
        if key.insert(key_name, key_value).is_some() {
            return None; // duplicate key
        }
        rest = &after_open[close + 1..];
    }

    Some((name.to_string(), key))
}

/// Split `name=value` at the first UNESCAPED '='; unescape `\x` → `x` in
/// both halves. Values are preserved exactly — no trimming.
fn split_key_pair(pair: &str) -> Option<(String, String)> {
    let mut escaped = false;
    let mut eq = None;
    for (i, ch) in pair.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '=' => {
                eq = Some(i);
                break;
            }
            _ => {}
        }
    }
    let eq = eq?;
    let name = unescape(&pair[..eq])?;
    let value = unescape(&pair[eq + 1..])?;
    if name.is_empty() || value.is_empty() {
        return None;
    }
    Some((name, value))
}

fn unescape(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            out.push(chars.next()?);
        } else {
            out.push(ch);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyed_interface_with_slash_survives() {
        let p = parse_path(
            "openconfig",
            "interfaces/interface[name=Ethernet1/1]/state/counters",
        )
        .unwrap();
        assert_eq!(p.elem.len(), 4);
        assert_eq!(p.elem[1].name, "interface");
        assert_eq!(
            p.elem[1].key.get("name").map(String::as_str),
            Some("Ethernet1/1")
        );
    }

    #[test]
    fn vendor_interface_name_with_slashes() {
        let p = parse_path("", "interfaces/interface[name=xe-0/0/0]/state").unwrap();
        assert_eq!(
            p.elem[1].key.get("name").map(String::as_str),
            Some("xe-0/0/0")
        );
    }

    #[test]
    fn multiple_keys_preserved() {
        let p = parse_path("", "foo[name=a][slot=1]/bar").unwrap();
        assert_eq!(p.elem[0].key.len(), 2);
        assert_eq!(p.elem[0].key.get("slot").map(String::as_str), Some("1"));
    }

    #[test]
    fn escaped_brackets_and_backslash_in_values() {
        let p = parse_path("", r"foo[name=a\]b][note=x\\y]/bar").unwrap();
        assert_eq!(p.elem[0].key.get("name").map(String::as_str), Some("a]b"));
        assert_eq!(p.elem[0].key.get("note").map(String::as_str), Some("x\\y"));
    }

    #[test]
    fn key_values_not_trimmed() {
        let p = parse_path("", "foo[name= spaced ]/bar").unwrap();
        assert_eq!(
            p.elem[0].key.get("name").map(String::as_str),
            Some(" spaced ")
        );
    }

    #[test]
    fn malformed_and_duplicate_rejected() {
        assert!(parse_path("", "foo[name=a").is_err());
        assert!(parse_path("", "foo[name=]").is_err());
        assert!(parse_path("", "foo[name=a][name=b]").is_err());
        assert!(parse_path("", "foo]bar/baz").is_err());
        assert!(parse_path("", "foo[bar]/baz").is_err());
        assert!(parse_path("", "foo[name=a]stray/baz").is_err());
        assert!(parse_path("", "/").is_err());
        assert!(parse_path("", "").is_err());
        assert!(parse_path("override: site-override(x)", "a/b").is_err());
    }

    #[test]
    fn canonical_key_stable_origin_sensitive_and_unambiguous() {
        let a = parse_path("openconfig", "interfaces/interface[name=Ethernet1/1]").unwrap();
        let b = parse_path("openconfig", "/interfaces/interface[name=Ethernet1/1]/").unwrap();
        assert_eq!(canonical_key(&a), canonical_key(&b));
        let c = parse_path("ietf", "interfaces/interface[name=Ethernet1/1]").unwrap();
        assert_ne!(canonical_key(&a), canonical_key(&c));
        let d = parse_path("openconfig", "interfaces/interface[name=Ethernet1/2]").unwrap();
        assert_ne!(canonical_key(&a), canonical_key(&d));
        // Separator characters inside values cannot collide: a single key
        // whose value contains ','/'=' must not equal two separate keys.
        let f = parse_path("", r"foo[k=a,b]").unwrap();
        let g = parse_path("", "foo[k=a][x=b]").unwrap();
        assert_ne!(canonical_key(&f), canonical_key(&g));
    }

    #[test]
    fn observation_covers_is_exact_and_structured() {
        let exp = parse_path(
            "openconfig",
            "interfaces/interface[name=Ethernet1/1]/state/counters",
        )
        .unwrap();
        // Descendant leaf observation covers the container expectation.
        let leaf = parse_path(
            "openconfig",
            "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets",
        )
        .unwrap();
        assert!(observation_covers(&exp, &leaf));
        // Exact path covers.
        let exact = parse_path(
            "openconfig",
            "interfaces/interface[name=Ethernet1/1]/state/counters",
        )
        .unwrap();
        assert!(observation_covers(&exp, &exact));
        // Different key instance never satisfies.
        let other = parse_path(
            "openconfig",
            "interfaces/interface[name=Ethernet1/2]/state/counters/in-octets",
        )
        .unwrap();
        assert!(!observation_covers(&exp, &other));
        // Case-sensitive keys.
        let case = parse_path(
            "openconfig",
            "interfaces/interface[name=ethernet1/1]/state/counters/in-octets",
        )
        .unwrap();
        assert!(!observation_covers(&exp, &case));
        // Different origin never satisfies a specified origin.
        let ietf = parse_path(
            "ietf",
            "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets",
        )
        .unwrap();
        assert!(!observation_covers(&exp, &ietf));
        // Ancestor observation cannot cover a deeper leaf expectation.
        let leaf_exp = parse_path(
            "openconfig",
            "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets",
        )
        .unwrap();
        assert!(!observation_covers(&leaf_exp, &exact));
        // Unkeyed expectation element is a wildcard over instances.
        let any_iface = parse_path("openconfig", "interfaces/interface/state").unwrap();
        assert!(observation_covers(&any_iface, &leaf));
        // But a keyed expectation still pins the instance.
        let keyed =
            parse_path("openconfig", "interfaces/interface[name=Ethernet1/2]/state").unwrap();
        assert!(!observation_covers(&keyed, &leaf));
        // Unrelated subtree never matches.
        let bgp = parse_path("openconfig", "network-instances/protocols").unwrap();
        assert!(!observation_covers(&bgp, &leaf));
    }

    #[test]
    fn observation_covers_star_key_is_single_value_wildcard() {
        // Automatic subscriptions emit `name=*`; a `*` expectation
        // value matches any instance value for that key — but never a
        // missing key or a different key SET.
        let wildcard = parse_path(
            "openconfig",
            "interfaces/interface[name=*]/state/counters/in-octets",
        )
        .unwrap();
        let obs = parse_path(
            "openconfig",
            "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets",
        )
        .unwrap();
        assert!(observation_covers(&wildcard, &obs));
        // `*` embedded in a larger value is literal, not a wildcard.
        let embedded = parse_path(
            "openconfig",
            "interfaces/interface[name=Ethernet*]/state/counters/in-octets",
        )
        .unwrap();
        assert!(!observation_covers(&embedded, &obs));
        // Multi-key: `*` on one key, exact on the other.
        let multi = parse_path("", "foo[name=*][slot=1]/bar").unwrap();
        let obs_multi = parse_path("", "foo[name=a][slot=1]/bar/baz").unwrap();
        assert!(observation_covers(&multi, &obs_multi));
        let obs_multi_bad = parse_path("", "foo[name=a][slot=2]/bar/baz").unwrap();
        assert!(!observation_covers(&multi, &obs_multi_bad));
        // Key-set cardinality is still enforced: an observation with an
        // extra key never satisfies a smaller expectation key set.
        let one_key = parse_path("", "foo[name=*]/bar").unwrap();
        assert!(!observation_covers(&one_key, &obs_multi));
    }

    #[test]
    fn delete_covers_is_conservative_and_structured() {
        let container = parse_path(
            "openconfig",
            "interfaces/interface[name=Ethernet1/1]/state/counters",
        )
        .unwrap();
        // Exact delete of the expectation path invalidates it.
        let exact = parse_path(
            "openconfig",
            "interfaces/interface[name=Ethernet1/1]/state/counters",
        )
        .unwrap();
        assert!(delete_covers(&container, &exact));
        // Ancestor delete (parent list instance) invalidates descendants.
        let iface = parse_path("openconfig", "interfaces/interface[name=Ethernet1/1]").unwrap();
        assert!(delete_covers(&container, &iface));
        let interfaces = parse_path("openconfig", "interfaces").unwrap();
        assert!(delete_covers(&container, &interfaces));
        // A delete DEEPER than the expectation is never proof of
        // complete absence — the caller qualifies it separately.
        let leaf = parse_path(
            "openconfig",
            "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets",
        )
        .unwrap();
        assert!(!delete_covers(&container, &leaf));
        // A different keyed instance's delete never invalidates.
        let other = parse_path(
            "openconfig",
            "interfaces/interface[name=Ethernet1/2]/state/counters/in-octets",
        )
        .unwrap();
        assert!(!delete_covers(&container, &other));
        // Sibling subtree delete never invalidates.
        let sibling = parse_path(
            "openconfig",
            "interfaces/interface[name=Ethernet1/1]/state/description",
        )
        .unwrap();
        assert!(!delete_covers(&container, &sibling));
        // Wildcard expectation: ONE concrete instance delete must not
        // imply absence across the whole wildcard scope.
        let wildcard = parse_path(
            "openconfig",
            "interfaces/interface[name=*]/state/counters/in-octets",
        )
        .unwrap();
        assert!(!delete_covers(&wildcard, &leaf));
        // Unkeyed (match-any) expectation element is likewise not
        // invalidated by a single keyed instance delete.
        let unkeyed = parse_path("openconfig", "interfaces/interface/state/counters").unwrap();
        let keyed_leaf = parse_path(
            "openconfig",
            "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets",
        )
        .unwrap();
        assert!(!delete_covers(&unkeyed, &keyed_leaf));
        // Partially-keyed expectation: a delete of one fully-keyed
        // instance must not imply absence for the partial-key scope.
        let partial = parse_path("", "foo[name=a]/bar").unwrap();
        let full = parse_path("", "foo[name=a][role=x]/bar/baz").unwrap();
        assert!(!delete_covers(&partial, &full));
        // Origin mismatch never matches.
        let ietf_leaf = parse_path(
            "ietf",
            "interfaces/interface[name=Ethernet1/1]/state/counters/in-octets",
        )
        .unwrap();
        assert!(!delete_covers(&container, &ietf_leaf));
    }
}
