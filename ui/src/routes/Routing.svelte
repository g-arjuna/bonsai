<script>
  import { onMount } from 'svelte';

  let health = $state(null);
  let speakers = $state([]);
  let peers = $state([]);
  let routes = $state([]);
  let coverage = $state([]);
  let prefixHistory = $state([]);
  let routesMeta = $state({
    completeness_state: 'syncing',
    support_status: 'supported',
    durable_sequence: 0,
    receive_time: '',
    total_matching: 0,
    next_cursor: null,
    message: '',
  });

  let loading = $state(true);
  let error = $state(null);

  // Filters for /api/routing/v1/routes
  let selectedSpeaker = $state('');
  let selectedPeer = $state('');
  let selectedView = $state('');
  let selectedVrfRd = $state('');
  let selectedFamily = $state('ipv4-unicast');
  let prefixFilter = $state('');
  let cursor = $state(0);

  // Selected prefix for /api/routing/v1/prefix-history
  let historyPrefix = $state('');
  let loadingHistory = $state(false);

  // A09 / A10: Grounded Missing-Prefix Investigator & Structured Operator Adjudication
  let investigatePrefix = $state('203.0.113.0/24');
  let investigateVrfRd = $state('0:0');
  let investigateSpeaker = $state('');
  let investigateQuestion = $state('Why is prefix 203.0.113.0/24 missing or degraded?');
  let loadingInvestigation = $state(false);
  let explanation = $state(null);

  // Structured Operator Adjudication form state (Strategy §8, A10)
  let adjVerdict = $state('unknown');
  let adjCheckUtility = $state('useful');
  let adjRecommendationAccepted = $state(false);
  let adjActionOutcome = $state('not_executed');
  let adjOperator = $state('netops-oncall');
  let adjRationale = $state('');
  let adjSelectedEvidenceIds = $state([]);
  let adjRejectedHypId = $state('');
  let adjRejectedHypReason = $state('');
  let adjCorrectionField = $state('');
  let adjCorrectionValue = $state('');
  let adjStatusMsg = $state('');
  let adjErrorMsg = $state('');
  let adjudicationSummary = $state(null);
  let adjudicationHistory = $state([]);

  const RIB_VIEWS = [
    { value: '', label: 'All Views' },
    { value: 'AdjRibInPrePolicy', label: 'Adj-RIB-In (Pre-Policy)' },
    { value: 'AdjRibInPostPolicy', label: 'Adj-RIB-In (Post-Policy)' },
    { value: 'LocRib', label: 'Loc-RIB' },
    { value: 'LocRibFiltered', label: 'Loc-RIB (Filtered)' },
    { value: 'AdjRibOutPrePolicy', label: 'Adj-RIB-Out (Pre-Policy)' },
    { value: 'AdjRibOutPostPolicy', label: 'Adj-RIB-Out (Post-Policy)' },
  ];

  const FAMILIES = [
    { value: 'ipv4-unicast', label: 'IPv4 Unicast (1/1)' },
    { value: 'ipv6-unicast', label: 'IPv6 Unicast (2/1)' },
    { value: 'ipv4-vpn', label: 'VPNv4 L3VPN (1/128)' },
    { value: 'ipv6-vpn', label: 'VPNv6 L3VPN (2/128)' },
    { value: 'l2vpn-evpn', label: 'L2VPN EVPN (25/70)' },
    { value: 'bgp-ls', label: 'BGP-LS (16388/71)' },
  ];

  onMount(() => {
    refreshAll();
    const interval = setInterval(refreshAll, 15_000);
    return () => clearInterval(interval);
  });

  async function refreshAll() {
    loading = true;
    error = null;
    try {
      const [hRes, sRes, pRes, cRes] = await Promise.all([
        fetch('/api/routing/v1/health'),
        fetch('/api/routing/v1/speakers'),
        fetch('/api/routing/v1/peers'),
        fetch('/api/routing/v1/coverage'),
      ]);

      if (!hRes.ok) {
        const body = await hRes.json().catch(() => ({}));
        throw new Error(body.message || `Routing service returned HTTP ${hRes.status}`);
      }

      health = await hRes.json();
      const sData = await sRes.json();
      speakers = sData.speakers || [];
      const pData = await pRes.json();
      peers = pData.peers || [];
      const cData = await cRes.json();
      coverage = cData.families || [];

      await loadRoutes();
      await loadAdjudications();
      if (historyPrefix.trim()) {
        await loadPrefixHistory(historyPrefix.trim());
      }
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function runPrefixInvestigation(pfxOverride) {
    if (pfxOverride) {
      investigatePrefix = pfxOverride;
    }
    if (!investigatePrefix.trim()) return;
    loadingInvestigation = true;
    adjStatusMsg = '';
    adjErrorMsg = '';
    try {
      const params = new URLSearchParams({
        prefix: investigatePrefix.trim(),
        vrf_rd: investigateVrfRd.trim() || '0:0',
        question: investigateQuestion.trim() || `Explain routing state for ${investigatePrefix.trim()}`,
      });
      if (investigateSpeaker.trim()) {
        params.set('speaker', investigateSpeaker.trim());
      }
      const r = await fetch(`/api/routing/v1/explain-prefix?${params.toString()}`);
      const data = await r.json();
      explanation = data;
      adjSelectedEvidenceIds = [...(data.supporting_evidence_ids || [])];
    } catch (e) {
      adjErrorMsg = e.message;
    } finally {
      loadingInvestigation = false;
    }
  }

  async function loadAdjudications() {
    try {
      const r = await fetch('/api/routing/v1/adjudications');
      if (r.ok) {
        const data = await r.json();
        adjudicationSummary = data.adjudicated_accuracy || null;
        adjudicationHistory = data.adjudications || [];
      }
    } catch (_) {
      // ignore background adjudication fetch errors
    }
  }

  function toggleEvidenceId(obsId) {
    if (adjSelectedEvidenceIds.includes(obsId)) {
      adjSelectedEvidenceIds = adjSelectedEvidenceIds.filter((id) => id !== obsId);
    } else {
      adjSelectedEvidenceIds = [...adjSelectedEvidenceIds, obsId];
    }
  }

  async function submitAdjudication() {
    adjStatusMsg = '';
    adjErrorMsg = '';
    const rejected_hypotheses =
      adjRejectedHypId.trim() && adjRejectedHypReason.trim()
        ? [{ hypothesis_id: adjRejectedHypId.trim(), reason: adjRejectedHypReason.trim() }]
        : [];
    const entity_corrections =
      adjCorrectionField.trim() && adjCorrectionValue.trim()
        ? [
            {
              field: adjCorrectionField.trim(),
              original_value: explanation?.root_cause_type || 'unknown',
              corrected_value: adjCorrectionValue.trim(),
            },
          ]
        : [];

    const payload = {
      prefix: investigatePrefix.trim(),
      vrf_rd: investigateVrfRd.trim() || '0:0',
      speaker: investigateSpeaker.trim() || null,
      diagnosis_verdict: adjVerdict,
      next_check_utility: adjCheckUtility,
      recommendation_accepted: adjRecommendationAccepted,
      action_outcome: adjActionOutcome,
      adjudicated_by: adjOperator.trim(),
      adjudication_evidence_ids: adjSelectedEvidenceIds,
      rejected_hypotheses,
      entity_corrections,
      missing_evidence_noted: explanation?.missing_observations || [],
      rationale: adjRationale.trim(),
    };

    try {
      const r = await fetch('/api/routing/v1/adjudicate-prefix', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
      });
      const data = await r.json();
      if (!r.ok || !data.accepted) {
        adjErrorMsg = data.message || 'Adjudication rejected by validation contract.';
        return;
      }
      adjudicationSummary = data.adjudicated_accuracy;
      adjStatusMsg = `Recorded adjudication ${data.adjudication.adjudication_id} (root_cause_verified=${data.adjudication.root_cause_verified}, eligible_for_training=${data.adjudication.eligible_for_training})`;
      await loadAdjudications();
    } catch (e) {
      adjErrorMsg = e.message;
    }
  }

  async function loadRoutes() {
    try {
      const params = new URLSearchParams();
      if (selectedSpeaker) params.set('speaker', selectedSpeaker);
      if (selectedPeer) params.set('peer', selectedPeer);
      if (selectedView) params.set('view', selectedView);
      if (selectedVrfRd.trim()) params.set('vrf_rd', selectedVrfRd.trim());
      if (selectedFamily) params.set('family', selectedFamily);
      if (prefixFilter.trim()) params.set('prefix', prefixFilter.trim());
      params.set('cursor', String(cursor));
      params.set('limit', '100');

      const r = await fetch(`/api/routing/v1/routes?${params.toString()}`);
      const data = await r.json();
      routes = data.routes || [];
      routesMeta = {
        completeness_state: data.completeness_state || 'syncing',
        support_status: data.support_status || 'supported',
        durable_sequence: data.durable_sequence ?? 0,
        receive_time: data.receive_time || '',
        total_matching: data.total_matching ?? routes.length,
        next_cursor: data.next_cursor ?? null,
        message: data.message || '',
      };
    } catch (e) {
      error = e.message;
    }
  }

  async function loadPrefixHistory(pfx) {
    if (!pfx) return;
    historyPrefix = pfx;
    loadingHistory = true;
    try {
      const params = new URLSearchParams({ prefix: pfx, limit: '50' });
      const r = await fetch(`/api/routing/v1/prefix-history?${params.toString()}`);
      if (r.ok) {
        const data = await r.json();
        prefixHistory = data.events || [];
      }
    } catch (_) {
      prefixHistory = [];
    } finally {
      loadingHistory = false;
    }
  }

  function applyFilters() {
    cursor = 0;
    loadRoutes();
  }

  function completenessBadgeClass(state) {
    switch (state) {
      case 'complete_for_observed_view':
      case 'supported':
        return 'badge-ok';
      case 'syncing':
      case 'hypothesis_only':
        return 'badge-info';
      case 'stale':
      case 'stale_observation':
      case 'insufficient_evidence':
        return 'badge-warn';
      case 'gap':
      case 'unsupported':
      case 'unavailable':
      case 'conflicted':
        return 'badge-danger';
      default:
        return 'badge-neutral';
    }
  }
</script>

<div class="routing-workspace">
  <header class="workspace-header">
    <div>
      <p class="eyebrow">Canonical BMP / BGP Control Plane</p>
      <h2>Routing &amp; RIB Observatory</h2>
    </div>
    <div class="header-badges">
      {#if health}
        <span class="badge {completenessBadgeClass(health.completeness_state)}" title="Overall RIB Completeness State">
          Completeness: {health.completeness_state}
        </span>
        <span class="badge badge-neutral" title="Durable RibEvent Journal Sequence">
          Seq #{health.durable_sequence ?? 0}
        </span>
        {#if health.graph_projection_lag !== undefined}
          <span class="badge {health.graph_projection_lag > 0 ? 'badge-warn' : 'badge-ok'}" title="Graph Summary Projection Lag">
            Graph Lag: {health.graph_projection_lag}
          </span>
        {/if}
        <span class="badge badge-neutral" title="Last Routing Health Snapshot Time">
          Observed: {health.receive_time ? new Date(health.receive_time).toLocaleTimeString() : '—'}
        </span>
      {/if}
      <button class="btn-refresh" onclick={refreshAll}>Refresh</button>
    </div>
  </header>

  {#if error}
    <div class="alert alert-danger" role="alert">
      <strong>Routing Service Unavailable or Degraded:</strong> {error}
    </div>
  {/if}

  {#if routesMeta.completeness_state !== 'complete_for_observed_view'}
    <div class="alert alert-warn" role="status">
      <strong>Data-Quality Notice ({routesMeta.completeness_state}):</strong>
      {#if routesMeta.completeness_state === 'unsupported'}
        {routesMeta.message || `Address family '${selectedFamily}' has support status '${routesMeta.support_status}' and is not projected as a complete RIB view.`}
      {:else if routesMeta.completeness_state === 'gap'}
        One or more BMP speaker sessions experienced a transport disconnect without explicit BGP route withdrawals. Last-known routes are preserved with a transport-gap indicator.
      {:else if routesMeta.completeness_state === 'stale'}
        Long-Lived Graceful Restart (LLGR) stale routes are present while a session is recovering.
      {:else}
        Initial table dump or End-of-RIB marker has not completed for all observed views (`syncing`). An empty or partial table does not imply absence of routes on the router.
      {/if}
    </div>
  {/if}

  <!-- Summary KPI Row -->
  <section class="kpi-grid" aria-label="Routing Summary">
    <div class="kpi-card">
      <span class="kpi-label">BMP Speakers</span>
      <strong class="kpi-value">{health?.speakers_connected ?? speakers.length}</strong>
    </div>
    <div class="kpi-card">
      <span class="kpi-label">BGP Peers Up</span>
      <strong class="kpi-value">{health?.peers_up ?? 0}</strong>
    </div>
    <div class="kpi-card">
      <span class="kpi-label">Active RIB Routes</span>
      <strong class="kpi-value">{health?.total_routes ?? 0}</strong>
    </div>
    <div class="kpi-card">
      <span class="kpi-label">Transport Gaps</span>
      <strong class="kpi-value">{health?.transport_gaps ?? 0}</strong>
    </div>
  </section>

  <!-- Speakers & Peer Sessions -->
  <section class="panel-grid">
    <div class="panel">
      <h3>BMP Exporters (Speakers)</h3>
      {#if speakers.length === 0}
        <p class="empty-hint">No BMP speakers connected yet.</p>
      {:else}
        <table class="data-table">
          <thead>
            <tr>
              <th>Speaker IP</th>
              <th>SysName</th>
              <th>Epoch</th>
              <th>Peers (Up/Total)</th>
              <th>Routes</th>
              <th>Completeness</th>
            </tr>
          </thead>
          <tbody>
            {#each speakers as spk}
              <tr>
                <td><code>{spk.speaker_addr}</code></td>
                <td>{spk.sys_name || '—'}</td>
                <td>{spk.source?.session_epoch ?? 0}</td>
                <td>{spk.up_peer_count}/{spk.peer_count}</td>
                <td>{spk.total_routes}</td>
                <td>
                  <span class="badge {completenessBadgeClass(spk.completeness_state)}">
                    {spk.completeness_state}
                  </span>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>

    <div class="panel">
      <h3>BGP Peer / Instance Sessions</h3>
      {#if peers.length === 0}
        <p class="empty-hint">No BGP peer sessions observed.</p>
      {:else}
        <table class="data-table">
          <thead>
            <tr>
              <th>Speaker</th>
              <th>Peer IP</th>
              <th>ASN</th>
              <th>VRF / RD</th>
              <th>State</th>
              <th>EoR Families</th>
              <th>Completeness</th>
            </tr>
          </thead>
          <tbody>
            {#each peers as peer}
              <tr>
                <td><code>{peer.speaker_addr}</code></td>
                <td><code>{peer.peer_address}</code></td>
                <td>AS{peer.peer_as}</td>
                <td><code>{peer.peer_distinguisher}</code></td>
                <td>
                  <span class="badge {peer.state === 'up' ? 'badge-ok' : 'badge-danger'}">
                    {peer.state}
                  </span>
                </td>
                <td>{(peer.eor_families || []).join(', ') || 'None'}</td>
                <td>
                  <span class="badge {completenessBadgeClass(peer.completeness_state)}">
                    {peer.completeness_state}
                  </span>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>
  </section>

  <!-- RIB Explorer & Filters -->
  <section class="panel">
    <div class="panel-header">
      <h3>Canonical RIB Explorer</h3>
      <div class="filter-bar">
        <select bind:value={selectedFamily} onchange={applyFilters} aria-label="Address Family">
          {#each FAMILIES as fam}
            <option value={fam.value}>{fam.label}</option>
          {/each}
        </select>

        <select bind:value={selectedView} onchange={applyFilters} aria-label="RIB View">
          {#each RIB_VIEWS as v}
            <option value={v.value}>{v.label}</option>
          {/each}
        </select>

        <input
          type="text"
          placeholder="Speaker IP"
          bind:value={selectedSpeaker}
          aria-label="Speaker IP Filter"
        />
        <input
          type="text"
          placeholder="Peer IP"
          bind:value={selectedPeer}
          aria-label="Peer IP Filter"
        />
        <input
          type="text"
          placeholder="VRF RD (e.g. 0:0 or 65000:100)"
          bind:value={selectedVrfRd}
          aria-label="VRF RD Filter"
        />
        <input
          type="text"
          placeholder="Prefix (e.g. 10.10.0.0/24)"
          bind:value={prefixFilter}
          aria-label="Prefix Filter"
        />
        <button class="btn-primary" onclick={applyFilters}>Filter Routes</button>
      </div>
    </div>

    {#if routesMeta.support_status !== 'supported'}
      <div class="empty-hint">
        Address family <code>{selectedFamily}</code> has status
        <span class="badge badge-danger">{routesMeta.support_status}</span>:
        {routesMeta.message}
      </div>
    {:else if routes.length === 0}
      <p class="empty-hint">
        No routes match current filter (Completeness: <code>{routesMeta.completeness_state}</code>).
      </p>
    {:else}
      <table class="data-table">
        <thead>
          <tr>
            <th>Prefix</th>
            <th>View</th>
            <th>VRF / RD</th>
            <th>Speaker</th>
            <th>Peer (ASN)</th>
            <th>Next Hop</th>
            <th>AS Path</th>
            <th>LP / MED</th>
            <th>Communities</th>
            <th>State / Freshness</th>
            <th>History</th>
          </tr>
        </thead>
        <tbody>
          {#each routes as rt}
            <tr>
              <td><code>{rt.prefix}</code></td>
              <td><code>{rt.view}</code></td>
              <td><code>{rt.peer_distinguisher}</code></td>
              <td><code>{rt.speaker_addr}</code></td>
              <td><code>{rt.peer_addr}</code> (AS{rt.peer_as})</td>
              <td><code>{rt.next_hop || '—'}</code></td>
              <td><code>{rt.as_path || 'i'}</code></td>
              <td>{rt.local_pref ?? '—'} / {rt.med ?? '—'}</td>
              <td><code>{(rt.communities || []).join(' ') || '—'}</code></td>
              <td>
                <span class="badge {completenessBadgeClass(rt.completeness_state)}">
                  {rt.completeness_state}
                </span>
              </td>
              <td>
                <button class="btn-small" onclick={() => loadPrefixHistory(rt.prefix)}>
                  Timeline
                </button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </section>

  <!-- Prefix History Timeline (Before / After Path Attributes) -->
  <section class="panel">
    <div class="panel-header">
      <h3>Prefix Event History Timeline</h3>
      <div class="filter-bar">
        <input
          type="text"
          placeholder="Inspect prefix history (e.g. 10.10.0.0/24)"
          bind:value={historyPrefix}
          aria-label="Prefix History Query"
        />
        <button class="btn-primary" onclick={() => loadPrefixHistory(historyPrefix.trim())}>
          Load Timeline
        </button>
      </div>
    </div>

    {#if loadingHistory}
      <p class="empty-hint">Loading prefix history...</p>
    {:else if prefixHistory.length === 0}
      <p class="empty-hint">Select a route or enter a prefix above to inspect announce/withdraw transitions and before/after attributes.</p>
    {:else}
      <table class="data-table">
        <thead>
          <tr>
            <th>Seq #</th>
            <th>Observed At</th>
            <th>Action</th>
            <th>View</th>
            <th>Speaker / Peer</th>
            <th>Previous Path Attributes</th>
            <th>Current Path Attributes</th>
          </tr>
        </thead>
        <tbody>
          {#each prefixHistory as ev}
            <tr>
              <td><code>#{ev.sequence}</code></td>
              <td>{ev.occurred_at ? new Date(ev.occurred_at).toLocaleString() : '—'}</td>
              <td>
                <span class="badge {ev.action === 'Withdraw' ? 'badge-danger' : 'badge-ok'}">
                  {ev.action}
                </span>
              </td>
              <td><code>{ev.view?.rib_type || '—'}</code></td>
              <td><code>{ev.speaker}</code> / <code>{ev.peer}</code> (AS{ev.peer_as})</td>
              <td>
                {#if ev.previous_attributes}
                  <code>
                    NH: {ev.previous_attributes.next_hop || '—'} |
                    AS: {ev.previous_attributes.as_path || '—'} |
                    LP: {ev.previous_attributes.local_pref ?? '—'}
                  </code>
                {:else}
                  <span class="muted">None (initial)</span>
                {/if}
              </td>
              <td>
                {#if ev.attributes}
                  <code>
                    NH: {ev.attributes.next_hop || '—'} |
                    AS: {ev.attributes.as_path || '—'} |
                    LP: {ev.attributes.local_pref ?? '—'}
                  </code>
                {:else}
                  <span class="muted">Withdrawn</span>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </section>

  <!-- A09 / A10: Grounded Missing-Prefix Investigator & Operator Adjudication -->
  <section class="panel" aria-label="Missing-Prefix Investigator and Operator Adjudication">
    <div class="panel-header">
      <div>
        <h3>Missing-Prefix Investigator &amp; Operator Adjudication (A09 / A10)</h3>
        <p class="muted">
          Separates observed BMP/BGP facts from ranked hypotheses, citations (<code>observation_id</code>), missing planes, and structured operator adjudication.
        </p>
      </div>
      <div class="filter-bar">
        <input
          type="text"
          placeholder="Prefix (e.g. 203.0.113.0/24)"
          bind:value={investigatePrefix}
          aria-label="Investigate Prefix"
        />
        <input
          type="text"
          placeholder="VRF RD (0:0)"
          bind:value={investigateVrfRd}
          aria-label="Investigate VRF RD"
        />
        <input
          type="text"
          placeholder="Speaker IP (optional)"
          bind:value={investigateSpeaker}
          aria-label="Investigate Speaker IP"
        />
        <button class="btn-primary" onclick={() => runPrefixInvestigation()} disabled={loadingInvestigation}>
          {loadingInvestigation ? 'Investigating...' : 'Explain Prefix'}
        </button>
      </div>
    </div>

    {#if adjudicationSummary}
      <div class="coverage-row" style="margin-bottom: 0.75rem;">
        <div class="coverage-chip">
          <span>Adjudicated RCA Precision:</span>
          <strong>{adjudicationSummary.adjudicated_precision_pct}%</strong>
        </div>
        <div class="coverage-chip">
          <span>Verified Correct:</span>
          <strong>{adjudicationSummary.verified_correct_count}</strong>
        </div>
        <div class="coverage-chip">
          <span>Verified Incorrect:</span>
          <strong>{adjudicationSummary.verified_incorrect_count}</strong>
        </div>
        <div class="coverage-chip">
          <span>Accepted w/o RCA Proof:</span>
          <strong>{adjudicationSummary.recommendation_accepted_without_rca_proof_count}</strong>
        </div>
        <div class="coverage-chip">
          <span>Training-Eligible:</span>
          <strong>{adjudicationSummary.training_eligible_count}</strong>
        </div>
      </div>
    {/if}

    {#if explanation}
      <!-- 1. Impact & Epistemic Scope Summary -->
      <div class="alert {explanation.status === 'supported' ? 'alert-info' : 'alert-warn'}" style="margin-bottom: 0.75rem;">
        <div class="header-badges" style="margin-bottom: 0.35rem;">
          <span class="badge {completenessBadgeClass(explanation.epistemic_state)}">
            Epistemic State: {explanation.epistemic_state}
          </span>
          <span class="badge {completenessBadgeClass(explanation.completeness_state)}">
            RIB Completeness: {explanation.completeness_state}
          </span>
          <span class="badge badge-neutral">
            Root Cause Class: {explanation.root_cause_type}
          </span>
          {#if explanation.confidence_is_uncalibrated}
            <span class="badge badge-warn" title="Confidence scores are heuristic until conformal calibration on held-out faults">
              Uncalibrated Heuristic Score
            </span>
          {/if}
        </div>
        <div><strong>Conclusion:</strong> {explanation.conclusion}</div>
      </div>

      <!-- 2 & 3. Observed Facts vs Ranked Hypotheses & Counterevidence -->
      <div class="panel-grid" style="margin-bottom: 0.75rem;">
        <div class="subpanel">
          <h4>Observed Control-Plane Facts ({(explanation.observed_facts || []).length})</h4>
          {#if (explanation.observed_facts || []).length === 0}
            <p class="empty-hint">No direct BMP/RIB observations recorded for this prefix yet.</p>
          {:else}
            <ul class="evidence-list">
              {#each explanation.observed_facts as fact}
                <li>
                  <div>{fact.statement}</div>
                  <div class="citation-row">
                    <span class="badge badge-neutral">{fact.source_plane}</span>
                    {#if fact.completeness_state}
                      <span class="badge {completenessBadgeClass(fact.completeness_state)}">
                        {fact.completeness_state}
                      </span>
                    {/if}
                    {#if fact.observation_id}
                      <button
                        type="button"
                        class="btn-small"
                        onclick={() => toggleEvidenceId(fact.observation_id)}
                        title="Toggle citation in operator adjudication"
                      >
                        {adjSelectedEvidenceIds.includes(fact.observation_id) ? '✓ ' : ''}{fact.observation_id}
                      </button>
                    {/if}
                  </div>
                </li>
              {/each}
            </ul>
          {/if}

          <h4 style="margin-top: 0.75rem;">Explicitly Missing / Unobserved Planes</h4>
          <ul class="evidence-list">
            {#each explanation.missing_observations || [] as missing}
              <li class="muted">{missing}</li>
            {/each}
          </ul>
        </div>

        <div class="subpanel">
          <h4>Ranked Hypotheses &amp; Counterevidence ({(explanation.hypotheses || []).length})</h4>
          {#if (explanation.hypotheses || []).length === 0}
            <p class="empty-hint">No candidate hypotheses generated.</p>
          {:else}
            <ul class="evidence-list">
              {#each explanation.hypotheses as hyp}
                <li>
                  <div class="header-badges">
                    <strong>#{hyp.rank} [{hyp.hypothesis_id}]</strong>
                    <span class="badge badge-info">{hyp.failure_class}</span>
                    <span class="badge badge-warn">
                      Score: {hyp.heuristic_score} ({hyp.score_semantics || 'uncalibrated'})
                    </span>
                    <button
                      type="button"
                      class="btn-small"
                      onclick={() => {
                        adjRejectedHypId = hyp.hypothesis_id;
                      }}
                    >
                      Reject Hypothesis
                    </button>
                  </div>
                  <div style="margin-top: 0.25rem;">{hyp.statement}</div>
                  <div class="citation-row">
                    <span class="muted">
                      Supporting: {(hyp.supporting_evidence_ids || []).join(', ') || 'none'}
                    </span>
                    {#if (hyp.counterevidence_ids || []).length > 0}
                      <span class="badge badge-danger">
                        Counterevidence: {hyp.counterevidence_ids.join(', ')}
                      </span>
                    {/if}
                  </div>
                </li>
              {/each}
            </ul>
          {/if}

          {#if explanation.next_check}
            <div class="next-check-box">
              <h4>Recommended Next Discriminating Check</h4>
              <div>
                <code>{explanation.next_check.tool_or_command}</code>
                <span class="badge {explanation.next_check.read_only ? 'badge-ok' : 'badge-danger'}">
                  {explanation.next_check.read_only ? 'READ-ONLY' : 'MUTATING'}
                </span>
              </div>
              <p class="muted" style="margin: 0.25rem 0 0 0;">{explanation.next_check.reason}</p>
            </div>
          {/if}
        </div>
      </div>

      <!-- 5. Structured Operator Adjudication Form (Strategy §8, A10) -->
      <div class="subpanel">
        <h4>Structured Operator Adjudication (Separates Recommendation Acceptance from Verified RCA)</h4>
        <div class="filter-bar" style="margin-top: 0.5rem;">
          <label>
            Diagnosis Verdict:
            <select bind:value={adjVerdict} aria-label="Diagnosis Verdict">
              <option value="unknown">unknown (unresolved)</option>
              <option value="correct">correct (verified root cause)</option>
              <option value="incorrect">incorrect (refuted root cause)</option>
            </select>
          </label>

          <label>
            Next-Check Utility:
            <select bind:value={adjCheckUtility} aria-label="Next-Check Utility">
              <option value="useful">useful</option>
              <option value="useless">useless</option>
              <option value="not_evaluated">not_evaluated</option>
            </select>
          </label>

          <label>
            Action Outcome:
            <select bind:value={adjActionOutcome} aria-label="Action Outcome">
              <option value="not_executed">not_executed</option>
              <option value="resolved">resolved</option>
              <option value="not_resolved">not_resolved</option>
            </select>
          </label>

          <label class="checkbox-label">
            <input type="checkbox" bind:checked={adjRecommendationAccepted} />
            Recommendation Accepted (does NOT imply verified RCA)
          </label>
        </div>

        <div class="filter-bar" style="margin-top: 0.5rem;">
          <input
            type="text"
            placeholder="Operator ID (e.g. netops-oncall)"
            bind:value={adjOperator}
            aria-label="Operator Identity"
          />
          <input
            type="text"
            placeholder="Reject Hypothesis ID (e.g. H1)"
            bind:value={adjRejectedHypId}
            aria-label="Reject Hypothesis ID"
          />
          <input
            type="text"
            placeholder="Hypothesis rejection reason"
            bind:value={adjRejectedHypReason}
            aria-label="Hypothesis Rejection Reason"
          />
          <input
            type="text"
            placeholder="Corrected field (e.g. root_cause_type)"
            bind:value={adjCorrectionField}
            aria-label="Entity Correction Field"
          />
          <input
            type="text"
            placeholder="Corrected value"
            bind:value={adjCorrectionValue}
            aria-label="Entity Correction Value"
          />
        </div>

        <div class="filter-bar" style="margin-top: 0.5rem;">
          <input
            type="text"
            style="flex: 1; min-width: 280px;"
            placeholder="Operator adjudication rationale (required for training-eligible feedback)"
            bind:value={adjRationale}
            aria-label="Adjudication Rationale"
          />
          <button class="btn-primary" onclick={submitAdjudication}>
            Record Adjudication
          </button>
        </div>

        {#if adjStatusMsg}
          <p class="badge badge-ok" style="margin-top: 0.5rem;">{adjStatusMsg}</p>
        {/if}
        {#if adjErrorMsg}
          <p class="badge badge-danger" style="margin-top: 0.5rem;">{adjErrorMsg}</p>
        {/if}
      </div>
    {:else}
      <p class="empty-hint">
        Enter a prefix above and click <strong>Explain Prefix</strong> to inspect grounded control-plane facts, ranked hypotheses, citations, and operator adjudication controls.
      </p>
    {/if}
  </section>

  <!-- Address-Family Coverage Matrix -->
  <section class="panel">
    <h3>AFI / SAFI Protocol Coverage Matrix</h3>
    <div class="coverage-row">
      {#each coverage as fam}
        <div class="coverage-chip">
          <strong>{fam.family}</strong>
          <span>({fam.afi}/{fam.safi})</span>
          <span class="badge {fam.support_status === 'supported' ? 'badge-ok' : 'badge-warn'}">
            {fam.support_status}
          </span>
        </div>
      {/each}
    </div>
  </section>
</div>

<style>
  .routing-workspace {
    display: flex;
    flex-direction: column;
    gap: 1rem;
    padding: 1rem 1.25rem;
  }
  .workspace-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.75rem;
  }
  .header-badges {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .kpi-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
    gap: 0.75rem;
  }
  .kpi-card {
    background: var(--surface-2, #161b22);
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    padding: 0.75rem 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .kpi-label {
    font-size: 0.75rem;
    color: var(--text-muted, #8b949e);
    text-transform: uppercase;
  }
  .kpi-value {
    font-size: 1.4rem;
  }
  .panel-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(420px, 1fr));
    gap: 1rem;
  }
  .panel {
    background: var(--surface-2, #161b22);
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    padding: 1rem;
    overflow-x: auto;
  }
  .panel-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.75rem;
    margin-bottom: 0.75rem;
  }
  .filter-bar {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
    align-items: center;
  }
  .filter-bar input,
  .filter-bar select {
    background: var(--surface-1, #0d1117);
    color: var(--text, #e6edf3);
    border: 1px solid var(--border, #30363d);
    border-radius: 4px;
    padding: 0.35rem 0.6rem;
    font-size: 0.85rem;
  }
  .data-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.85rem;
  }
  .data-table th,
  .data-table td {
    text-align: left;
    padding: 0.45rem 0.6rem;
    border-bottom: 1px solid var(--border, #21262d);
  }
  .badge {
    display: inline-block;
    padding: 0.15rem 0.45rem;
    border-radius: 4px;
    font-size: 0.75rem;
    font-weight: 600;
  }
  .badge-ok {
    background: rgba(46, 160, 67, 0.2);
    color: #3fb950;
  }
  .badge-info {
    background: rgba(56, 139, 253, 0.2);
    color: #58a6ff;
  }
  .badge-warn {
    background: rgba(210, 153, 34, 0.2);
    color: #d29922;
  }
  .badge-danger {
    background: rgba(248, 81, 73, 0.2);
    color: #f85149;
  }
  .badge-neutral {
    background: rgba(110, 118, 129, 0.2);
    color: #8b949e;
  }
  .alert {
    padding: 0.65rem 0.9rem;
    border-radius: 6px;
    font-size: 0.85rem;
  }
  .alert-warn {
    background: rgba(210, 153, 34, 0.15);
    border: 1px solid rgba(210, 153, 34, 0.4);
  }
  .alert-danger {
    background: rgba(248, 81, 73, 0.15);
    border: 1px solid rgba(248, 81, 73, 0.4);
  }
  .coverage-row {
    display: flex;
    gap: 0.75rem;
    flex-wrap: wrap;
    margin-top: 0.5rem;
  }
  .coverage-chip {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.35rem 0.65rem;
    background: var(--surface-1, #0d1117);
    border: 1px solid var(--border, #30363d);
    border-radius: 4px;
    font-size: 0.8rem;
  }
  .btn-primary,
  .btn-refresh,
  .btn-small {
    cursor: pointer;
    border-radius: 4px;
    border: 1px solid var(--border, #30363d);
    background: var(--surface-3, #21262d);
    color: var(--text, #e6edf3);
    padding: 0.35rem 0.7rem;
    font-size: 0.8rem;
  }
  .alert-info {
    background: rgba(56, 139, 253, 0.12);
    border: 1px solid rgba(56, 139, 253, 0.35);
  }
  .subpanel {
    background: var(--surface-1, #0d1117);
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    padding: 0.75rem 0.9rem;
  }
  .subpanel h4 {
    margin: 0 0 0.5rem 0;
    font-size: 0.9rem;
  }
  .evidence-list {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    font-size: 0.85rem;
  }
  .evidence-list li {
    padding: 0.45rem 0.6rem;
    border-radius: 4px;
    background: var(--surface-2, #161b22);
    border: 1px solid var(--border, #21262d);
  }
  .citation-row {
    display: flex;
    gap: 0.4rem;
    flex-wrap: wrap;
    align-items: center;
    margin-top: 0.3rem;
  }
  .next-check-box {
    margin-top: 0.75rem;
    padding: 0.6rem 0.75rem;
    border-radius: 4px;
    border: 1px dashed var(--border, #30363d);
    background: var(--surface-2, #161b22);
  }
  .checkbox-label {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.82rem;
  }
  .empty-hint,
  .muted {
    color: var(--text-muted, #8b949e);
    font-size: 0.85rem;
  }
</style>

