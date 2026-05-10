<script>
  import { onMount } from 'svelte';
  import { relativeTime, absoluteTime, shortTime } from '$lib/timeutil.js';
  import { navigate } from '$lib/router.svelte.js';

  let { address, onclose } = $props();

  let device = $state(null);
  let loading = $state(true);
  let error = $state(null);
  let activeTab = $state('interfaces');

  let enrichmentProps = $state([]);
  let enrichmentLoading = $state(false);
  let configHistory = $state({ snapshots: [], changes: [] });
  let configLoading = $state(false);
  let readiness = $state(null);
  let readinessLoading = $state(false);
  let recommendations = $state(null);
  let recommendationsLoading = $state(false);
  let recommendationDraft = $state([]);
  let recommendationDirty = $state(false);
  let recommendationSaveBusy = $state(false);
  let recommendationSaveMessage = $state('');
  let customPath = $state('');
  let customMode = $state('SAMPLE');
  let customSampleSeconds = $state('10');
  let reparseBusy = $state(false);
  let reparseMessage = $state('');

  const TABS = ['interfaces', 'peers', 'paths', 'recommendations', 'events', 'detections', 'readiness', 'config', 'enrichment', 'audit'];

  $effect(() => {
    if (address) {
      loading = true;
      error = null;
      device = null;
      enrichmentProps = [];
      configHistory = { snapshots: [], changes: [] };
      readiness = null;
      recommendations = null;
      recommendationDraft = [];
      recommendationDirty = false;
      recommendationSaveMessage = '';
      customPath = '';
      customMode = 'SAMPLE';
      customSampleSeconds = '10';
      reparseMessage = '';
      fetch('/api/devices/' + encodeURIComponent(address))
        .then(r => r.ok ? r.json() : r.text().then(t => { throw new Error(t); }))
        .then(d => { device = d; loading = false; })
        .catch(e => { error = e.message; loading = false; });
    }
  });

  $effect(() => {
    if (activeTab === 'enrichment' && address && !enrichmentLoading && enrichmentProps.length === 0) {
      enrichmentLoading = true;
      fetch('/api/devices/' + encodeURIComponent(address) + '/enrichment')
        .then(r => r.ok ? r.json() : r.text().then(t => { throw new Error(t); }))
        .then(d => { enrichmentProps = d.properties || []; enrichmentLoading = false; })
        .catch(() => { enrichmentLoading = false; });
    }
  });

  $effect(() => {
    if (activeTab === 'recommendations' && address && !recommendationsLoading && !recommendations) {
      recommendationsLoading = true;
      fetch('/api/devices/' + encodeURIComponent(address) + '/recommendations')
        .then(r => r.ok ? r.json() : r.text().then(t => { throw new Error(t); }))
        .then(d => {
          recommendations = d.report;
          const current = Array.isArray(device?.selected_paths) && device.selected_paths.length > 0
            ? device.selected_paths
            : (d.report?.recommended_paths || []).map(path => ({
                path: path.path,
                origin: path.origin,
                mode: path.mode,
                sample_interval_ns: path.sample_interval_ns,
                rationale: path.rationale,
                optional: !!path.optional
              }));
          recommendationDraft = dedupeSelectedPaths(current);
          recommendationDirty = false;
          recommendationSaveMessage = '';
          recommendationsLoading = false;
        })
        .catch(() => { recommendationsLoading = false; });
    }
  });

  $effect(() => {
    if (activeTab === 'readiness' && address && !readinessLoading && !readiness) {
      readinessLoading = true;
      fetch('/api/devices/' + encodeURIComponent(address) + '/gnmi-readiness')
        .then(r => r.ok ? r.json() : r.text().then(t => { throw new Error(t); }))
        .then(d => { readiness = d.report; readinessLoading = false; })
        .catch(() => { readinessLoading = false; });
    }
  });

  $effect(() => {
    if (activeTab === 'config' && address && !configLoading && configHistory.snapshots.length === 0 && configHistory.changes.length === 0) {
      configLoading = true;
      fetch('/api/devices/' + encodeURIComponent(address) + '/config-history')
        .then(r => r.ok ? r.json() : r.text().then(t => { throw new Error(t); }))
        .then(d => {
          configHistory = {
            snapshots: d.snapshots || [],
            changes: d.changes || []
          };
          configLoading = false;
        })
        .catch(() => { configLoading = false; });
    }
  });

  // Group enrichment properties by source_name
  function groupedEnrichment() {
    const groups = {};
    for (const p of enrichmentProps) {
      (groups[p.source_name] ||= []).push(p);
    }
    return Object.entries(groups);
  }

  function healthClass(h) {
    if (h === 'healthy') return 'healthy';
    if (h === 'critical') return 'critical';
    return 'warn';
  }

  function fmtBytes(n) {
    if (!n) return '—';
    if (n < 1024) return n + ' B';
    if (n < 1048576) return (n / 1024).toFixed(1) + ' KB';
    if (n < 1073741824) return (n / 1048576).toFixed(1) + ' MB';
    return (n / 1073741824).toFixed(2) + ' GB';
  }

  function pathKey(path) {
    return (path.path || '') + '::' + (path.mode || '');
  }

  function dedupeSelectedPaths(paths) {
    const seen = new Set();
    const result = [];
    for (const path of paths || []) {
      const normalized = {
        path: path.path || '',
        origin: path.origin || '',
        mode: path.mode || 'SAMPLE',
        sample_interval_ns: Number(path.sample_interval_ns || 0),
        rationale: path.rationale || '',
        optional: !!path.optional
      };
      const key = pathKey(normalized);
      if (!normalized.path || seen.has(key)) continue;
      seen.add(key);
      result.push(normalized);
    }
    return result;
  }

  function recommendationChecked(path) {
    const key = pathKey(path);
    return recommendationDraft.some(entry => pathKey(entry) === key);
  }

  function toggleRecommendation(path, checked) {
    const key = pathKey(path);
    if (checked) {
      recommendationDraft = dedupeSelectedPaths([
        ...recommendationDraft,
        {
          path: path.path,
          origin: path.origin || '',
          mode: path.mode || 'SAMPLE',
          sample_interval_ns: Number(path.sample_interval_ns || 0),
          rationale: path.rationale || '',
          optional: !!path.optional
        }
      ]);
    } else {
      recommendationDraft = recommendationDraft.filter(entry => pathKey(entry) !== key);
    }
    recommendationDirty = true;
    recommendationSaveMessage = '';
  }

  function removeDraftPath(path) {
    const key = pathKey(path);
    recommendationDraft = recommendationDraft.filter(entry => pathKey(entry) !== key);
    recommendationDirty = true;
    recommendationSaveMessage = '';
  }

  function addCustomDraftPath() {
    const trimmed = customPath.trim();
    if (!trimmed) return;
    recommendationDraft = dedupeSelectedPaths([
      ...recommendationDraft,
      {
        path: trimmed,
        origin: 'manual_override',
        mode: customMode,
        sample_interval_ns: Number(customSampleSeconds || '0') * 1_000_000_000,
        rationale: 'Added manually from recommendations panel.',
        optional: false
      }
    ]);
    recommendationDirty = true;
    recommendationSaveMessage = '';
    customPath = '';
  }

  async function saveRecommendationDraft() {
    if (!address || recommendationSaveBusy) return;
    recommendationSaveBusy = true;
    recommendationSaveMessage = '';
    try {
      const response = await fetch('/api/devices/' + encodeURIComponent(address) + '/selected-paths', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ selected_paths: recommendationDraft })
      });
      const payload = await response.json();
      if (!response.ok || payload.success === false) {
        throw new Error(payload.error || 'Failed to save selected paths');
      }
      recommendationDraft = dedupeSelectedPaths(payload.selected_paths || recommendationDraft);
      recommendationDirty = false;
      recommendationSaveMessage = 'Selected paths saved';
      if (device) {
        device = { ...device, selected_paths: recommendationDraft };
      }
    } catch (error) {
      recommendationSaveMessage = error.message || 'Failed to save selected paths';
    } finally {
      recommendationSaveBusy = false;
    }
  }

  async function triggerReparse() {
    if (!address || reparseBusy) return;
    reparseBusy = true;
    reparseMessage = '';
    try {
      const response = await fetch('/api/devices/' + encodeURIComponent(address) + '/reparse', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ reason: 'Device drawer manual re-parse' })
      });
      const payload = await response.json();
      reparseMessage = payload.message || 'Re-parse requested';
      if (payload.success) {
        configHistory = { snapshots: [], changes: [] };
      }
    } catch {
      reparseMessage = 'Failed to queue re-parse';
    } finally {
      reparseBusy = false;
    }
  }
