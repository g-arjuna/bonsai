<script>
  import { onMount, onDestroy } from 'svelte';

  let data = $state(null);
  let loading = $state(true);
  let error = $state('');
  let profileSwitching = $state(false);
  let profileMsg = $state('');
  let interval;

  onMount(() => {
    loadData();
    interval = setInterval(loadData, 5000);
  });

  onDestroy(() => { if (interval) clearInterval(interval); });

  async function loadData() {
    try {
      const r = await fetch('/api/governance/history');
      if (!r.ok) throw new Error(await r.text());
      data = await r.json();
      error = '';
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function switchProfile(p) {
    profileSwitching = true;
    profileMsg = '';
    try {
      const r = await fetch('/api/governance/profile', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ profile: p }),
      });
      const res = await r.json();
      if (!r.ok) throw new Error(res.error || 'Failed');
      profileMsg = res.note || 'Profile updated';
    } catch (e) {
      profileMsg = e.message;
    } finally {
      profileSwitching = false;
    }
  }

  function pct(rss, budget) {
    if (!budget) return 0;
    return Math.min(100, Math.round((rss / budget) * 100));
  }

  function barColor(p) {
    if (p >= 95) return '#ef4444';
    if (p >= 80) return '#f59e0b';
    return '#22c55e';
  }

  const profiles = ['tiny', 'small', 'medium', 'large', 'xlarge'];
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">System</p>
      <h2>Resource Governance</h2>
    </div>
    <button class="primary" onclick={loadData}>Refresh</button>
  </div>

  {#if loading}
    <p class="muted">Loading governance state...</p>
  {:else if error}
    <p class="error">{error}</p>
  {:else if data?.status === 'governance_not_started'}
    <p class="muted">Resource governor is not running.</p>
  {:else if data}
    {@const rssPct = pct(data.current_rss_mb, data.memory_budget_mb)}

    <div class="status-grid">
      <div class="status-card">
        <span class="card-label">Profile</span>
        <span class="card-value profile">{data.profile}</span>
      </div>
      <div class="status-card">
        <span class="card-label">RSS</span>
        <span class="card-value">{data.current_rss_mb} MB</span>
        <div class="bar-track">
          <div class="bar-fill" style="width:{rssPct}%;background:{barColor(rssPct)}"></div>
        </div>
        <span class="bar-label">{rssPct}% of {data.memory_budget_mb} MB budget</span>
      </div>
      <div class="status-card">
        <span class="card-label">Rate Budget</span>
        <span class="card-value">{data.rate_budget_eps?.toLocaleString()} eps</span>
      </div>
    </div>

    <div class="flags-row">
      <span class="flag" class:active={data.memory_pressure_active} class:green={!data.memory_pressure_active}>
        Memory Pressure: {data.memory_pressure_active ? 'ACTIVE' : 'clear'}
      </span>
      <span class="flag" class:active={data.write_pressure_active} class:green={!data.write_pressure_active}>
        Write Pressure: {data.write_pressure_active ? 'ACTIVE' : 'clear'}
      </span>
      <span class="flag" class:active={data.rate_shedding_active} class:green={!data.rate_shedding_active}>
        Rate Shedding: {data.rate_shedding_active ? 'ACTIVE' : 'clear'}
      </span>
    </div>

    <h3>Governance Counters</h3>
    <table class="data-table">
      <thead><tr><th>Counter</th><th>Value</th></tr></thead>
      <tbody>
        <tr><td>Memory shrink actions</td><td class="num">{data.counters?.memory_shrink ?? 0}</td></tr>
        <tr><td>Memory flush actions</td><td class="num">{data.counters?.memory_flush ?? 0}</td></tr>
        <tr><td>Write batch expansions</td><td class="num">{data.counters?.write_batch_expand ?? 0}</td></tr>
        <tr><td>Rate shed actions</td><td class="num">{data.counters?.rate_shed ?? 0}</td></tr>
      </tbody>
    </table>

    <h3 style="margin-top:24px">Profile Switcher</h3>
    <p class="muted" style="margin-bottom:12px">Select a resource profile. Changes take effect after restart.</p>
    <div class="profile-btns">
      {#each profiles as p}
        <button
          class:selected={data.profile === p}
          onclick={() => switchProfile(p)}
          disabled={profileSwitching}
        >{p}</button>
      {/each}
    </div>
    {#if profileMsg}
      <p class="profile-msg">{profileMsg}</p>
    {/if}
  {/if}
</div>

<style>
  .status-grid { display: flex; gap: 16px; margin-bottom: 20px; flex-wrap: wrap; }
  .status-card { background: var(--surface); border: 1px solid var(--border); border-radius: 8px; padding: 16px 24px; min-width: 180px; flex: 1; }
  .card-label { display: block; font-size: 11px; color: var(--text-muted); text-transform: uppercase; letter-spacing: 0.5px; margin-bottom: 4px; }
  .card-value { display: block; font-size: 22px; font-weight: 700; color: var(--accent, #60a5fa); }
  .card-value.profile { text-transform: capitalize; }

  .bar-track { height: 8px; background: var(--border); border-radius: 4px; margin-top: 8px; overflow: hidden; }
  .bar-fill { height: 100%; border-radius: 4px; transition: width 0.3s; }
  .bar-label { font-size: 11px; color: var(--text-muted); margin-top: 2px; display: block; }

  .flags-row { display: flex; gap: 10px; margin-bottom: 20px; flex-wrap: wrap; }
  .flag { padding: 6px 14px; border-radius: 16px; font-size: 12px; font-weight: 600; border: 1px solid var(--border); background: var(--surface); }
  .flag.active { background: rgba(239, 68, 68, 0.15); color: #ef4444; border-color: #ef4444; }
  .flag.green { background: rgba(34, 197, 94, 0.1); color: #22c55e; border-color: #22c55e; }

  .data-table { width: 100%; max-width: 500px; border-collapse: collapse; font-size: 13px; }
  .data-table th { text-align: left; border-bottom: 1px solid var(--border); padding: 6px 10px; font-weight: 600; color: var(--text-muted); font-size: 11px; text-transform: uppercase; }
  .data-table td { padding: 5px 10px; border-bottom: 1px solid var(--border-light, rgba(255,255,255,0.05)); }
  .num { text-align: right; font-variant-numeric: tabular-nums; }

  .profile-btns { display: flex; gap: 8px; flex-wrap: wrap; }
  .profile-btns button { padding: 8px 20px; border-radius: 6px; border: 1px solid var(--border); background: var(--surface); color: var(--text); cursor: pointer; font-size: 13px; text-transform: capitalize; }
  .profile-btns button.selected { background: var(--accent, #60a5fa); color: #fff; border-color: var(--accent, #60a5fa); font-weight: 600; }
  .profile-btns button:hover:not(.selected) { background: var(--surface-hover); }
  .profile-btns button:disabled { opacity: 0.5; cursor: not-allowed; }
  .profile-msg { margin-top: 8px; font-size: 13px; color: var(--text-muted); }

  h3 { margin: 0 0 12px; font-size: 14px; }
  .error { color: #ef4444; }
</style>
