/* Design token color values as JS constants.
   Used where CSS custom properties cannot be referenced (D3 attr calls, canvas). */

export const C = {
  bgBase:     '#0a0b0d',
  bgSurface:  '#15171b',
  bgElevated: '#1d2026',

  textPrimary:   '#e8eaed',
  textSecondary: '#9aa0a6',
  textTertiary:  '#5f6368',

  stateHealthy:  '#34d399',
  stateDegraded: '#fbbf24',
  stateFailed:   '#f87171',
  stateInfo:     '#60a5fa',
  stateNeutral:  '#9ca3af',

  accentPrimary: '#5eead4',
  accentMuted:   '#14b8a6',

  borderSubtle:  'rgba(255,255,255,0.06)',
  borderDefault: 'rgba(255,255,255,0.10)',
};

export function healthColor(health) {
  if (health === 'healthy')  return C.stateHealthy;
  if (health === 'warn')     return C.stateDegraded;
  if (health === 'critical') return C.stateFailed;
  return C.stateNeutral;
}

export function roleStrokeColor(role, hostname) {
  const r = (role || '').toLowerCase().replace(/[-_ ]/g, '');
  const h = (hostname || '').toLowerCase();
  if (r === 'spine' && (h.includes('super') || h.startsWith('ss'))) return C.accentMuted;
  if (r === 'spine') return C.accentPrimary;
  if (['pe', 'rr', 'border', 'core'].includes(r)) return C.stateInfo;
  return null; // fall back to health color
}
