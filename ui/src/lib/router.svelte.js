// Minimal Svelte 5 hash router.
// Usage:
//   import { path, navigate, matchRoute } from '$lib/router.svelte.js';
//   const params = matchRoute('/devices/:address', path());

function parsePath() {
  const h = location.hash.slice(1);
  return h.startsWith('/') ? h : '/' + (h || '');
}

let _path = $state(parsePath());

if (typeof window !== 'undefined') {
  window.addEventListener('hashchange', () => {
    _path = parsePath();
  });
  window.addEventListener('popstate', () => {
    _path = parsePath();
  });
}

export function path() {
  return _path;
}

export function navigate(to) {
  location.hash = to;
}

/**
 * Match a route pattern against a path. Returns params object if matched, null otherwise.
 * Pattern segments starting with ':' are captured as params.
 * Example: matchRoute('/devices/:addr', '/devices/10.0.0.1') → { addr: '10.0.0.1' }
 */
export function matchRoute(pattern, currentPath) {
  const pp = pattern.split('/');
  const cp = currentPath.split('/');
  if (pp.length !== cp.length) return null;
  const params = {};
  for (let i = 0; i < pp.length; i++) {
    if (pp[i].startsWith(':')) {
      params[pp[i].slice(1)] = decodeURIComponent(cp[i] ?? '');
    } else if (pp[i] !== cp[i]) {
      return null;
    }
  }
  return params;
}
