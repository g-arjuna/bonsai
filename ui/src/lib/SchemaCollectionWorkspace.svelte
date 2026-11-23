<script>
  // M6D — Integrated device-effective YANG browser and schema-bound
  // collection-plan workspace.
  //
  // This component is a VIEW over the authoritative backend: the compiled
  // catalogue, persisted applicability snapshot, schema-binding preview /
  // activation / revocation and subscription-status records. It never
  // computes its own eligibility and never mutates collection state except
  // through the existing API endpoints.
  import { onMount } from 'svelte';
  import { relativeTime, absoluteTime } from '$lib/timeutil.js';
  import { selectionKey } from '$lib/pathkey.js';
  import {
    templateKeys, instantiateTemplate, hasPlaceholders, keyId,
    validOrigin, secondsToNs, nsToSeconds, formatInterval
  } from '$lib/gnmi.js';

  let { device, onchanged } = $props();

  // M6E-A4: unsaved drafts survive schema-tab navigation, scoped by
  // stable device identity. Module-scoped only — never browser storage,
  // never secrets or approval authority.
  const draftStore = new Map();
  function deviceDraftKey(d) {
    return d?.external_id || d?.address || '';
  }

  const PAGE_SIZE = 100;
  const STATUSES = ['applicable', 'candidate', 'unknown', 'not_applicable', 'stale', 'conflicted'];
  const KINDS = ['container', 'list', 'leaf', 'leaf-list', 'anyxml', 'anydata'];

  // ── Session role (display hint only; backend enforces RBAC) ──────────
  let me = $state(null);
  let isAdmin = $derived(me?.role === 'admin');

  // ── Device applicability (authoritative device evidence) ─────────────
  let appState = $state('idle'); // idle|loading|ready|error
  let appError = $state('');
  let applicability = $state(null); // {snapshot, paths, paths_matching, modules}
  let filters = $state({ q: '', status: '', module: '', kind: '', config: '', feature: '' });
  let pathOffset = $state(0);
  let qTimer = null;
  // M6E-A3: request-generation fencing — a slow response for a previous
  // device/filter/page can never overwrite current UI state.
  let appGen = 0;

  // ── M6E remote discovery job state ───────────────────────────────────
  let executor = $state('auto'); // auto|core_direct|assigned_collector
  let remoteJob = $state(null); // {job_id,state,collector_id,reason,...}
  let jobPollTimer = null;
  let confirmRefresh = $state(false);

  // ── Draft selection ──────────────────────────────────────────────────
  // Each entry: { id, template, path, origin, mode, sample_interval_ns,
  //               provenance, rationale, optional, keyValues, needsKeys,
  //               warning }
  let draft = $state([]);
  let draftLoadedFor = $state('');

  // ── Preview / activation ─────────────────────────────────────────────
  let preview = $state(null);
  let previewBusy = $state(false);
  let previewError = $state('');
  let previewGen = 0;
  let datastore = $state('');
  let confirmActivate = $state(false);
  let activateBusy = $state(false);
  let activateMessage = $state('');
  let activateError = $state('');
  let confirmRevoke = $state(false);
  let revokeReason = $state('');
  let revokeBusy = $state(false);
  let revokeMessage = $state('');
  let refreshBusy = $state(false);
  let refreshMessage = $state('');
  let notice = $state('');

  // ── M6F-F per-device schema selection ──────────────────────────────
  let schemaSel = $state(null);   // GET /schema-selection payload
  let schemaBundles = $state([]); // verified compiled bundles
  let schemaSelBusy = $state(false);
  let schemaSelMessage = $state('');
  let schemaSelError = $state('');
  let schemaSelChoice = $state('');
  let confirmSchemaSel = $state(false);

  const address = $derived(device?.address || '');
  const binding = $derived(device?.schema_binding || null);
  const snapshot = $derived(applicability?.snapshot || null);

  onMount(async () => {
    try {
      const r = await fetch('/api/auth/me');
      if (r.ok) me = await r.json();
    } catch { /* role unknown — server still enforces */ }
  });

  // Load applicability whenever the device changes or filters/page change.
  $effect(() => {
    const addr = address;
    if (!addr) return;
    const f = filters;
    const off = pathOffset;
    const timer = setTimeout(() => {
      loadApplicability(addr, f, off);
      loadSchemaSelection(addr);
    }, f.q ? 250 : 0);
    return () => clearTimeout(timer);
  });

  async function loadApplicability(addr, f, off) {
    const gen = ++appGen;
    appState = 'loading';
    appError = '';
    try {
      const params = new URLSearchParams();
      if (f.q) params.set('q', f.q);
      if (f.status) params.set('status', f.status);
      if (f.module) params.set('module', f.module);
      if (f.kind) params.set('kind', f.kind);
      if (f.config) params.set('config', f.config);
      if (f.feature) params.set('feature', f.feature);
      params.set('limit', String(PAGE_SIZE));
      params.set('offset', String(off));
      const r = await fetch(
        '/api/devices/' + encodeURIComponent(addr) + '/yang-applicability?' + params
      );
      const payload = await r.json().catch(() => null);
      // Fencing: device/filter/page changed or a newer request started
      // while this one was in flight — discard entirely.
      if (gen !== appGen || addr !== address) return;
      if (!r.ok) throw new Error(payload?.error || payload?.message || ('HTTP ' + r.status));
      applicability = payload;
      appState = 'ready';
      if (!datastore && applicability?.snapshot?.datastore) {
        datastore = applicability.snapshot.datastore;
      }
      // Restore or seed the draft once per device. A preserved draft
      // always wins over re-seeding from the approved selection; new
      // evidence never silently deletes it — preview eligibility is
      // fenced separately by previewStale.
      const dkey = deviceDraftKey(device);
      if (draftLoadedFor !== dkey && device) {
        draftLoadedFor = dkey;
        const stored = draftStore.get(dkey);
        if (Array.isArray(stored) && stored.length > 0) {
          draft = dedupe(stored.map(normalizeDraftEntry));
          draft.forEach(refreshEntryPath);
        } else {
          const approved = Array.isArray(device.selected_paths) ? device.selected_paths : [];
          draft = dedupe(approved.map(normalizeDraftEntry));
        }
        invalidatePreview();
      }
    } catch (e) {
      if (gen !== appGen || addr !== address) return;
      appError = e.message || 'Failed to load applicability';
      appState = 'error';
    }
  }

  // Persist the working draft whenever it changes (draft entries are
  // $state proxies — this tracks deep mutations too).
  $effect(() => {
    const dkey = deviceDraftKey(device);
    if (!dkey || draftLoadedFor !== dkey) return;
    draftStore.set(dkey, draft.map((e) => ({ ...e, keyValues: { ...e.keyValues } })));
  });

  function onFilter(key, value) {
    filters = { ...filters, [key]: value };
    pathOffset = 0;
    invalidatePreview();
  }

  function page(delta) {
    pathOffset = Math.max(0, pathOffset + delta * PAGE_SIZE);
  }

  // ── Draft helpers ────────────────────────────────────────────────────
  function normalizeDraftEntry(p) {
    return {
      id: selectionKey(p),
      template: p.path || '',
      path: p.path || '',
      origin: p.origin || '',
      provenance: p.provenance || '',
      mode: p.mode || 'SAMPLE',
      sample_interval_ns: Number(p.sample_interval_ns || 0),
      rationale: p.rationale || '',
      optional: !!p.optional,
      keyValues: {},
      needsKeys: hasPlaceholders(p.path || ''),
      warning: ''
    };
  }

  function dedupe(entries) {
    const seen = new Set();
    const out = [];
    for (const e of entries || []) {
      if (!e.path && !e.template) continue;
      const key = selectionKey({
        path: e.path || e.template,
        origin: e.origin,
        mode: e.mode,
        sample_interval_ns: e.sample_interval_ns
      });
      if (seen.has(key)) continue;
      seen.add(key);
      out.push(e);
    }
    return out;
  }

  function entryKeyValues(entry) {
    return entry.keyValues || {};
  }

  function refreshEntryPath(entry) {
    const keys = templateKeys(entry.template || entry.path);
    if (keys.length === 0) {
      entry.path = entry.template || entry.path;
      entry.needsKeys = false;
      entry.warning = '';
      return;
    }
    const res = instantiateTemplate(entry.template, entryKeyValues(entry));
    entry.needsKeys = !res.ok;
    entry.path = res.ok ? res.path : entry.template;
    entry.warning = res.ok ? '' : 'Missing list keys: ' + res.missing.join(', ');
  }

  function isEntryReady(entry) {
    return !entry.needsKeys && entry.path && !hasPlaceholders(entry.path);
  }

  function addPathFromBrowser(p) {
    const entry = {
      id: '',
      template: p.path,
      path: p.path,
      origin: '',
      provenance: 'schema_workspace',
      mode: 'SAMPLE',
      sample_interval_ns: 10_000_000_000,
      rationale: p.module || '',
      optional: false,
      keyValues: {},
      needsKeys: false,
      warning: ''
    };
    refreshEntryPath(entry);
    entry.id = selectionKey({
      path: entry.path || entry.template,
      origin: entry.origin,
      mode: entry.mode,
      sample_interval_ns: entry.sample_interval_ns
    });
    draft = dedupe([...draft, entry]);
    invalidatePreview();
  }

  function removeEntry(entry) {
    draft = draft.filter((e) => e !== entry);
    invalidatePreview();
  }

  function updateEntry(entry, patch) {
    Object.assign(entry, patch);
    if ('keyValues' in patch || 'template' in patch) refreshEntryPath(entry);
    entry.id = selectionKey({
      path: entry.path || entry.template,
      origin: entry.origin,
      mode: entry.mode,
      sample_interval_ns: entry.sample_interval_ns
    });
    invalidatePreview();
  }

  function setMode(entry, mode) {
    updateEntry(entry, {
      mode,
      sample_interval_ns: mode === 'ON_CHANGE' ? 0 : (entry.sample_interval_ns || 10_000_000_000)
    });
  }

  function setIntervalSeconds(entry, seconds) {
    const ns = secondsToNs(seconds);
    if (ns === null) return;
    updateEntry(entry, { sample_interval_ns: ns });
  }

  function draftPayload() {
    return draft.filter(isEntryReady).map((e) => ({
      path: e.path,
      origin: e.origin || '',
      provenance: e.provenance || '',
      mode: e.mode,
      sample_interval_ns: e.mode === 'ON_CHANGE' ? 0 : e.sample_interval_ns,
      rationale: e.rationale || '',
      optional: !!e.optional
    }));
  }

  function draftSignature() {
    return JSON.stringify(draftPayload());
  }

  function invalidatePreview() {
    previewGen++;
    preview = null;
    previewError = '';
    activateMessage = '';
    activateError = '';
    confirmActivate = false;
  }

  // A preview is stale when the draft signature, datastore, device
  // configuration revision, evidence fingerprint or collector no
  // longer match what was evaluated — activation from an obsolete
  // preview is impossible. Changed/expired evidence invalidates preview
  // ELIGIBILITY only; the draft itself is preserved.
  const previewStale = $derived(
    !!preview && (
      preview._signature !== draftSignature() ||
      preview._datastore !== (datastore || null) ||
      (preview._evidence_fp !== undefined &&
        preview._evidence_fp !== (snapshot?.evidence_fingerprint ?? null)) ||
      (preview.config_revision != null && device?.updated_at_ns != null &&
        preview.config_revision !== device.updated_at_ns)
    )
  );

  const unresolvedCount = $derived(draft.filter((e) => !isEntryReady(e)).length);
  const readyCount = $derived(draftPayload().length);

  async function runPreview() {
    if (!address || previewBusy) return;
    if (unresolvedCount > 0) {
      previewError = unresolvedCount + ' selected path(s) still need list keys or are invalid.';
      return;
    }
    if (readyCount === 0) {
      previewError = 'Select at least one path before previewing.';
      return;
    }
    previewBusy = true;
    previewError = '';
    const gen = ++previewGen;
    const signature = draftSignature();
    try {
      const r = await fetch(
        '/api/devices/' + encodeURIComponent(address) + '/schema-binding/preview',
        {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({
            selected_paths: draftPayload(),
            datastore: datastore || null
          })
        }
      );
      const payload = await r.json().catch(() => ({}));
      if (gen !== previewGen) return; // superseded by a newer request
      if (!r.ok) {
        preview = null;
        previewError = payload.error || payload.message || ('Preview failed (HTTP ' + r.status + ')');
        return;
      }
      preview = payload;
      preview._signature = signature;
      preview._datastore = datastore || null;
      preview._evidence_fp = applicability?.snapshot?.evidence_fingerprint ?? null;
    } catch (e) {
      if (gen === previewGen) {
        preview = null;
        previewError = e.message || 'Preview request failed';
      }
    } finally {
      if (gen === previewGen) previewBusy = false;
    }
  }

  // ── Diff: approved/current selection vs proposed ─────────────────────
  // M6E-A5: subscription IDENTITY (exact path + origin) is separated
  // from PARAMETERS (mode, interval). A mode or interval change on the
  // same path is reported as a parameter change — never as a
  // remove+add — and the backend manifest identity is untouched.
  function diffProposed() {
    if (!preview?.evaluation?.proposed_subscriptions) {
      return { added: [], removed: [], changed: [] };
    }
    const current = Array.isArray(device?.selected_paths) ? device.selected_paths : [];
    const identityOf = (p) => (p.path || '') + '|' + (p.origin || '');
    const paramsOf = (p) => ({
      mode: p.mode || 'SAMPLE',
      interval: Number(p.sample_interval_ns || 0)
    });
    const proposed = preview.evaluation.proposed_subscriptions;
    const curById = new Map(current.map((p) => [identityOf(p), p]));
    const propById = new Map(proposed.map((p) => [identityOf(p), p]));
    const added = proposed.filter((p) => !curById.has(identityOf(p)));
    const removed = current.filter((p) => !propById.has(identityOf(p)));
    const changed = [];
    for (const p of proposed) {
      const c = curById.get(identityOf(p));
      if (!c) continue;
      const cp = paramsOf(c);
      const pp = paramsOf(p);
      const diffs = [];
      if (cp.mode !== pp.mode) diffs.push(`mode ${cp.mode}→${pp.mode}`);
      if (cp.interval !== pp.interval) {
        diffs.push(`interval ${formatInterval(cp.interval)}→${formatInterval(pp.interval)}`);
      }
      if (diffs.length > 0) {
        changed.push({ path: p.path, origin: p.origin || '', changes: diffs.join(', ') });
      }
    }
    return { added, removed, changed };
  }

  const canActivate = $derived(
    !!preview &&
    !previewStale &&
    !!preview.evaluation?.activation_permitted &&
    !activateBusy
  );

  async function runActivate() {
    if (!canActivate || activateBusy) return;
    activateBusy = true;
    activateError = '';
    activateMessage = '';
    try {
      const ev = preview.evaluation;
      const r = await fetch(
        '/api/devices/' + encodeURIComponent(address) + '/schema-binding/activate',
        {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({
            selected_paths: draftPayload(),
            datastore: datastore || null,
            plan_id: ev.plan_id,
            binding_fingerprint: ev.binding_fingerprint,
            expected_config_revision: preview.config_revision
          })
        }
      );
      const payload = await r.json().catch(() => ({}));
      if (r.status === 409) {
        activateError = 'Conflict: device configuration changed since this preview. ' +
          'Your draft is preserved — review the refreshed evidence and preview again.';
        preview = null;
        await loadApplicability(address, filters, pathOffset);
        if (onchanged) onchanged();
        return;
      }
      if (!r.ok) {
        if (r.status === 403) {
          activateError = 'Forbidden: schema-bound activation requires an admin role.';
        } else {
          activateError = payload.error || payload.message || ('Activation failed (HTTP ' + r.status + ')');
        }
        return;
      }
      activateMessage = 'Schema-bound plan activated (approval revision ' +
        (payload.approval_revision ?? '—') + '). Collector resync carries the new authority.';
      confirmActivate = false;
      preview = null;
      if (onchanged) onchanged();
      await loadApplicability(address, filters, pathOffset);
    } catch (e) {
      activateError = e.message || 'Activation request failed';
    } finally {
      activateBusy = false;
    }
  }

  async function runRevoke() {
    if (revokeBusy) return;
    revokeBusy = true;
    revokeMessage = '';
    activateError = '';
    try {
      const r = await fetch(
        '/api/devices/' + encodeURIComponent(address) + '/schema-binding/revoke',
        {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ reason: revokeReason || 'operator revocation' })
        }
      );
      const payload = await r.json().catch(() => ({}));
      if (r.status === 403) {
        revokeMessage = 'Forbidden: revocation requires an admin role.';
        return;
      }
      if (!r.ok) {
        revokeMessage = payload.error || payload.message || ('Revocation failed (HTTP ' + r.status + ')');
        return;
      }
      revokeMessage = 'Schema-bound approval revoked — acquisition is disabled and the ' +
        'previous plan is NOT restored. Re-preview and re-activate to approve a new plan.';
      confirmRevoke = false;
      revokeReason = '';
      if (onchanged) onchanged();
      await loadApplicability(address, filters, pathOffset);
    } catch (e) {
      revokeMessage = e.message || 'Revocation request failed';
    } finally {
      revokeBusy = false;
    }
  }

  // M6F-F: load the device's schema selection + verified bundle list.
  async function loadSchemaSelection(addr) {
    if (!addr) return;
    try {
      const [selR, bundlesR] = await Promise.all([
        fetch('/api/devices/' + encodeURIComponent(addr) + '/schema-selection'),
        fetch('/api/yang/compiled')
      ]);
      if (selR.ok) schemaSel = await selR.json();
      if (bundlesR.ok) {
        const payload = await bundlesR.json();
        schemaBundles = (payload.bundles || []).filter((b) => b.schema_id);
      }
    } catch { /* selection view is advisory; backend remains authority */ }
  }

  async function applySchemaSelection(clear) {
    if (schemaSelBusy) return;
    schemaSelBusy = true;
    schemaSelMessage = '';
    schemaSelError = '';
    try {
      const url = '/api/devices/' + encodeURIComponent(address) + '/schema-selection';
      const r = await fetch(url, {
        method: clear ? 'DELETE' : 'PUT',
        headers: { 'content-type': 'application/json' },
        body: clear ? undefined : JSON.stringify({ schema_id: schemaSelChoice })
      });
      const payload = await r.json().catch(() => ({}));
      if (r.status === 403) {
        schemaSelError = 'Forbidden: schema selection requires an admin role.';
        return;
      }
      if (!r.ok) {
        schemaSelError = payload.error || payload.message || ('Schema selection failed (HTTP ' + r.status + ')');
        return;
      }
      schemaSel = payload;
      schemaSelMessage = clear
        ? 'Schema selection cleared — device follows the active catalogue.'
        : 'Schema selection updated to ' + shortId(payload.effective_schema_id) +
          '. Approvals bound to a different schema were revoked.';
      confirmSchemaSel = false;
      if (onchanged) onchanged();
      await loadApplicability(address, filters, pathOffset);
    } catch (e) {
      schemaSelError = e.message || 'Schema selection request failed';
    } finally {
      schemaSelBusy = false;
    }
  }

  // M6E: poll remote discovery job status until terminal, then reload
  // the authoritative snapshot. Bounded attempts — the durable job
  // deadline is the outer bound.
  async function pollDiscoveryJob(jobId) {
    if (jobPollTimer) clearTimeout(jobPollTimer);
    let attempts = 0;
    const tick = async () => {
      attempts++;
      try {
        const r = await fetch(
          '/api/devices/' + encodeURIComponent(address) + '/yang-discovery/jobs'
        );
        if (r.ok) {
          const payload = await r.json().catch(() => ({}));
          const jobs = payload.jobs || [];
          const job = jobs.find((j) => j.job_id === jobId) || jobs[0] || null;
          if (job && address === device?.address) {
            remoteJob = job;
            if (['completed', 'unsupported', 'denied', 'timed_out', 'failed',
                 'cancelled', 'stale_authority', 'superseded'].includes(job.state)) {
              refreshBusy = false;
              if (job.state === 'completed') {
                refreshMessage = 'Remote discovery completed via collector ' +
                  (job.collector_id || '—') + ' — snapshot #' + (job.snapshot_id ?? '—') + '.';
                invalidatePreview();
                if (onchanged) onchanged();
                await loadApplicability(address, filters, pathOffset);
              } else {
                refreshMessage = 'Remote discovery finished: ' + job.state +
                  (job.reason_code ? ' (' + job.reason_code + ')' : '') +
                  (job.result_classification ? ' — ' + job.result_classification : '');
              }
              return;
            }
          }
        }
      } catch { /* transient — keep polling */ }
      if (attempts < 90) { // ~3 min at 2s intervals
        jobPollTimer = setTimeout(tick, 2000);
      } else {
        refreshBusy = false;
        refreshMessage = 'Remote discovery job still pending — check job status later.';
      }
    };
    jobPollTimer = setTimeout(tick, 500);
  }

  async function runRefresh() {
    if (refreshBusy || !address) return;
    // M6E-A6: refreshing an active schema-bound device can invalidate
    // its approval — require explicit confirmation first.
    if (binding && !confirmRefresh) {
      confirmRefresh = true;
      return;
    }
    confirmRefresh = false;
    refreshBusy = true;
    refreshMessage = '';
    notice = '';
    remoteJob = null;
    try {
      const r = await fetch(
        '/api/devices/' + encodeURIComponent(address) + '/yang-applicability/refresh',
        {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ executor, datastore: datastore || null })
        }
      );
      const payload = await r.json().catch(() => ({}));
      if (r.status === 403) {
        refreshMessage = 'Forbidden: applicability refresh requires an admin role.';
        return;
      }
      if (r.status === 202) {
        // Remote discovery accepted — poll the durable job.
        refreshMessage = 'Remote discovery queued on collector ' +
          (payload.collector_id || '—') + ' (job ' + (payload.job_id || '').slice(0, 8) + '…)';
        remoteJob = { job_id: payload.job_id, state: payload.state || 'queued',
                      collector_id: payload.collector_id, reason_code: null };
        pollDiscoveryJob(payload.job_id);
        return;
      }
      if (!r.ok) {
        refreshMessage = payload.error || payload.message || ('Refresh failed (HTTP ' + r.status + ')');
        return;
      }
      refreshMessage = 'Refresh complete: outcome=' + (payload.outcome || 'unknown') +
        ', overall=' + (payload.overall_status || 'unknown') +
        (payload.schema_binding_revoked
          ? ' — the previous schema-bound approval was REVOKED; re-preview and re-activate.'
          : '') +
        (payload.error ? ' — ' + payload.error : '');
      invalidatePreview();
      if (onchanged) onchanged();
      await loadApplicability(address, filters, pathOffset);
    } catch (e) {
      refreshMessage = e.message || 'Refresh request failed';
    } finally {
      refreshBusy = false;
    }
  }

  // ── Presentation helpers ─────────────────────────────────────────────
  function statusClass(s) {
    switch (s) {
      case 'applicable': return 'ok';
      case 'candidate': return 'warn';
      case 'unknown': case 'stale': return 'warn';
      case 'conflicted': case 'not_applicable': return 'bad';
      default: return 'muted';
    }
  }

  function freshnessLabel() {
    if (!snapshot) return 'No discovery';
    switch (snapshot.freshness) {
      case 'fresh': return 'Fresh';
      case 'stale': return 'Stale';
      case 'invalid': return 'Invalid';
      default: return snapshot.freshness || 'Unknown';
    }
  }

  function freshnessClass() {
    if (!snapshot) return 'muted';
    if (snapshot.freshness === 'fresh') return 'ok';
    return 'warn';
  }

  function shortId(s) {
    if (!s) return '—';
    return s.length > 16 ? s.slice(0, 12) + '…' + s.slice(-4) : s;
  }

  function decisionKindLabel(k) {
    return (k || '').replace(/_/g, ' ');
  }

  function subscriptionStatusClass(s) {
    switch (s) {
      case 'observed': return 'ok';
      case 'synchronized': case 'submitted': return 'info';
      case 'pending': return 'warn';
      case 'failed': case 'disconnected': return 'bad';
      case 'stale': case 'superseded': case 'revoked': return 'muted';
      case 'deleted': return 'warn';
      default: return 'muted';
    }
  }

  const modules = $derived(applicability?.modules || []);
  const pathRows = $derived(applicability?.paths || []);
  const pathsMatching = $derived(applicability?.paths_matching || 0);
  const hasPrevPage = $derived(pathOffset > 0);
  const hasNextPage = $derived(pathOffset + PAGE_SIZE < pathsMatching);
  const eval_ = $derived(preview?.evaluation || null);
  const diff = $derived(diffProposed());
