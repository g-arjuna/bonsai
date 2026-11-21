// gNMI path template helpers mirroring src/gnmi_path.rs semantics.
//
// Schema paths from the catalogue are TEMPLATES: list segments carry
// bare `[keyname]` placeholders, e.g.
//   /interfaces/interface[name]/state/counters/in-octets
// Operators must instantiate them with concrete key VALUES:
//   /interfaces/interface[name=Ethernet1]/state/counters/in-octets
// This module extracts required keys and instantiates templates with
// correct backslash escaping. The backend remains the authority on
// validity — these helpers only build candidate strings.

const SEGMENT_RE = /([^\/\[]+)((?:\[[^\]]*\])*)/g;

/// Split a raw path on '/' outside bracketed key expressions.
export function splitSegments(raw) {
  const segments = [];
  let depth = 0;
  let start = 0;
  let escaped = false;
  for (let i = 0; i < raw.length; i++) {
    const ch = raw[i];
    if (escaped) { escaped = false; continue; }
    if (ch === '\\') { escaped = true; continue; }
    if (ch === '[') depth++;
    else if (ch === ']') depth = Math.max(0, depth - 1);
    else if (ch === '/' && depth === 0) {
      segments.push(raw.slice(start, i));
      start = i + 1;
    }
  }
  segments.push(raw.slice(start));
  return segments.filter((s) => s.length > 0);
}

/// Extract `[key]` placeholder names from a template segment.
/// A template placeholder is a bracket group with NO '=' inside.
/// Returns ordered unique key names for the segment.
export function segmentTemplateKeys(segment) {
  const keys = [];
  const re = /\[([^\]]*)\]/g;
  let m;
  while ((m = re.exec(segment)) !== null) {
    const inner = m[1];
    if (!inner.includes('=')) {
      // bare placeholder like [name]
      if (inner && !keys.includes(inner)) keys.push(inner);
    }
  }
  return keys;
}

/// All required list keys for a template path, in path order.
/// Returns [{ segmentIndex, segment, key }].
export function templateKeys(templatePath) {
  const out = [];
  splitSegments(templatePath).forEach((seg, idx) => {
    for (const key of segmentTemplateKeys(seg)) {
      out.push({ segmentIndex: idx, segment: seg, key });
    }
  });
  return out;
}

/// True when the path still contains uninstantiated `[key]` placeholders.
export function hasPlaceholders(path) {
  return /\[[^\]=]*\]/.test(path);
}

/// Escape a key VALUE for the wire grammar: '\', '[', ']', '=' are
/// backslash-escaped inside key expressions (mirrors gnmi_path.rs
/// unescape semantics). '/' needs no escaping — it is inside brackets.
export function escapeKeyValue(value) {
  return String(value).replace(/[\\\[\]=]/g, (ch) => '\\' + ch);
}

/// Segment-qualified key identity (M6E-A2): repeated key names in
/// nested lists (e.g. `/a/list[name]/b/list[name]`) are INDEPENDENT
/// placeholders — state must be keyed by position, not name alone.
export function keyId(segmentIndex, key) {
  return `${segmentIndex}:${key}`;
}

/// Look up a key value: segment-qualified id first, bare name as a
/// legacy fallback for callers that have not migrated.
function keyValueFor(keyValues, segmentIndex, key) {
  const qualified = keyValues[keyId(segmentIndex, key)];
  if (qualified !== undefined && qualified !== null && qualified !== '') return qualified;
  const bare = keyValues[key];
  return bare === undefined || bare === null || bare === '' ? undefined : bare;
}

/// Instantiate a template path: replace every `[key]` placeholder with
/// `[key=value]` using the provided keyValues map. Keys are looked up by
/// segment-qualified id (`"{segmentIndex}:{key}"`) so repeated key names
/// in nested lists instantiate independently; a bare `{key: value}`
/// entry remains a fallback for single-occurrence templates.
/// Returns { ok, path, missing } — `missing` lists qualified key ids
/// with no value. Values are used verbatim (no trimming) like the
/// backend parser.
export function instantiateTemplate(templatePath, keyValues) {
  const missing = [];
  const keys = templateKeys(templatePath);
  for (const { segmentIndex, key } of keys) {
    if (keyValueFor(keyValues, segmentIndex, key) === undefined) {
      missing.push(keyId(segmentIndex, key));
    }
  }
  if (missing.length > 0) {
    return { ok: false, path: templatePath, missing };
  }
  // Replace bare [key] placeholders segment by segment. Segments may
  // contain several placeholders; replace each — scoped to THIS
  // segment's qualified ids so nested repeats never collide.
  const segs = splitSegments(templatePath).map((seg, idx) => {
    return seg.replace(/\[([^\]]*)\]/g, (whole, inner) => {
      if (inner.includes('=')) return whole; // already instantiated
      const v = keyValueFor(keyValues, idx, inner);
      if (v === undefined) return whole;
      return `[${inner}=${escapeKeyValue(v)}]`;
    });
  });
  return { ok: true, path: '/' + segs.join('/'), missing: [] };
}

/// gNMI model origins are simple identifiers; anything else (including
/// provenance labels) is invalid on the wire.
export function validOrigin(origin) {
  return !!origin && /^[A-Za-z0-9_\-.]+$/.test(origin);
}

/// Human-readable interval helpers: seconds <-> nanoseconds.
export function secondsToNs(seconds) {
  const s = Number(seconds);
  if (!Number.isFinite(s) || s <= 0) return null;
  const ns = Math.round(s * 1_000_000_000);
  if (!Number.isSafeInteger(ns) || ns <= 0) return null;
  return ns;
}

export function nsToSeconds(ns) {
  const n = Number(ns);
  if (!Number.isFinite(n) || n <= 0) return '';
  const s = n / 1_000_000_000;
  return Number.isInteger(s) ? String(s) : s.toFixed(3).replace(/0+$/, '').replace(/\.$/, '');
}

export function formatInterval(ns) {
  const s = Number(ns);
  if (!Number.isFinite(s) || s <= 0) return '—';
  if (s % 1_000_000_000 === 0) return `${s / 1_000_000_000}s`;
  if (s % 1_000_000 === 0) return `${s / 1_000_000}ms`;
  return `${s}ns`;
}
