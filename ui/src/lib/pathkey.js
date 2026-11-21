// Canonical frontend identity for a selected subscription path.
//
// Mirrors the backend's exact selection semantics closely enough for
// deduplication: origin + mode + sample interval + the structured path
// with each segment's [key=value] pairs sorted by key name. Two spellings
// of the same structured path (reordered keys, redundant slashes)
// collapse; different origins, modes, intervals or key VALUES never do.
// This is an identity key only — it is never sent to the backend.

function splitSegments(raw) {
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

function normalizeSegment(seg) {
  const open = seg.indexOf('[');
  if (open === -1) return seg;
  const name = seg.slice(0, open);
  const rest = seg.slice(open);
  // Extract [k=v] pairs at top level of this segment.
  const pairs = [];
  let i = 0;
  while (i < rest.length) {
    if (rest[i] !== '[') { i++; continue; }
    let j = i + 1;
    let escaped = false;
    let close = -1;
    for (; j < rest.length; j++) {
      const ch = rest[j];
      if (escaped) { escaped = false; continue; }
      if (ch === '\\') { escaped = true; continue; }
      if (ch === ']') { close = j; break; }
    }
    if (close === -1) break;
    pairs.push(rest.slice(i, close + 1));
    i = close + 1;
  }
  pairs.sort();
  return name + pairs.join('');
}

/// Canonicalize a raw gNMI path string for identity purposes.
/// Returns '' for empty input; never throws on malformed input —
/// malformed paths simply keep their literal spelling (the backend
/// remains the authority on validity).
export function canonicalPath(raw) {
  if (!raw) return '';
  return splitSegments(raw).map(normalizeSegment).join('/');
}

/// Full selection identity: origin + mode + interval + canonical path.
/// Distinct list-key values, origins, modes and intervals never collapse.
export function selectionKey(path) {
  if (!path) return '';
  return [
    path.origin || '',
    path.mode || '',
    String(path.sample_interval_ns ?? 0),
    canonicalPath(path.path || ''),
  ].join('|');
}

/// True when two selected-path records identify the same subscription.
export function sameSelection(a, b) {
  return selectionKey(a) === selectionKey(b);
}
