/* Design token color values as JS constants.
   Used where CSS custom properties cannot be referenced (D3 attr calls, canvas). */

export const C = {
  bgBase: '#09090c',
  bgSurface: '#13151a',
  bgElevated: '#1c1f27',

  textPrimary: '#e4e8ef',
  textSecondary: '#8b95a3',
  textTertiary: '#525c6b',

  stateHealthy: '#34d399',
  stateDegraded: '#fbbf24',
  stateFailed: '#f87171',
  stateInfo: '#60a5fa',
  stateNeutral: '#9ca3af',

  accentPrimary: '#5eead4',
  accentMuted: '#0d9488',
  accentHover: '#0f766e',

  borderSubtle: 'rgba(255,255,255,0.06)',
  borderDefault: 'rgba(255,255,255,0.11)',

  vendorPrometheus: '#e6522c',
  vendorSplunk: '#65a637',
  vendorElastic: '#00bfb3',
  vendorServiceNow: '#81b5a1',
  vendorNetBox: '#00b0f0',
  vendorStub: '#6b7280',
};

export function healthColor(health) {
  if (health === 'healthy') return C.stateHealthy;
  if (health === 'warn') return C.stateDegraded;
  if (health === 'critical') return C.stateFailed;
  return C.stateNeutral;
}

export function roleStrokeColor(role) {
  const r = (role || '').toLowerCase().replace(/[-_\s]/g, '');
  // Tier-0 roles (core / superspine / route-reflector / WAN core)
  if (['superspine', 'superleaf', 'core', 'backbone', 'rr', 'routereflector',
    'wancore', 'wanrouter', 'wan', 'datacentercore', 'dccore', 'borderleaf'].includes(r))
    return C.accentMuted;
  // Tier-1 roles (spine / aggregation / distribution / PE / WLC / firewall)
  if (['spine', 'aggregation', 'distribution', 'pe', 'providerededge',
    'border', 'borderrouter', 'p',
    'wlc', 'wirelesscontroller', 'wlancontroller',
    'firewall', 'fw', 'loadbalancer', 'lb'].includes(r))
    return C.accentPrimary;
  // Provider/WAN edge roles get info-blue
  if (['ce', 'customeredge', 'edge', 'edgerouter', 'cpe'].includes(r))
    return C.stateInfo;
  return null; // fall back to health color
}
