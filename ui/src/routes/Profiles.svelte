<script>
  import { onMount } from 'svelte';

  const ENV_BADGE = {
    data_center:      'DC',
    campus_wired:     'Campus',
    campus_wireless:  'Wireless',
    service_provider: 'SP',
    home_lab:         'Lab',
  };

  let profiles    = $state([]);
  let plugins     = $state([]);
  let loadErrors  = $state([]);
  let loading     = $state(true);
  let selected    = $state(null);
  let filter      = $state('');
  let activeTab   = $state('profiles');
  let overrides   = $state([]);
  let newOverride = $state({ type: 'site', site: '', role: '', environment: 'data_center', path: '', action: 'add', sample_interval_s: 10, optional: false });

  onMount(loadProfiles);

  async function loadOverrides() {
    try {
      const r = await fetch('/api/overrides');
      if (r.ok) overrides = await r.json();
    } catch (e) {}
  }
  onMount(loadOverrides);

  async function createOverride() {
    let scope = {};
    if (newOverride.type === 'site') scope = { site: newOverride.site };
    else if (newOverride.type === 'role_env') scope = { role_env: { role: newOverride.role, environment: newOverride.environment } };
    else scope = { device: newOverride.device };

    const payload = {
      scope,
      path: newOverride.path,
      action: newOverride.action,
      sample_interval_s: newOverride.sample_interval_s || null,
      optional: newOverride.optional ? true : null
    };

    const r = await fetch('/api/overrides', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload)
    });
    if (r.ok) {
      await loadOverrides();
      newOverride.path = '';
    } else {
      alert(await r.text());
    }
  }

  async function removeOverride(o) {
    const r = await fetch('/api/overrides/remove', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ scope: o.scope, path: o.path })
    });
    if (r.ok) await loadOverrides();
  }


  async function loadProfiles() {
    loading = true;
    try {
      const r = await fetch('/api/profiles');
      if (!r.ok) throw new Error(await r.text());
      const data = await r.json();
      profiles   = data.profiles   ?? [];
      plugins    = data.plugins    ?? [];
      loadErrors = data.load_errors ?? [];
    } catch (e) {
      loadErrors = [e.message];
    } finally {
      loading = false;
    }
  }

  let filtered = $derived(
    filter.trim()
      ? profiles.filter(p =>
          p.name.includes(filter) ||
          p.roles.some(r => r.includes(filter)) ||
          p.environment.some(e => e.includes(filter))
        )
      : profiles
  );
</script>