</script>

<section class="workspace" aria-label="Schema collection workspace" data-testid="schema-workspace">
  <header class="ws-header">
    <h3>Schema-bound collection</h3>
    <p class="muted">
      Browse device-effective YANG evidence, select exact telemetry paths,
      preview the proposed plan, and manage the approved binding.
      Applicability, transport acceptance and received telemetry are
      separate claims — this panel shows each stage explicitly.
    </p>
  </header>

  <!-- ── 1. Device / collector context ──────────────────────────────── -->
  <div class="card context">
    <div class="ctx-grid">
      <div><span class="muted">Device</span><strong>{device?.hostname || device?.address}</strong></div>
      <div><span class="muted">Collector</span><strong>{device?.collector_id || 'unassigned'}</strong></div>
      <div><span class="muted">Acquisition intent</span><strong>{device?.acquisition || 'automatic'}</strong></div>
      <div>
        <span class="muted">Schema binding</span>
        <strong>
          {#if binding}active (rev {binding.approval_revision}){:else}none{/if}
        </strong>
      </div>
    </div>
    {#if binding}
      <div class="binding-meta muted" data-testid="binding-meta">
        plan {shortId(binding.plan_id)} · schema {shortId(binding.schema_id)} ·
        approved by {binding.approved_by || '—'} {relativeTime(binding.approved_at_ns)} ·
        datastore {binding.datastore || '—'} · collector {binding.collector_id || '—'}
      </div>
    {/if}
  </div>

  <!-- ── 2. Schema and evidence status ──────────────────────────────── -->
  <div class="card" data-testid="evidence-card">
    <div class="card-head">
      <h4>Device schema evidence</h4>
      <div class="head-actions">
        {#if me && !isAdmin}
          <span class="muted" data-testid="role-note">Signed in as {me.role} — refresh/activate/revoke require admin.</span>
        {/if}
        {#if device?.collector_id}
          <label class="muted" data-testid="executor-select">Discovery
            <select bind:value={executor} aria-label="Discovery executor">
              <option value="auto">auto</option>
              <option value="core_direct">core direct</option>
              <option value="assigned_collector">assigned collector</option>
            </select>
          </label>
        {/if}
        <button
          class="ghost"
          disabled={refreshBusy}
          title={isAdmin ? 'Re-run device discovery (may invalidate an active approval)' : 'Requires admin role'}
          onclick={runRefresh}
          data-testid="refresh-applicability"
        >{refreshBusy ? 'Refreshing…' : 'Refresh discovery'}</button>
      </div>
    </div>
    {#if confirmRefresh}
      <div class="notice warn confirm" data-testid="refresh-confirm">
        <p>
          <strong>Refresh discovery on a schema-bound device?</strong><br/>
          New evidence may not match the approved binding — the active
          schema-bound approval can be REVOKED and acquisition disabled.
          The previous plan is not restored; re-preview and re-activate
          to approve again.
        </p>
        <div class="head-actions">
          <button class="primary" disabled={refreshBusy} onclick={runRefresh} data-testid="confirm-refresh">Refresh anyway</button>
          <button class="ghost" onclick={() => { confirmRefresh = false; }} data-testid="cancel-refresh">Cancel</button>
        </div>
      </div>
    {/if}
    {#if remoteJob}
      <div class="notice info" data-testid="remote-job" role="status">
        Remote discovery job <span class="mono-sm">{remoteJob.job_id?.slice(0, 8)}…</span> —
        <strong>{remoteJob.state}</strong>
        {#if remoteJob.collector_id}via collector {remoteJob.collector_id}{/if}
        {#if remoteJob.reason_code}· {remoteJob.reason_code}{/if}
        {#if remoteJob.result_classification}· {remoteJob.result_classification}{/if}
        {#if remoteJob.snapshot_id}· snapshot #{remoteJob.snapshot_id}{/if}
      </div>
    {/if}
    {#if refreshMessage}
      <div class="notice {refreshMessage.startsWith('Forbidden') ? 'error' : ''}" data-testid="refresh-message" role="status">{refreshMessage}</div>
    {/if}
    {#if appState === 'loading' && !applicability}
      <p class="muted">Loading applicability evidence…</p>
    {:else if appState === 'error'}
      <div class="notice error" role="alert">{appError}</div>
    {:else if !snapshot}
      <p class="muted" data-testid="no-discovery">
        No applicability discovery has run for this device. An admin can run
        “Refresh discovery” — Core-direct, or via the assigned authenticated
        collector when the device is only reachable remotely.
      </p>
    {:else}
      <div class="evidence-grid">
        <div><span class="muted">Overall</span><strong class="status-{statusClass(snapshot.overall_status)}">{snapshot.overall_status}</strong></div>
        <div><span class="muted">Freshness</span><strong class="status-{freshnessClass()}">{freshnessLabel()}</strong></div>
        <div><span class="muted">Active schema</span><strong title={snapshot.active_schema_id || ''}>{shortId(snapshot.active_schema_id)}</strong></div>
        <div><span class="muted">Effective schema</span><strong title={snapshot.effective_schema_id || ''} data-testid="effective-schema">{shortId(snapshot.effective_schema_id)}</strong></div>
        {#if snapshot.selected_schema_id}
          <div><span class="muted">Selected schema</span><strong title={snapshot.selected_schema_id}>{shortId(snapshot.selected_schema_id)}</strong></div>
        {/if}
        <div><span class="muted">Snapshot</span><strong>#{snapshot.snapshot_id}</strong></div>
        <div><span class="muted">Observed</span><strong title={absoluteTime(snapshot.observed_at_ns)}>{relativeTime(snapshot.observed_at_ns)}</strong></div>
        <div><span class="muted">Datastore</span><strong>{snapshot.datastore || '—'}</strong></div>
        <div><span class="muted">Content-ID</span><strong title={snapshot.content_id || ''}>{snapshot.content_id || '—'}</strong></div>
        <div><span class="muted">Outcome</span><strong>{snapshot.outcome}</strong></div>
        <div><span class="muted">Executor</span><strong data-testid="evidence-executor">{snapshot.provenance?.executor || 'legacy unknown'}</strong></div>
        {#if snapshot.provenance?.observing_collector_id}
          <div><span class="muted">Observed by</span><strong>{snapshot.provenance.observing_collector_id}</strong></div>
        {/if}
        {#if snapshot.provenance?.discovery_job_id}
          <div><span class="muted">Job</span><strong class="mono-sm" title={snapshot.provenance.discovery_job_id}>{snapshot.provenance.discovery_job_id.slice(0, 8)}…</strong></div>
        {/if}
        <div><span class="muted">Catalogue</span><strong>{snapshot.catalogue_state ? snapshot.catalogue_state.replace(/_/g, ' ') : 'verified complete'}</strong></div>
      </div>
      {#if snapshot.freshness !== 'fresh' || (snapshot.freshness_reasons || []).length > 0}
        <div class="notice warn" data-testid="freshness-reasons">
          {#if snapshot.freshness !== 'fresh'}Evidence is {snapshot.freshness} — conclusions are uncertain until refreshed. {/if}
          {(snapshot.freshness_reasons || []).join(', ')}
        </div>
      {/if}
      {#if snapshot.error}
        <div class="notice error">Discovery error: {snapshot.error}</div>
      {/if}
    {/if}
  </div>

  <!-- ── 3. Schema selection (M6F-F) ────────────────────────────────── -->
  {#if schemaSel}
    <div class="card" data-testid="schema-selection">
      <div class="card-head">
        <h4>Device schema</h4>
        <span class="muted">
          {#if schemaSel.selection}
            explicit · rev {schemaSel.selection.selection_revision} · {schemaSel.selection.selection_source}
          {:else}
            default (active catalogue)
          {/if}
        </span>
      </div>
      <div class="evidence-grid">
        <div><span class="muted">Selected</span><strong class="mono-sm" title={schemaSel.selection?.schema_id || ''}>{schemaSel.selection ? shortId(schemaSel.selection.schema_id) : '—'}</strong></div>
        <div><span class="muted">Effective</span><strong class="mono-sm" title={schemaSel.effective_schema_id || ''}>{shortId(schemaSel.effective_schema_id)}</strong></div>
        <div><span class="muted">Active catalogue</span><strong class="mono-sm" title={schemaSel.active_schema_id || ''}>{shortId(schemaSel.active_schema_id)}</strong></div>
        <div><span class="muted">Catalogue state</span><strong>{schemaSel.catalogue_state ? schemaSel.catalogue_state.replace(/_/g, ' ') : 'verified complete'}</strong></div>
      </div>
      {#if isAdmin}
        <div class="filters" style="margin-top: 0.75rem;">
          <select aria-label="Compiled schema bundle" bind:value={schemaSelChoice} data-testid="schema-bundle-select">
            <option value="">— choose verified bundle —</option>
            {#each schemaBundles as b}
              <option value={b.schema_id}>{shortId(b.schema_id)} · {b.module_count} modules · {b.path_count} paths{b.active ? ' · active' : ''}</option>
            {/each}
          </select>
          <button
            class="btn"
            disabled={!schemaSelChoice || schemaSelBusy || schemaSelChoice === schemaSel?.selection?.schema_id}
            onclick={() => { confirmSchemaSel = true; }}
            data-testid="schema-select-apply"
          >Select schema</button>
          {#if schemaSel.selection}
            <button
              class="btn"
              disabled={schemaSelBusy}
              onclick={() => applySchemaSelection(true)}
              data-testid="schema-select-clear"
            >Clear (use active)</button>
          {/if}
        </div>
        {#if confirmSchemaSel}
          <div class="notice warn" role="alert" data-testid="schema-select-confirm">
            Bind this device to the selected schema? Approvals derived from a
            different schema will be revoked; other devices are unaffected.
            <button class="btn" disabled={schemaSelBusy} onclick={() => applySchemaSelection(false)}>Confirm</button>
            <button class="btn" onclick={() => { confirmSchemaSel = false; }}>Cancel</button>
          </div>
        {/if}
      {/if}
      {#if schemaSelMessage}
        <div class="notice info" role="status" data-testid="schema-select-message">{schemaSelMessage}</div>
      {/if}
      {#if schemaSelError}
        <div class="notice error" role="alert" data-testid="schema-select-error">{schemaSelError}</div>
      {/if}
    </div>
  {/if}

  <!-- ── 4. Module browser ──────────────────────────────────────────── -->
  {#if modules.length > 0}
    <div class="card" data-testid="module-browser">
      <h4>Modules ({modules.length})</h4>
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Module</th><th>Revision</th><th>Classification</th>
              <th>Features</th><th>Status</th><th>Reasons</th>
            </tr>
          </thead>
          <tbody>
            {#each modules as m}
              <tr>
                <td>
                  <strong>{m.name}</strong>
                  <div class="muted mono-sm">{m.namespace}</div>
                  {#if m.organization}<div class="muted mono-sm">{m.organization}</div>{/if}
                </td>
                <td class="mono-sm">{m.device_revision || m.catalogue_revision || '—'}</td>
                <td>{m.classification}</td>
                <td class="mono-sm">
                  {#if (m.enabled_features || []).length > 0}
                    {m.enabled_features.join(', ')}
                  {:else}
                    <span class="muted">{m.features_state_known ? 'none' : 'unknown'}</span>
                  {/if}
                </td>
                <td><span class="chip status-{statusClass(m.status)}">{m.status}</span></td>
                <td class="mono-sm muted">{(m.reasons || []).join(', ')}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    </div>
  {/if}

  <!-- ── 4. Path browser ────────────────────────────────────────────── -->
  <div class="card" data-testid="path-browser">
    <div class="card-head">
      <h4>Device-effective paths</h4>
      <span class="muted">
        {pathsMatching} match{pathsMatching === 1 ? '' : 'es'}
        {#if snapshot?.result?.paths_truncated || applicability?.snapshot?.paths_truncated}· catalogue truncated{/if}
      </span>
    </div>
    <div class="filters" role="search">
      <input
        type="search"
        placeholder="Search path, module, namespace, type…"
        aria-label="Search schema paths"
        value={filters.q}
        oninput={(e) => onFilter('q', e.currentTarget.value)}
        data-testid="path-search"
      />
      <select aria-label="Applicability status" value={filters.status} onchange={(e) => onFilter('status', e.currentTarget.value)} data-testid="filter-status">
        <option value="">All statuses</option>
        {#each STATUSES as s}<option value={s}>{s.replace(/_/g, ' ')}</option>{/each}
      </select>
      <select aria-label="Module" value={filters.module} onchange={(e) => onFilter('module', e.currentTarget.value)} data-testid="filter-module">
        <option value="">All modules</option>
        {#each modules as m}<option value={m.name}>{m.name}</option>{/each}
      </select>
      <select aria-label="Data kind" value={filters.kind} onchange={(e) => onFilter('kind', e.currentTarget.value)} data-testid="filter-kind">
        <option value="">All kinds</option>
        {#each KINDS as k}<option value={k}>{k}</option>{/each}
      </select>
      <select aria-label="Config or state" value={filters.config} onchange={(e) => onFilter('config', e.currentTarget.value)} data-testid="filter-config">
        <option value="">Config + state</option>
        <option value="false">State only</option>
        <option value="true">Config only</option>
      </select>
      <input
        type="text"
        placeholder="Required feature"
        aria-label="Required feature"
        value={filters.feature}
        oninput={(e) => onFilter('feature', e.currentTarget.value)}
        data-testid="filter-feature"
      />
    </div>
    {#if appState === 'loading'}
      <p class="muted">Loading paths…</p>
    {:else if pathRows.length === 0}
      <p class="muted" data-testid="paths-empty">
        {snapshot ? 'No paths match the current filters.' : 'No applicability evidence yet.'}
      </p>
    {:else}
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Schema path</th><th>Module</th><th>Kind</th><th>Config</th>
              <th>Type</th><th>Keys</th><th>Features</th><th>Status</th><th></th>
            </tr>
          </thead>
          <tbody>
            {#each pathRows as p}
              <tr data-testid="path-row">
                <td class="mono-sm path-cell" title={p.path}>{p.path}</td>
                <td class="mono-sm muted">{p.module}{p.module_revision ? ' · ' + p.module_revision : ''}</td>
                <td>{p.kind}</td>
                <td>{p.config ? 'config' : 'state'}</td>
                <td class="mono-sm">{p.type_name || p.resolved_type || '—'}{p.units ? ' (' + p.units + ')' : ''}</td>
                <td class="mono-sm muted">{(p.keys || []).join(', ') || '—'}</td>
                <td class="mono-sm muted">{(p.required_features || []).join(', ') || '—'}</td>
                <td>
                  <span class="chip status-{statusClass(p.status)}" title={(p.reasons || []).join(', ')}>{p.status.replace(/_/g, ' ')}</span>
                </td>
                <td>
                  <button
                    class="ghost sm"
                    disabled={p.status !== 'applicable' && p.status !== 'candidate'}
                    title={p.status === 'applicable' || p.status === 'candidate' ? 'Add to draft' : 'Not selectable in current evidence state'}
                    onclick={() => addPathFromBrowser(p)}
                    data-testid="select-path"
                  >Select</button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
      <div class="pager">
        <button class="ghost sm" disabled={!hasPrevPage} onclick={() => page(-1)} data-testid="paths-prev">← Prev</button>
        <span class="muted">{pathOffset + 1}–{Math.min(pathOffset + PAGE_SIZE, pathsMatching)} of {pathsMatching}</span>
        <button class="ghost sm" disabled={!hasNextPage} onclick={() => page(1)} data-testid="paths-next">Next →</button>
      </div>
    {/if}
  </div>

  <!-- ── 5. Selected subscription draft ─────────────────────────────── -->
  <div class="card" data-testid="draft-panel">
    <div class="card-head">
      <h4>Selected draft ({draft.length})</h4>
      <span class="muted">
        {readyCount} ready{unresolvedCount > 0 ? ` · ${unresolvedCount} need keys/invalid` : ''}
        · draft is operator intent — not yet approved or executed
      </span>
    </div>
    {#if draft.length === 0}
      <p class="muted">No paths selected. Use “Select” in the path browser or add a manual gNMI path.</p>
    {:else}
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Path</th><th>Origin</th><th>Mode</th><th>Interval</th>
              <th>Provenance</th><th>Keys</th><th></th>
            </tr>
          </thead>
          <tbody>
            {#each draft as entry (entry.id + ':' + entry.path)}
              <tr class={!isEntryReady(entry) ? 'row-warn' : ''} data-testid="draft-row">
                <td class="mono-sm path-cell" title={entry.path}>
                  {entry.path}
                  {#if entry.template !== entry.path}<div class="muted mono-sm">template: {entry.template}</div>{/if}
                  {#if entry.warning}<div class="warn-text" data-testid="draft-warning">{entry.warning}</div>{/if}
                </td>
                <td>
                  <input
                    class="cell-input"
                    value={entry.origin}
                    placeholder="(device default)"
                    aria-label="gNMI origin for {entry.path}"
                    oninput={(e) => updateEntry(entry, { origin: e.currentTarget.value })}
                    data-testid="draft-origin"
                  />
                </td>
                <td>
                  <select
                    aria-label="Mode for {entry.path}"
                    value={entry.mode}
                    onchange={(e) => setMode(entry, e.currentTarget.value)}
                    data-testid="draft-mode"
                  >
                    <option value="SAMPLE">SAMPLE</option>
                    <option value="ON_CHANGE">ON_CHANGE</option>
                  </select>
                </td>
                <td>
                  {#if entry.mode === 'SAMPLE'}
                    <input
                      class="cell-input"
                      type="number"
                      min="0"
                      step="any"
                      value={nsToSeconds(entry.sample_interval_ns)}
                      aria-label="Sample interval seconds for {entry.path}"
                      onchange={(e) => setIntervalSeconds(entry, e.currentTarget.value)}
                      data-testid="draft-interval"
                    />
                  {:else}
                    <span class="muted">—</span>
                  {/if}
                </td>
                <td class="mono-sm muted">{entry.provenance || '—'}</td>
                <td>
                  {#if templateKeys(entry.template).length > 0}
                    <div class="key-inputs">
                      {#each templateKeys(entry.template) as k (keyId(k.segmentIndex, k.key))}
                        <label class="key-field" title={k.segment}>
                          <span class="muted">{k.key}{templateKeys(entry.template).filter((o) => o.key === k.key).length > 1 ? ' §' + (k.segmentIndex + 1) : ''}</span>
                          <input
                            value={entry.keyValues[keyId(k.segmentIndex, k.key)] || ''}
                            placeholder={k.key}
                            aria-label="List key {k.key} (segment {k.segmentIndex + 1}) for {entry.template}"
                            oninput={(e) => {
                              entry.keyValues = { ...entry.keyValues, [keyId(k.segmentIndex, k.key)]: e.currentTarget.value };
                              updateEntry(entry, { keyValues: entry.keyValues });
                            }}
                            data-testid="draft-key"
                          />
                        </label>
                      {/each}
                    </div>
                  {:else}
                    <span class="muted">—</span>
                  {/if}
                </td>
                <td>
                  <button class="ghost sm danger" onclick={() => removeEntry(entry)} aria-label="Remove {entry.path}" data-testid="draft-remove">✕</button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </div>

  <!-- ── 6. Plan preview and diff ───────────────────────────────────── -->
  <div class="card" data-testid="preview-panel">
    <div class="card-head">
      <h4>Plan preview</h4>
      <div class="head-actions">
        <label class="muted">Datastore
          <input
            class="cell-input"
            value={datastore}
            placeholder="ietf-datastores:operational"
            aria-label="Datastore"
            oninput={(e) => { datastore = e.currentTarget.value; invalidatePreview(); }}
            data-testid="datastore-input"
          />
        </label>
        <button
          class="primary"
          disabled={previewBusy || readyCount === 0}
          onclick={runPreview}
          data-testid="run-preview"
        >{previewBusy ? 'Evaluating…' : 'Preview plan'}</button>
      </div>
    </div>
    {#if previewError}
      <div class="notice error" role="alert" data-testid="preview-error">{previewError}</div>
    {/if}
    {#if previewStale && preview}
      <div class="notice warn" data-testid="preview-stale">Draft or device state changed — this preview is stale. Preview again before activating.</div>
    {/if}
    {#if preview}
      <div class="evidence-grid" data-testid="preview-summary">
        <div><span class="muted">Activation</span><strong class={preview.evaluation?.activation_permitted ? 'status-ok' : 'status-bad'}>{preview.evaluation?.activation_permitted ? 'permitted' : 'blocked'}</strong></div>
        <div><span class="muted">Plan</span><strong class="mono-sm" title={preview.evaluation?.plan_id}>{shortId(preview.evaluation?.plan_id)}</strong></div>
        <div><span class="muted">Binding</span><strong class="mono-sm" title={preview.evaluation?.binding_fingerprint}>{shortId(preview.evaluation?.binding_fingerprint)}</strong></div>
        <div><span class="muted">Schema</span><strong class="mono-sm" title={preview.evaluation?.schema_id}>{shortId(preview.evaluation?.schema_id)}</strong></div>
        <div><span class="muted">Evidence</span><strong class="mono-sm" title={preview.evaluation?.evidence_fingerprint}>{shortId(preview.evaluation?.evidence_fingerprint)}</strong></div>
        <div><span class="muted">Config revision</span><strong class="mono-sm">{preview.config_revision}</strong></div>
        <div><span class="muted">Collector</span><strong>{preview.evaluation?.collector_id || device?.collector_id || '—'}</strong></div>
        <div><span class="muted">Mode</span><strong>{preview.evaluation?.acquisition_mode || 'explicit'}</strong></div>
      </div>
      {#if (preview.evaluation?.rejection_reasons || []).length > 0}
        <div class="notice error" data-testid="preview-rejections">
          <strong>Blocked:</strong>
          <ul>{#each preview.evaluation.rejection_reasons as r}<li class="mono-sm">{r}</li>{/each}</ul>
        </div>
      {/if}
      {#if (preview.evaluation?.warnings || []).length > 0}
        <div class="notice warn" data-testid="preview-warnings">
          <ul>{#each preview.evaluation.warnings as w}<li class="mono-sm">{w}</li>{/each}</ul>
        </div>
      {/if}
      {#if eval_?.estimated_initial_collection}
        <p class="muted" data-testid="preview-estimate">
          Estimate ({eval_.estimated_initial_collection.kind}):
          {eval_.estimated_initial_collection.value} — {eval_.estimated_initial_collection.basis}
          {#if eval_.estimated_ongoing_collection}
            · ongoing ({eval_.estimated_ongoing_collection.kind}): {eval_.estimated_ongoing_collection.value} — {eval_.estimated_ongoing_collection.basis}
          {/if}
        </p>
      {/if}
      <!-- Diff vs current selection -->
      <div class="diff" data-testid="preview-diff">
        <h5>Current → proposed</h5>
        <p class="muted">
          +{diff.added.length} added · −{diff.removed.length} removed ·
          {diff.changed.length} modified · {eval_?.proposed_subscriptions?.length ?? 0} proposed subscriptions
        </p>
        {#if diff.added.length + diff.removed.length + diff.changed.length > 0}
          <ul class="diff-list">
            {#each diff.added as p}<li class="add">+ {p.path} <span class="muted">({p.mode}{p.sample_interval_ns ? ' ' + formatInterval(p.sample_interval_ns) : ''})</span></li>{/each}
            {#each diff.removed as p}<li class="del">− {p.path} <span class="muted">({p.mode}{p.sample_interval_ns ? ' ' + formatInterval(p.sample_interval_ns) : ''})</span></li>{/each}
            {#each diff.changed as p}<li class="chg">~ {p.path} {p.changes}</li>{/each}
          </ul>
        {/if}
      </div>
      <!-- Per-path decisions -->
      {#if (eval_?.decisions || []).length > 0}
        <div class="table-wrap">
          <table data-testid="decision-table">
            <thead><tr><th>Path</th><th>Decision</th><th>Schema path</th><th>Reasons</th><th>Warnings</th></tr></thead>
            <tbody>
              {#each eval_.decisions as d}
                <tr>
                  <td class="mono-sm path-cell">{d.selected_path || '—'}</td>
                  <td><span class="chip">{decisionKindLabel(d.kind)}</span></td>
                  <td class="mono-sm muted">{d.schema_path || '—'}</td>
                  <td class="mono-sm muted">{(d.reasons || []).join(', ') || '—'}</td>
                  <td class="mono-sm muted">{(d.warnings || []).join(', ') || '—'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
      <!-- Exact proposed subscriptions -->
      {#if (eval_?.proposed_subscriptions || []).length > 0}
        <details class="proposed" data-testid="proposed-subscriptions">
          <summary>Exact proposed subscriptions ({eval_.proposed_subscriptions.length})</summary>
          <ul class="mono-sm">
            {#each eval_.proposed_subscriptions as p}
              <li>{p.origin || '(default)'} {p.path} — {p.mode}{p.sample_interval_ns ? ' @ ' + formatInterval(p.sample_interval_ns) : ''}{p.provenance ? ' · ' + p.provenance : ''}</li>
            {/each}
          </ul>
        </details>
      {/if}
      <!-- Activation -->
      <div class="activate-row">
        {#if confirmActivate}
          <div class="notice warn confirm" data-testid="activate-confirm">
            <p>
              <strong>Approve schema-bound plan?</strong><br/>
              Device {device?.address} · collector {eval_?.collector_id || device?.collector_id || '—'} ·
              {eval_?.proposed_subscriptions?.length ?? 0} paths ·
              schema {shortId(eval_?.schema_id)} ·
              evidence {shortId(eval_?.evidence_fingerprint)}<br/>
              {#if binding}This replaces the currently approved binding (rev {binding.approval_revision}). {/if}
              {#if (diff.added.length + diff.removed.length + diff.changed.length) > 0}
                The running plan will change (+{diff.added.length}/−{diff.removed.length}/{diff.changed.length} modified).
              {/if}
            </p>
            <div class="head-actions">
              <button class="primary" disabled={activateBusy} onclick={runActivate} data-testid="confirm-activate">
                {activateBusy ? 'Activating…' : 'Confirm activation'}
              </button>
              <button class="ghost" onclick={() => { confirmActivate = false; }} data-testid="cancel-activate">Cancel</button>
            </div>
          </div>
        {:else}
          <button
            class="primary"
            disabled={!canActivate || (me !== null && !isAdmin)}
            title={me !== null && !isAdmin ? 'Activation requires an admin role' : !preview ? 'Run a preview first' : previewStale ? 'Preview is stale — preview again' : !preview.evaluation?.activation_permitted ? 'Server evaluation blocks activation' : 'Activate approved plan'}
            onclick={() => { confirmActivate = true; }}
            data-testid="activate-button"
          >Activate approved plan</button>
        {/if}
      </div>
    {:else if !previewBusy}
      <p class="muted">Run a preview to evaluate the current draft against device evidence. Preview is read-only.</p>
    {/if}
  </div>

  <!-- Persistent action status — survives preview/binding state changes
       so a 409 or post-activation result is never hidden by a rerender. -->
  {#if activateMessage}
    <div class="notice ok" role="status" data-testid="activate-message">{activateMessage}</div>
  {/if}
  {#if activateError}
    <div class="notice error" role="alert" data-testid="activate-error">{activateError}</div>
  {/if}
  {#if revokeMessage}
    <div class="notice {revokeMessage.startsWith('Forbidden') ? 'error' : 'ok'}" role="status" data-testid="revoke-message">{revokeMessage}</div>
  {/if}

  <!-- ── 7. Approval / revocation ────────────────────────────────────── -->
  {#if binding}
    <div class="card" data-testid="binding-panel">
      <div class="card-head">
        <h4>Approved binding</h4>
        <button
          class="ghost danger"
          disabled={!isAdmin && me !== null}
          title={isAdmin ? 'Revoke the schema-bound approval' : 'Requires admin role'}
          onclick={() => { confirmRevoke = true; }}
          data-testid="revoke-button"
        >Revoke approval</button>
      </div>
      {#if confirmRevoke}
        <div class="notice warn confirm" data-testid="revoke-confirm">
          <p>
            <strong>Revoke schema-bound acquisition?</strong><br/>
            This disables the current approved authority (plan {shortId(binding.plan_id)},
            rev {binding.approval_revision}). It does <em>not</em> restore a previous
            collection plan — the device keeps its acquisition intent but the approved
            manifest is withdrawn and collectors resync.
          </p>
          <label class="muted">Reason
            <input
              class="cell-input"
              bind:value={revokeReason}
              placeholder="operator revocation"
              aria-label="Revocation reason"
              data-testid="revoke-reason"
            />
          </label>
          <div class="head-actions">
            <button class="primary danger" disabled={revokeBusy} onclick={runRevoke} data-testid="confirm-revoke">
              {revokeBusy ? 'Revoking…' : 'Confirm revocation'}
            </button>
            <button class="ghost" onclick={() => { confirmRevoke = false; }} data-testid="cancel-revoke">Cancel</button>
          </div>
        </div>
      {/if}
    </div>
  {/if}

  <!-- ── 8. Execution and coverage ───────────────────────────────────── -->
  <div class="card" data-testid="execution-panel">
    <div class="card-head">
      <h4>Execution status</h4>
      <span class="muted">Desired → approved → assigned → submitted → synchronized → observed are separate stages.</span>
    </div>
    {#if (device?.subscription_statuses || []).length === 0}
      <p class="muted" data-testid="execution-empty">
        {binding ? 'Approved, but no subscription execution evidence yet — the collector has not reported a submitted manifest.' : 'No subscription execution evidence.'}
      </p>
    {:else}
      <div class="table-wrap">
        <table>
          <thead>
            <tr><th>Path</th><th>Mode</th><th>Execution</th><th>First observed</th><th>Last observed</th></tr>
          </thead>
          <tbody>
            {#each device.subscription_statuses as s}
              <tr data-testid="subscription-row">
                <td class="mono-sm path-cell" title={s.path}>{s.origin ? s.origin + ' ' : ''}{s.path}</td>
                <td>{s.mode}{s.sample_interval_ns ? ' ' + formatInterval(s.sample_interval_ns) : ''}</td>
                <td><span class="chip status-{subscriptionStatusClass(s.status)}">{(s.status || 'unknown').replace(/_/g, ' ')}</span></td>
                <td title={absoluteTime(s.first_observed_at_ns)}>{s.first_observed_at_ns ? relativeTime(s.first_observed_at_ns) : '—'}</td>
                <td title={absoluteTime(s.last_observed_at_ns)}>{s.last_observed_at_ns ? relativeTime(s.last_observed_at_ns) : '—'}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
      <p class="muted">
        “Observed” requires matching telemetry; approval alone never implies execution
        or receipt. Paths not yet observed stay pending/stale — they are not failures
        unless the backend reports one.
      </p>
    {/if}
  </div>
</section>

<style>
  .workspace { display: flex; flex-direction: column; gap: 12px; }
  .ws-header h3 { margin: 0 0 4px; }
  .ws-header p { margin: 0 0 8px; font-size: var(--text-small); }
  .card {
    background: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-lg);
    padding: var(--card-pad);
  }
  .card h4 { margin: 0 0 10px; font-size: var(--text-heading-2); }
  .card h5 { margin: 12px 0 6px; font-size: var(--text-body); }
  .card-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; flex-wrap: wrap; }
  .head-actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .ctx-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(140px, 1fr)); gap: 10px; }
  .ctx-grid span, .evidence-grid span { display: block; font-size: var(--text-xs); }
  .evidence-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 10px; margin: 10px 0; }
  .binding-meta { margin-top: 8px; font-size: var(--text-xs); }
  .filters { display: flex; gap: 8px; flex-wrap: wrap; margin-bottom: 10px; }
  .filters input[type="search"] { flex: 1 1 220px; }
  .filters input, .filters select, .cell-input {
    background: var(--bg-input);
    border: 1px solid var(--border-default);
    border-radius: var(--radius-sm);
    color: var(--text-primary);
    padding: 5px 8px;
    font-size: var(--text-small);
  }
  .cell-input { width: 100%; min-width: 90px; }
  .table-wrap { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; font-size: var(--text-small); }
  th { text-align: left; color: var(--text-secondary); font-weight: 600; padding: 6px 8px; border-bottom: 1px solid var(--border-subtle); white-space: nowrap; }
  td { padding: 6px 8px; border-bottom: 1px solid var(--border-subtle); vertical-align: top; }
  tr:last-child td { border-bottom: none; }
  .row-warn td { background: var(--state-degraded-bg); }
  .path-cell { max-width: 340px; word-break: break-all; }
  .mono-sm { font-family: var(--font-mono); font-size: var(--text-mono-sm); }
  .muted { color: var(--text-secondary); }
  .warn-text { color: var(--state-degraded); font-size: var(--text-xs); }
  .chip {
    display: inline-block; padding: 1px 8px; border-radius: var(--radius-pill);
    font-size: var(--text-xs); border: 1px solid var(--border-subtle);
    background: var(--state-neutral-bg); color: var(--text-secondary);
    white-space: nowrap;
  }
  .chip.status-ok, .status-ok { color: var(--state-healthy); }
  .chip.status-warn { color: var(--state-degraded); }
  .chip.status-bad { color: var(--state-failed); }
  .chip.status-info { color: var(--state-info); }
  .notice { border-radius: var(--radius-md); padding: 8px 12px; margin: 8px 0; font-size: var(--text-small); border: 1px solid var(--border-subtle); }
  .notice.error { background: var(--state-failed-bg); border-color: var(--state-failed-border); color: var(--state-failed); }
  .notice.warn { background: var(--state-degraded-bg); border-color: var(--state-degraded-border); color: var(--state-degraded); }
  .notice.ok { background: var(--state-healthy-bg); border-color: var(--state-healthy-border); color: var(--state-healthy); }
  .notice ul { margin: 4px 0 0; padding-left: 18px; }
  .pager { display: flex; align-items: center; justify-content: space-between; padding: 8px 4px 0; }
  .key-inputs { display: flex; flex-direction: column; gap: 4px; }
  .key-field { display: flex; flex-direction: column; gap: 2px; font-size: var(--text-xs); }
  .key-field input { min-width: 140px; }
  .diff-list { list-style: none; padding: 0; margin: 8px 0; }
  .diff-list li { padding: 2px 0; font-family: var(--font-mono); font-size: var(--text-mono-sm); }
  .diff-list .add { color: var(--state-healthy); }
  .diff-list .del { color: var(--state-failed); }
  .diff-list .chg { color: var(--state-degraded); }
  .proposed { margin-top: 8px; }
  .proposed ul { margin: 6px 0; padding-left: 18px; }
  .activate-row { margin-top: 12px; display: flex; flex-direction: column; gap: 8px; align-items: flex-start; }
  .confirm { max-width: 640px; }
  .confirm p { margin: 0 0 8px; }
  button.primary {
    background: var(--accent-muted); color: var(--text-on-accent);
    border: none; border-radius: var(--radius-sm); padding: 7px 14px;
    font-size: var(--text-small); cursor: pointer;
  }
  button.primary:hover:not(:disabled) { background: var(--accent-hover); }
  button.primary:disabled { opacity: 0.45; cursor: not-allowed; }
  button.primary.danger { background: var(--state-failed); }
  button.ghost {
    background: transparent; color: var(--text-secondary);
    border: 1px solid var(--border-default); border-radius: var(--radius-sm);
    padding: 5px 10px; font-size: var(--text-small); cursor: pointer;
  }
  button.ghost:hover:not(:disabled) { background: var(--bg-hover); color: var(--text-primary); }
  button.ghost:disabled { opacity: 0.45; cursor: not-allowed; }
  button.ghost.sm { padding: 2px 8px; font-size: var(--text-xs); }
  button.ghost.danger:hover:not(:disabled) { color: var(--state-failed); border-color: var(--state-failed-border); }
  .filters select { min-width: 120px; }
</style>