</script>

<button
  class="drawer-backdrop"
  onclick={onclose}
  aria-label="Close drawer backdrop"
></button>

<aside class="drawer">
  <div class="drawer-header">
    {#if loading}
      <div class="drawer-title-skeleton"></div>
    {:else if device}
      <div class="drawer-title">
        <span class="badge {healthClass(device.health)}" style="margin-right:8px;">{device.health}</span>
        <strong>{device.hostname || device.address}</strong>
        {#if device.hostname && device.hostname !== device.address}
          <span class="muted" style="font-size:12px; margin-left:6px;">{device.address}</span>
        {/if}
      </div>
      <div class="drawer-meta">
        {#if device.vendor}<span class="meta-chip">{device.vendor}</span>{/if}
        {#if device.role}<span class="meta-chip">{device.role}</span>{/if}
        {#if device.site}<span class="meta-chip">📍 {device.site}</span>{/if}
        {#if device.collector_id}<span class="meta-chip">⇄ {device.collector_id}</span>{/if}
      </div>
    {:else if error}
      <div class="drawer-title muted">Error loading device</div>
    {/if}
    <button class="drawer-close ghost" onclick={onclose} aria-label="Close drawer">✕</button>
  </div>

  {#if error}
    <div class="notice error" style="margin: 12px 16px;">{error}</div>
  {:else if loading}
    <div class="drawer-loading">
      {#each [1, 2, 3] as _}
        <div class="skeleton-line"></div>
      {/each}
    </div>
  {:else if device}
    <div class="drawer-tabs">
      {#each TABS as tab}
        <button class:active={activeTab === tab} onclick={() => (activeTab = tab)}>
          {tab}
        </button>
      {/each}
    </div>

    <div class="drawer-body">
      {#if activeTab === 'interfaces'}
        {#if device.interfaces.length === 0}
          <div class="empty">No interfaces recorded yet.</div>
        {:else}
          <table>
            <thead>
              <tr>
                <th>Name</th>
                <th>In errors</th>
                <th>Out errors</th>
                <th>In octets</th>
                <th>Out octets</th>
                <th>Updated</th>
              </tr>
            </thead>
            <tbody>
              {#each device.interfaces as iface}
                <tr>
                  <td><code>{iface.name}</code></td>
                  <td class="{iface.in_errors > 0 ? 'text-warn' : ''}">{iface.in_errors}</td>
                  <td class="{iface.out_errors > 0 ? 'text-warn' : ''}">{iface.out_errors}</td>
                  <td>{fmtBytes(iface.in_octets)}</td>
                  <td>{fmtBytes(iface.out_octets)}</td>
                  <td title={absoluteTime(iface.updated_at_ns)}>{relativeTime(iface.updated_at_ns)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}

      {:else if activeTab === 'peers'}
        {#if device.bgp_neighbors.length === 0 && device.lldp_neighbors.length === 0}
          <div class="empty">No peers recorded yet.</div>
        {:else}
          {#if device.bgp_neighbors.length > 0}
            <h4 class="section-head">BGP</h4>
            <table>
              <thead><tr><th>Peer</th><th>AS</th><th>State</th></tr></thead>
              <tbody>
                {#each device.bgp_neighbors as n}
                  <tr>
                    <td><code>{n.peer}</code></td>
                    <td>{n.peer_as || '—'}</td>
                    <td>
                      <span class="badge {n.state === 'established' ? 'healthy' : 'critical'}">{n.state || 'unknown'}</span>
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
          {#if device.lldp_neighbors.length > 0}
            <h4 class="section-head" style="margin-top:16px;">LLDP</h4>
            <table>
              <thead><tr><th>Local port</th><th>Neighbor</th><th>Port ID</th></tr></thead>
              <tbody>
                {#each device.lldp_neighbors as n}
                  <tr>
                    <td><code>{n.local_if}</code></td>
                    <td>{n.system_name || n.chassis_id || '—'}</td>
                    <td><code>{n.port_id || '—'}</code></td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        {/if}

      {:else if activeTab === 'paths'}
        {#if device.resolution_audit && device.resolution_audit.length > 0}
          <div class="section">
            <h4 class="section-head">Subscription Resolution Audit</h4>
            <ul style="list-style: none; padding: 0; margin-bottom: 20px;">
              {#each device.resolution_audit as auditLine}
                <li style="font-size:12px; font-family:monospace; margin-bottom:4px; color:var(--fg-muted, #888);">&gt; {auditLine}</li>
              {/each}
            </ul>
          </div>
        {/if}
        {#if device.subscription_statuses.length === 0}
          <div class="empty">No subscription paths.</div>
        {:else}
          <table>
            <thead><tr><th>Path</th><th>Mode</th><th>Status</th><th>Last seen</th></tr></thead>
            <tbody>
              {#each device.subscription_statuses as s}
                <tr>
                  <td><code style="font-size:11px; overflow-wrap:anywhere;">{s.path}</code></td>
                  <td>{s.mode}</td>
                  <td><span class="badge {s.status === 'observed' ? 'healthy' : 'warn'}">{s.status}</span></td>
                  <td title={absoluteTime(s.last_observed_at_ns)}>{relativeTime(s.last_observed_at_ns)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}

      {:else if activeTab === 'recommendations'}
        {#if recommendationsLoading}
          <div class="empty">Building recommendations…</div>
        {:else if !recommendations}
          <div class="empty">No recommendation data yet.</div>
        {:else}
          <div class="section">
            <h4 class="section-head">Recommended Subscriptions</h4>
            <div class="audit-grid">
              <span class="muted">Status</span>
              <span>{recommendations.status}</span>
              <span class="muted">Role</span>
              <span>{recommendations.role || '—'}</span>
              <span class="muted">Environment</span>
              <span>{recommendations.environment || '—'}</span>
              <span class="muted">Vendor</span>
              <span>{recommendations.vendor || '—'}</span>
            </div>

            {#if recommendations.matched_rules?.length > 0}
              <h4 class="section-head" style="margin-top:16px;">Matched Rules</h4>
              <div class="chip-row">
                {#each recommendations.matched_rules as rule}
                  <span class="meta-chip">{rule}</span>
                {/each}
              </div>
            {/if}

            {#if recommendations.recommended_profiles?.length > 0}
              <h4 class="section-head" style="margin-top:16px;">Profiles</h4>
              <table>
                <thead><tr><th>Profile</th><th>Rule</th><th>Paths</th><th>Confidence</th></tr></thead>
                <tbody>
                  {#each recommendations.recommended_profiles as profile}
                    <tr>
                      <td>{profile.profile_name}</td>
                      <td>{profile.rule_name}</td>
                      <td>{profile.path_count}</td>
                      <td>{Math.round((profile.confidence || 0) * 100)}%</td>
                    </tr>
                    <tr>
                      <td colspan="4" class="muted" style="font-size:12px;">{profile.rationale}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            {/if}

            {#if recommendations.recommended_paths?.length > 0}
              <h4 class="section-head" style="margin-top:16px;">Paths</h4>
              <table>
                <thead><tr><th>Use</th><th>Path</th><th>Profile</th><th>Mode</th><th>Rationale</th></tr></thead>
                <tbody>
                  {#each recommendations.recommended_paths as path}
                    <tr>
                      <td>
                        <input
                          type="checkbox"
                          checked={recommendationChecked(path)}
                          onchange={(event) => toggleRecommendation(path, event.currentTarget.checked)}
                        />
                      </td>
                      <td><code style="font-size:11px; overflow-wrap:anywhere;">{path.path}</code></td>
                      <td>{path.profile_name}</td>
                      <td>{path.mode}</td>
                      <td class="muted" style="font-size:12px;">{path.rationale}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            {:else}
              <div class="empty">No recommended paths available yet.</div>
            {/if}

            <h4 class="section-head" style="margin-top:16px;">Approved Set</h4>
            {#if recommendationDraft.length === 0}
              <div class="empty">No paths selected yet.</div>
            {:else}
              <table>
                <thead><tr><th>Path</th><th>Mode</th><th>Interval</th><th></th></tr></thead>
                <tbody>
                  {#each recommendationDraft as path}
                    <tr>
                      <td><code style="font-size:11px; overflow-wrap:anywhere;">{path.path}</code></td>
                      <td>{path.mode}</td>
                      <td>{path.sample_interval_ns > 0 ? Math.round(path.sample_interval_ns / 1000000000) + 's' : '—'}</td>
                      <td><button class="ghost" onclick={() => removeDraftPath(path)}>Remove</button></td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            {/if}

            <div class="section" style="margin-top:16px;">
              <h4 class="section-head">Add Custom Path</h4>
              <div class="audit-grid">
                <span class="muted">Path</span>
                <input bind:value={customPath} placeholder="network-instances/network-instance/..." />
                <span class="muted">Mode</span>
                <select bind:value={customMode}>
                  <option value="SAMPLE">SAMPLE</option>
                  <option value="ON_CHANGE">ON_CHANGE</option>
                </select>
                <span class="muted">Sample seconds</span>
                <input bind:value={customSampleSeconds} type="number" min="0" step="1" />
              </div>
              <button class="ghost" style="margin-top:10px;" onclick={addCustomDraftPath}>Add path</button>
            </div>

            <div class="section" style="display:flex; gap:10px; align-items:center; margin-top:16px;">
              <button onclick={saveRecommendationDraft} disabled={recommendationSaveBusy}>
                {recommendationSaveBusy ? 'Saving…' : 'Apply selected set'}
              </button>
              {#if recommendationSaveMessage}
                <span class="muted">{recommendationSaveMessage}</span>
              {/if}
            </div>

            {#if recommendations.blockers?.length > 0}
              <h4 class="section-head" style="margin-top:16px;">Blockers</h4>
              <ul class="plain-list">
                {#each recommendations.blockers as blocker}
                  <li>{blocker}</li>
                {/each}
              </ul>
            {/if}

            {#if recommendations.gaps?.length > 0}
              <h4 class="section-head" style="margin-top:16px;">Gaps</h4>
              <ul class="plain-list">
                {#each recommendations.gaps as gap}
                  <li>{gap}</li>
                {/each}
              </ul>
            {/if}

            {#if recommendations.warnings?.length > 0}
              <h4 class="section-head" style="margin-top:16px;">Notes</h4>
              <ul class="plain-list">
                {#each recommendations.warnings as warning}
                  <li>{warning}</li>
                {/each}
              </ul>
            {/if}

            {#if recommendations.override_audit?.length > 0}
              <h4 class="section-head" style="margin-top:16px;">Override Audit</h4>
              <ul class="plain-list">
                {#each recommendations.override_audit as line}
                  <li>{line}</li>
                {/each}
              </ul>
            {/if}
          </div>
        {/if}

      {:else if activeTab === 'events'}
        {#if device.recent_state_changes.length === 0}
          <div class="empty">No state changes recorded yet.</div>
        {:else}
          <div class="event-list">
            {#each device.recent_state_changes as ev}
              <div class="event-row">
                <span class="ts" title={absoluteTime(ev.occurred_at_ns)}>{relativeTime(ev.occurred_at_ns)}</span>
                <div class="body">
                  <span class="evt-type">{ev.event_type.replace(/_/g, ' ')}</span>
                  {#if ev.detail}
                    <span class="muted" style="font-size:12px; display:block; margin-top:2px;">{ev.detail}</span>
                  {/if}
                </div>
              </div>
            {/each}
          </div>
        {/if}

      {:else if activeTab === 'detections'}
        {#if device.recent_detections.length === 0}
          <div class="empty">No detections for this device.</div>
        {:else}
          <div class="event-list">
            {#each device.recent_detections as det}
              <button class="det-btn" onclick={() => det.id && navigate('/trace/' + encodeURIComponent(det.id))}>
                <span class="badge {det.severity === 'critical' ? 'critical' : det.severity === 'high' ? 'warn' : 'info'}">{det.severity}</span>
                <span class="det-rule">{det.rule_id || 'detection'}</span>
                <span class="muted det-ts" title={absoluteTime(det.fired_at_ns)}>{relativeTime(det.fired_at_ns)}</span>
                {#if det.remediation_status}
                  <span class="badge {det.remediation_status === 'succeeded' ? 'healthy' : 'warn'}">{det.remediation_status}</span>
                {/if}
              </button>
            {/each}
          </div>
        {/if}

      {:else if activeTab === 'readiness'}
        {#if readinessLoading}
          <div class="drawer-loading">
            {#each [1, 2] as _}<div class="skeleton-line"></div>{/each}
          </div>
        {:else if !readiness}
          <div class="empty">No readiness data yet.</div>
        {:else}
          <div class="audit-grid">
            <span class="muted">Service</span>
            <span>{readiness.service_status}</span>
            <span class="muted">TLS</span>
            <span>{readiness.tls_status}</span>
            <span class="muted">Encodings</span>
            <span>{(readiness.encoding_support || []).join(', ') || '—'}</span>
          </div>
          {#if readiness.blockers?.length}
            <h4 class="section-head" style="margin-top:16px;">Blockers</h4>
            <ul class="plain-list">
              {#each readiness.blockers as blocker}
                <li>{blocker}</li>
              {/each}
            </ul>
          {/if}
          {#if readiness.recommended_actions?.length}
            <h4 class="section-head" style="margin-top:16px;">Recommended Actions</h4>
            <ul class="plain-list">
              {#each readiness.recommended_actions as action}
                <li>{action}</li>
              {/each}
            </ul>
          {/if}
          {#if readiness.known_issues?.length}
            <h4 class="section-head" style="margin-top:16px;">Known Issues</h4>
            <ul class="plain-list">
              {#each readiness.known_issues as issue}
                <li>{issue}</li>
              {/each}
            </ul>
          {/if}
        {/if}

      {:else if activeTab === 'config'}
        <div class="section" style="display:flex; gap:10px; align-items:center; margin-bottom:16px;">
          <button class="ghost" onclick={triggerReparse} disabled={reparseBusy}>
            {reparseBusy ? 'Queueing…' : 'Re-parse now'}
          </button>
          {#if reparseMessage}
            <span class="muted" style="font-size:12px;">{reparseMessage}</span>
          {/if}
        </div>

        {#if configLoading}
          <div class="drawer-loading">
            {#each [1, 2] as _}<div class="skeleton-line"></div>{/each}
          </div>
        {:else}
          <h4 class="section-head">Snapshots</h4>
          {#if configHistory.snapshots.length === 0}
            <div class="empty">No config snapshots recorded yet.</div>
          {:else}
            <table>
              <thead><tr><th>Trigger</th><th>Summary</th><th>Size</th><th>Captured</th></tr></thead>
              <tbody>
                {#each configHistory.snapshots as snapshot}
                  <tr>
                    <td><code style="font-size:12px;">{snapshot.trigger}</code></td>
                    <td>{snapshot.summary}<div class="muted" style="font-size:11px;">{snapshot.confidence || '—'} / {snapshot.parser || '—'}</div></td>
                    <td>{fmtBytes(snapshot.bytes_len)}</td>
                    <td title={absoluteTime(snapshot.captured_at_ns)}>{relativeTime(snapshot.captured_at_ns)}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}

          <h4 class="section-head" style="margin-top:16px;">Changes</h4>
          {#if configHistory.changes.length === 0}
            <div class="empty">No config changes recorded yet.</div>
          {:else}
            <table>
              <thead><tr><th>Trigger</th><th>Summary</th><th>Delta</th><th>Changed</th></tr></thead>
              <tbody>
                {#each configHistory.changes as change}
                  <tr>
                    <td><code style="font-size:12px;">{change.trigger}</code></td>
                    <td>{change.summary}<div class="muted" style="font-size:11px;">{change.confidence || '—'} / {change.parser || '—'}</div></td>
                    <td>+{change.added_lines} / -{change.removed_lines}</td>
                    <td title={absoluteTime(change.changed_at_ns)}>{relativeTime(change.changed_at_ns)}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        {/if}

      {:else if activeTab === 'enrichment'}
        {#if enrichmentLoading}
          <div class="drawer-loading">
            {#each [1, 2] as _}<div class="skeleton-line"></div>{/each}
          </div>
        {:else if enrichmentProps.length === 0}
          <div class="empty">No enrichment data for this device. Run an enricher from the Enrichment workspace.</div>
        {:else}
          {#each groupedEnrichment() as [source, props]}
            <h4 class="section-head">{source}</h4>
            <table>
              <thead><tr><th>Property</th><th>Value</th><th>Updated</th></tr></thead>
              <tbody>
                {#each props as p}
                  <tr>
                    <td><code style="font-size:12px;">{p.key}</code></td>
                    <td style="max-width:180px; overflow-wrap:anywhere;">{p.value}<div class="muted" style="font-size:11px;">{p.confidence || '—'} / {p.parser || '—'}</div></td>
                    <td title={absoluteTime(p.updated_at_ns)} style="white-space:nowrap;">{relativeTime(p.updated_at_ns)}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/each}
        {/if}

      {:else if activeTab === 'audit'}
        <div class="audit-grid">
          <span class="muted">Created</span>
          <span title={absoluteTime(device.created_at_ns)}>{device.created_at_ns ? relativeTime(device.created_at_ns) : '—'}</span>

          <span class="muted">Created by</span>
          <span>{device.created_by || 'unknown'}</span>

          <span class="muted">Last updated</span>
          <span title={absoluteTime(device.updated_at_ns)}>{device.updated_at_ns ? relativeTime(device.updated_at_ns) : '—'}</span>

          <span class="muted">Updated by</span>
          <span>{device.updated_by || 'unknown'}</span>

          <span class="muted">Last action</span>
          <code>{device.last_operator_action || 'unknown'}</code>
        </div>
      {/if}
    </div>
  {/if}
</aside>

<style>
  .drawer-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0,0,0,0.4);
    z-index: 100;
    border: 0;
    padding: 0;
    width: 100%;
    cursor: pointer;
  }

  .drawer {
    position: fixed;
    top: 0;
    right: 0;
    width: 520px;
    max-width: 90vw;
    height: 100vh;
    background: var(--bg);
    border-left: 1px solid var(--border);
    z-index: 101;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .drawer-header {
    padding: 16px;
    border-bottom: 1px solid var(--border);
    position: relative;
    padding-right: 44px;
  }

  .drawer-title {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px;
    font-size: 15px;
  }

  .drawer-title-skeleton {
    height: 22px;
    width: 200px;
    background: var(--bg2);
    border-radius: 4px;
    animation: pulse 1.5s infinite;
  }

  .audit-grid {
    display: grid;
    grid-template-columns: 110px 1fr;
    gap: 10px 12px;
    font-size: 13px;
  }

  .drawer-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 8px;
  }

  .meta-chip {
    font-size: 11px;
    padding: 2px 7px;
    background: rgba(255,255,255,0.05);
    border: 1px solid var(--border);
    border-radius: 99px;
    color: var(--muted);
  }

  .drawer-close {
    position: absolute;
    top: 14px;
    right: 14px;
    padding: 4px 8px;
    font-size: 14px;
  }

  .drawer-loading {
    padding: 16px;
    display: grid;
    gap: 8px;
  }

  .skeleton-line {
    height: 16px;
    background: var(--bg2);
    border-radius: 4px;
    animation: pulse 1.5s infinite;
  }
  .skeleton-line:nth-child(2) { width: 80%; }
  .skeleton-line:nth-child(3) { width: 60%; }

  @keyframes pulse { 0%, 100% { opacity: 0.6; } 50% { opacity: 0.3; } }

  .drawer-tabs {
    display: flex;
    gap: 2px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
    background: var(--bg2);
    overflow-x: auto;
  }

  .drawer-tabs button {
    background: transparent;
    border: none;
    color: var(--muted);
    padding: 5px 10px;
    border-radius: 4px;
    font-size: 12px;
    text-transform: capitalize;
    cursor: pointer;
  }

  .drawer-tabs button.active {
    background: rgba(88,166,255,0.15);
    color: var(--text);
  }

  .drawer-body {
    flex: 1;
    overflow-y: auto;
    padding: 12px 0;
  }

  .drawer-body table { margin: 0 12px; width: calc(100% - 24px); }

  .section-head {
    font-size: 11px;
    font-weight: 700;
    text-transform: uppercase;
    color: var(--muted);
    letter-spacing: 0.1em;
    padding: 0 12px;
    margin-bottom: 6px;
  }

  .event-list { padding: 0 12px; }

  .event-row {
    display: flex;
    gap: 10px;
    padding: 7px 0;
    border-bottom: 1px solid var(--border);
  }

  .ts { color: var(--muted); font-size: 11px; min-width: 60px; }
  .evt-type { text-transform: capitalize; font-size: 13px; }

  .det-btn {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 7px 0;
    border: none;
    border-bottom: 1px solid var(--border);
    background: transparent;
    color: var(--text);
    cursor: pointer;
    text-align: left;
    font-size: 13px;
    font-family: inherit;
  }
  .det-btn:hover { background: rgba(255,255,255,0.03); }

  .det-rule { flex: 1; }
  .det-ts { font-size: 11px; }
  .text-warn { color: var(--yellow); }
  .empty { padding: 24px 16px; color: var(--muted); text-align: center; }
</style>
