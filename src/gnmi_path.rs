//! Structured gNMI path parsing shared by configuration validation,
//! subscription planning and the runtime subscriber.
//!
//! Path elements are separated by '/' ONLY outside bracketed key
//! expressions, so keyed names containing slashes (e.g. `name=Ethernet1/1`,
//! `name=xe-0/0/0`) survive intact. Elements support `elem[k1=v1][k2=v2]`
//! key lists. Malformed brackets, empty names/keys and duplicate keys are
//! rejected — never silently repaired.

use std::collections::HashMap;

use crate::proto::gnmi::{Path, PathElem};

/// Parse a configured path string into a structured gNMI Path.
/// `origin` is the gNMI model origin (e.g. "openconfig"), NOT a provenance
/// label — callers must keep provenance separate.
pub fn parse_path(origin: &str, raw_path: &str) -> Result<Path, String> {
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

/// Canonical identity for a structured path: origin plus elements in order,
/// each rendered as `name{k1=v1,k2=v2}` with keys sorted. Equivalent textual
/// spellings of the same structured path produce the same key; different
/// origins or different keyed paths never collapse together.
pub fn canonical_key(path: &Path) -> String {
    let mut out = String::new();
    out.push_str(&path.origin);
    for elem in &path.elem {
        out.push('/');
        out.push_str(&elem.name);
        if !elem.key.is_empty() {
            let mut pairs: Vec<(&String, &String)> = elem.key.iter().collect();
            pairs.sort();
            out.push('[');
            for (i, (k, v)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&format!("{}={}", k, v));
            }
            out.push(']');
        }
    }
    out
}

/// Convenience: canonical dedup/identity key directly from a configured
/// (origin, raw path) pair. Returns None for malformed input.
pub fn canonical_path_key(origin: &str, raw_path: &str) -> Option<String> {
    parse_path(origin, raw_path).ok().map(|p| canonical_key(&p))
}

/// Split on '/' separators outside '[' ... ']' key expressions. An unclosed
/// '[' keeps the remainder inside the current segment so the segment parser
/// rejects it as malformed.
fn split_path_segments(raw: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (idx, ch) in raw.char_indices() {
        match ch {
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
    let name = segment[..first_key].trim();
    if name.is_empty() {
        return None;
    }

    let mut key = HashMap::new();
    let mut rest = &segment[first_key..];
    while !rest.is_empty() {
        let after_open = rest.strip_prefix('[')?;
        let close = after_open.find(']')?;
        let pair = &after_open[..close];
        let (key_name, key_value) = pair.split_once('=')?;
        let key_name = key_name.trim();
        let key_value = key_value.trim();
        if key_name.is_empty() || key_value.is_empty() {
            return None;
        }
        if key
            .insert(key_name.to_string(), key_value.to_string())
            .is_some()
        {
            return None;
        }
        rest = &after_open[close + 1..];
    }

    Some((name.to_string(), key))
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
        assert_eq!(p.elem.len(), 3);
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
    fn malformed_and_duplicate_rejected() {
        assert!(parse_path("", "foo[name=a").is_err());
        assert!(parse_path("", "foo[name=]").is_err());
        assert!(parse_path("", "foo[name=a][name=b]").is_err());
        assert!(parse_path("", "/").is_err());
        assert!(parse_path("", "").is_err());
    }

    #[test]
    fn canonical_key_stable_and_origin_sensitive() {
        let a = parse_path("openconfig", "interfaces/interface[name=Ethernet1/1]").unwrap();
        let b = parse_path("openconfig", "/interfaces/interface[name=Ethernet1/1]/").unwrap();
        assert_eq!(canonical_key(&a), canonical_key(&b));
        let c = parse_path("ietf", "interfaces/interface[name=Ethernet1/1]").unwrap();
        assert_ne!(canonical_key(&a), canonical_key(&c));
        let d = parse_path("openconfig", "interfaces/interface[name=Ethernet1/2]").unwrap();
        assert_ne!(canonical_key(&a), canonical_key(&d));
    }
}
