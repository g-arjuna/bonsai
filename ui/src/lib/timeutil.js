// Time formatting utilities.

/** Format a nanosecond timestamp as "2 min ago" relative to now. */
export function relativeTime(ns) {
  if (!ns) return '—';
  const deltaSecs = (Date.now() - ns / 1e6) / 1000;
  if (deltaSecs < 5)   return 'just now';
  if (deltaSecs < 60)  return `${Math.floor(deltaSecs)}s ago`;
  if (deltaSecs < 3600) return `${Math.floor(deltaSecs / 60)}m ago`;
  if (deltaSecs < 86400) return `${Math.floor(deltaSecs / 3600)}h ago`;
  return `${Math.floor(deltaSecs / 86400)}d ago`;
}

/** Format a nanosecond timestamp as an absolute ISO string for tooltips. */
export function absoluteTime(ns) {
  if (!ns) return '';
  return new Date(ns / 1e6).toISOString().replace('T', ' ').replace('Z', ' UTC');
}

/** Format ns as HH:MM:SS.mmm */
export function shortTime(ns) {
  if (!ns) return '—';
  return new Date(ns / 1e6).toISOString().slice(11, 23);
}

/** Duration between two nanosecond timestamps, as a human string. */
export function duration(startNs, endNs) {
  if (!startNs || !endNs) return '';
  const secs = Math.round((endNs - startNs) / 1e9);
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}