<div class="workspace">
  <div class="workspace-header">
    <h1>Profiles</h1>
    <p class="muted">Path profiles loaded at startup. Add plugins to <code>config/path_profiles/plugins/</code>.</p>
  </div>

  {#if loadErrors.length > 0}
    <div class="error-banner">
      <strong>Load errors ({loadErrors.length})</strong>
      <ul>
        {#each loadErrors as err}
          <li>{err}</li>
        {/each}
      </ul>
    </div>
  {/if}

  
  <div class="tabs">
    <button class="tab" class:active={activeTab === 'profiles'} onclick={() => activeTab = 'profiles'}>Catalogue Profiles</button>
    <button class="tab" class:active={activeTab === 'overrides'} onclick={() => activeTab = 'overrides'}>Path Overrides</button>
  </div>

  {#if activeTab === "profiles"}
  <div class="split">
    <!-- Left: profile list -->
    <div class="list-col">
      <div class="search-row">
        <input
          bind:value={filter}
          placeholder="Filter by name, role, or environment…"
          class="search-input"
        />
        <span class="count-label">
          {#if loading}
            Loading…
          {:else}
            {filtered.length} / {profiles.length} profiles
          {/if}
        </span>
      </div>

      {#if loading}
        <div class="empty-state">Loading profiles…</div>
      {:else if filtered.length === 0}
        <div class="empty-state">No profiles match the filter.</div>
      {:else}
        <ul class="profile-list">
          {#each filtered as p}
            <li
              class="profile-row"
              class:active={selected?.name === p.name}
              onclick={() => selected = p}
              role="button"
              tabindex="0"
              onkeydown={(e) => e.key === 'Enter' && (selected = p)}
            >
              <div class="profile-name">{p.name}</div>
              <div class="badge-row">
                {#each p.environment as env}
                  <span class="badge env-badge">{ENV_BADGE[env] ?? env}</span>
                {/each}
                {#each p.roles as role}
                  <span class="badge role-badge">{role}</span>
                {/each}
                {#if p.source !== 'built-in'}
                  <span class="badge plugin-badge">plugin</span>
                {/if}
              </div>
              <div class="profile-meta muted">{p.path_count} paths</div>
            </li>
          {/each}
        </ul>
      {/if}

      {#if plugins.length > 0}
        <div class="section-header">Plugins ({plugins.length})</div>
        <ul class="plugin-list">
          {#each plugins as pg}
            <li class="plugin-row" class:has-conflicts={pg.conflicts.length > 0}>
              <div class="plugin-name">{pg.name}</div>
              <div class="muted small">v{pg.version}{pg.author ? ` · ${pg.author}` : ''}</div>
              <div class="muted small">{pg.profile_count} profile{pg.profile_count !== 1 ? 's' : ''}</div>
              {#if pg.conflicts.length > 0}
                <div class="conflict-list">
                  {#each pg.conflicts as c}
                    <div class="conflict-item">⚠ {c}</div>
                  {/each}
                </div>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    <!-- Right: profile detail -->
    <div class="detail-col">
      {#if selected}
        <div class="detail-card">
          <div class="detail-header">
            <h2>{selected.name}</h2>
            <span class="source-label">{selected.source}</span>
          </div>

          {#if selected.description}
            <p class="detail-desc">{selected.description}</p>
          {/if}
          {#if selected.rationale}
            <p class="detail-rationale muted">{selected.rationale}</p>
          {/if}

          <div class="detail-section">
            <div class="detail-label">Environments</div>
            <div class="badge-row">
              {#each selected.environment as env}
                <span class="badge env-badge">{ENV_BADGE[env] ?? env}</span>
              {:else}
                <span class="muted small">All environments</span>
              {/each}
            </div>
          </div>

          <div class="detail-section">
            <div class="detail-label">Roles</div>
            <div class="badge-row">
              {#each selected.roles as role}
                <span class="badge role-badge">{role}</span>
              {:else}
                <span class="muted small">None</span>
              {/each}
            </div>
          </div>

          {#if selected.vendor_scope && selected.vendor_scope.length > 0}
            <div class="detail-section">
              <div class="detail-label">Vendor scope</div>
              <div class="badge-row">
                {#each selected.vendor_scope as v}
                  <span class="badge vendor-badge">{v}</span>
                {/each}
              </div>
            </div>
          {/if}

          <div class="detail-section">
            <div class="detail-label">Paths ({selected.path_count})</div>
            <div class="muted small">Expand in Discovery results to see filtered paths per device.</div>
          </div>
        </div>
      {:else}
        <div class="empty-state">Select a profile to see details.</div>
      {/if}
    </div>
  </div>
  {/if}

  {#if activeTab === "overrides"}
    <div class="overrides-view">
      <div class="list-col">
        <div class="section-header">Existing Overrides</div>
        {#if overrides.length === 0}
          <div class="empty-state">No path overrides configured.</div>
        {:else}
          <ul class="profile-list">
            {#each overrides as o}
              <li class="profile-row" style="display:flex; justify-content:space-between;">
                <div>
                  <div class="profile-name">{o.path} <span class="badge" style="background:#444;color:#eee;">{o.action}</span></div>
                  <div class="profile-meta muted">
                    Scope: {o.scope.site ? `Site(${o.scope.site})` : o.scope.role_env ? `Role(${o.scope.role_env.role}) Env(${o.scope.role_env.environment})` : `Device(${o.scope.device})`}
                    {#if o.sample_interval_s} | {o.sample_interval_s}s{/if}
                  </div>
                </div>
                <button onclick={() => removeOverride(o)} style="background:transparent; border:1px solid #555; border-radius:4px; color:#fff; cursor:pointer;">Remove</button>
              </li>
            {/each}
          </ul>
        {/if}
      </div>
      <div class="detail-col">
        <div class="detail-card">
          <h3>Create Override</h3>
          <div class="form-group">
            <label>Scope Type</label>
            <select bind:value={newOverride.type} class="search-input">
              <option value="site">Site</option>
              <option value="role_env">Role + Environment</option>
              <option value="device">Device</option>
            </select>
          </div>
          {#if newOverride.type === "site"}
            <div class="form-group">
              <label>Site ID</label>
              <input bind:value={newOverride.site} class="search-input" placeholder="e.g. lab" />
            </div>
          {:else if newOverride.type === "role_env"}
            <div class="form-group">
              <label>Environment</label>
              <select bind:value={newOverride.environment} class="search-input">
                <option value="data_center">Data Center</option>
                <option value="campus_wired">Campus Wired</option>
                <option value="campus_wireless">Campus Wireless</option>
                <option value="service_provider">Service Provider</option>
                <option value="home_lab">Home Lab</option>
              </select>
            </div>
            <div class="form-group">
              <label>Role</label>
              <input bind:value={newOverride.role} class="search-input" placeholder="e.g. leaf" />
            </div>
          {:else}
            <div class="form-group">
              <label>Device Address</label>
              <input bind:value={newOverride.device} class="search-input" placeholder="e.g. 10.0.0.1:57400" />
            </div>
          {/if}
          <div class="form-group">
            <label>Action</label>
            <select bind:value={newOverride.action} class="search-input">
              <option value="add">Add Path</option>
              <option value="drop">Drop Path</option>
              <option value="modify">Modify Path</option>
            </select>
          </div>
          <div class="form-group">
            <label>Path</label>
            <input bind:value={newOverride.path} class="search-input" placeholder="/interfaces/interface[name=*]/state/counters" />
          </div>
          <div class="form-group">
            <label>Sample Interval (s)</label>
            <input type="number" bind:value={newOverride.sample_interval_s} class="search-input" />
          </div>
          <button class="primary-btn" style="margin-top:10px" onclick={createOverride}>Create</button>
        </div>
      </div>
    </div>
  {/if}
</div>

<style>

  .tabs { display: flex; gap: 10px; margin-bottom: 20px; border-bottom: 1px solid var(--border); padding-bottom: 10px; }
  .tab { background: transparent; border: none; color: var(--fg-muted, #888); font-size: 16px; cursor: pointer; padding: 4px 8px; border-radius: 4px; }
  .tab:hover { background: rgba(255,255,255,0.05); }
  .tab.active { color: var(--fg); font-weight: 600; background: rgba(255,255,255,0.1); }
  .overrides-view { display: grid; grid-template-columns: 1fr 320px; gap: 20px; }
  .form-group { margin-bottom: 12px; }
  .form-group label { display: block; font-size: 12px; color: var(--fg-muted); margin-bottom: 4px; }
  .primary-btn { background: var(--accent, #58a6ff); color: #000; border: none; padding: 8px 16px; border-radius: 6px; font-weight: 600; cursor: pointer; }

  .workspace { padding: 24px; max-width: 1100px; }
  .workspace-header { margin-bottom: 20px; }
  .workspace-header h1 { margin: 0 0 6px; font-size: 22px; font-weight: 600; }
  .workspace-header p { margin: 0; }
  code { font-family: monospace; font-size: 12px; background: var(--border); padding: 1px 4px; border-radius: 3px; }

  .error-banner {
    background: rgba(255, 80, 80, 0.1);
    border: 1px solid rgba(255, 80, 80, 0.4);
    border-radius: 6px;
    padding: 12px 16px;
    margin-bottom: 16px;
    font-size: 13px;
  }
  .error-banner ul { margin: 8px 0 0 16px; padding: 0; }

  .split { display: grid; grid-template-columns: 320px 1fr; gap: 20px; }

  .list-col { display: flex; flex-direction: column; gap: 8px; }
  .search-row { display: flex; align-items: center; gap: 10px; }
  .search-input { flex: 1; padding: 7px 10px; font-size: 13px; border: 1px solid var(--border); border-radius: 6px; background: var(--input-bg, #111); color: var(--fg); }
  .count-label { font-size: 12px; color: var(--fg-muted, #888); white-space: nowrap; }

  .profile-list, .plugin-list { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 4px; }
  .profile-row {
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: 6px;
    cursor: pointer;
    transition: border-color 0.15s, background 0.15s;
  }
  .profile-row:hover { border-color: var(--accent, #58a6ff); }
  .profile-row.active { border-color: var(--accent, #58a6ff); background: rgba(88,166,255,0.06); }
  .profile-name { font-size: 14px; font-weight: 600; margin-bottom: 4px; }
  .profile-meta { font-size: 12px; margin-top: 4px; }

  .badge-row { display: flex; flex-wrap: wrap; gap: 4px; }
  .badge { font-size: 11px; padding: 1px 6px; border-radius: 10px; font-weight: 600; }
  .env-badge { background: rgba(88,166,255,0.15); color: var(--accent, #58a6ff); }
  .role-badge { background: rgba(120,220,120,0.12); color: #6bdf6b; }
  .plugin-badge { background: rgba(255,180,50,0.15); color: #ffb432; }
  .vendor-badge { background: rgba(180,120,255,0.15); color: #c47aff; }

  .section-header { font-size: 11px; font-weight: 700; text-transform: uppercase; letter-spacing: 0.06em; color: var(--fg-muted, #888); margin-top: 16px; margin-bottom: 6px; }

  .plugin-row {
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .plugin-row.has-conflicts { border-color: rgba(255,180,50,0.5); }
  .plugin-name { font-size: 14px; font-weight: 600; margin-bottom: 2px; }
  .conflict-list { margin-top: 6px; }
  .conflict-item { font-size: 12px; color: #ffb432; }

  .detail-col { }
  .detail-card { background: var(--card-bg, #1a1a2e); border: 1px solid var(--border); border-radius: 8px; padding: 20px; }
  .detail-header { display: flex; align-items: baseline; gap: 10px; margin-bottom: 12px; }
  .detail-header h2 { margin: 0; font-size: 18px; }
  .source-label { font-size: 11px; background: var(--border); padding: 2px 7px; border-radius: 10px; color: var(--fg-muted, #888); }
  .detail-desc { margin: 0 0 8px; font-size: 14px; line-height: 1.5; }
  .detail-rationale { margin: 0 0 16px; font-size: 13px; line-height: 1.5; }
  .detail-section { margin-bottom: 14px; }
  .detail-label { font-size: 11px; font-weight: 700; text-transform: uppercase; letter-spacing: 0.06em; color: var(--fg-muted, #888); margin-bottom: 6px; }

  .empty-state { color: var(--fg-muted, #888); font-size: 14px; padding: 32px 0; text-align: center; }
  .muted { color: var(--fg-muted, #888); }
  .small { font-size: 12px; }
</style>
