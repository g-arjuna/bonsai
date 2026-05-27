/** Thin fetch wrapper — throws on non-ok responses with parsed error body. */
export async function apiFetch(path, opts = {}) {
  const res = await fetch(path, {
    headers: { 'Content-Type': 'application/json', ...(opts.headers || {}) },
    ...opts,
  });
  if (!res.ok) {
    let msg = `HTTP ${res.status}`;
    try { msg = (await res.json()).error || msg; } catch (_) { }
    throw new Error(msg);
  }
  if (res.status === 204) return null;
  return res.json();
}

export const api = {
  jobs: {
    list: () => apiFetch('/api/ml/jobs'),
    get: id => apiFetch(`/api/ml/jobs/${id}`),
    cancel: id => apiFetch(`/api/ml/jobs/${id}/cancel`, { method: 'POST' }),
    retry: id => apiFetch(`/api/ml/jobs/${id}/retry`, { method: 'POST' })
  },
  schedules: {
    list: () => apiFetch('/api/ml/schedules'),
    upsert: b => apiFetch('/api/ml/schedules', { method: 'POST', body: JSON.stringify(b) }),
    del: id => apiFetch(`/api/ml/schedules/${id}`, { method: 'DELETE' })
  },
  models: {
    list: () => apiFetch('/api/ml/models'),
    active: t => apiFetch(`/api/ml/models/active${t ? '?model_type=' + t : ''}`),
    activate: id => apiFetch(`/api/ml/models/${id}/activate`, { method: 'POST' })
  },
  exports: {
    list: () => apiFetch('/api/ml/exports'),
    quality: () => apiFetch('/api/ml/exports/quality')
  },
  embeddings: { stats: () => apiFetch('/api/ml/embeddings/stats') },
  gnn: { results: () => apiFetch('/api/gnn/results?limit=20') },
  detections: { list: (limit = 100) => apiFetch(`/api/detections?limit=${limit}`) },
  events: { list: (params = '') => apiFetch(`/api/events${params}`) },
  sidecar: { status: () => apiFetch('/api/sidecar/status') },
  syslogClusters: () => apiFetch('/api/ml/syslog-clusters'),
  rules: {
    list: () => apiFetch('/api/sidecar/rules'),
    analytics: () => apiFetch('/api/sidecar/rules/analytics'),
    toggle: id => apiFetch(`/api/sidecar/rules/${id}/toggle`, { method: 'POST' }),
    getParams: id => apiFetch(`/api/sidecar/rules/${id}/parameters`),
    patchParams: (id, params) => apiFetch(`/api/sidecar/rules/${id}/parameters`, { method: 'PATCH', body: JSON.stringify({ parameters: params }) }),
    setShadow: (id, enabled) => apiFetch(`/api/sidecar/rules/${id}/shadow-mode`, { method: 'POST', body: JSON.stringify({ enabled }) }),
    shadowFirings: (id, since = 0) => apiFetch(`/api/sidecar/rules/${id}/shadow-firings?since=${since}`),
  },
  syslogRules: {
    list: () => apiFetch('/api/syslog-rules'),
    create: b => apiFetch('/api/syslog-rules', { method: 'POST', body: JSON.stringify(b) }),
  },
  playbooks: {
    list: () => apiFetch('/api/playbooks-v2'),
    get: id => apiFetch(`/api/playbooks-v2/${id}`),
    create: b => apiFetch('/api/playbooks-v2', { method: 'POST', body: JSON.stringify(b) }),
    update: (id, b) => apiFetch(`/api/playbooks-v2/${id}`, { method: 'PUT', body: JSON.stringify(b) }),
    del: id => apiFetch(`/api/playbooks-v2/${id}`, { method: 'DELETE' }),
    stats: () => apiFetch('/api/playbooks-v2/stats'),
    executions: id => apiFetch(`/api/playbooks-v2/${id}/executions`),
  },
};
