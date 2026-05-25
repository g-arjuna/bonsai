<script>
  import { onMount } from 'svelte';

  const SIGNAL_TYPES = ['gnmi', 'syslog', 'snmp', 'bmp', 'netflow', 'sflow', 'bgp_ls', 'otlp'];
  const SCOPE_LABELS = { device: 'Device', site: 'Site', role: 'Role' };
  const SCOPE_PLACEHOLDERS = {
    device: 'e.g. 192.168.1.1',
    site: 'e.g. backbone-dc1',
    role: 'e.g. backbone, spine, pe',
  };

  let tab = $state('device');
  let summary = $state({ scopes: [] });
  let loading = $state(true);
  let saving = $state(false);
  let error = $state('');
  let toast = $state('');

  let newScopeValue = $state('');
  let newSignalType = $state(SIGNAL_TYPES[0]);
  let newEnabled = $state(false);
  let newReason = $state('');

  async function load() {
    loading = true;
    error = '';
    try {
      const r = await fetch('/api/signal-policy/summary');
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      summary = await r.json();
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  onMount(load);

  function scopesForTab() {
    return summary.scopes.filter(s => s.scope_type === tab);
  }

  async function toggleSignal(scopeType, scopeValue, signalType, currentEnabled) {
    saving = true;
    try {
      const r = await fetch('/api/signal-policy', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          scope_type: scopeType,
          scope_value: scopeValue,
          signal_type: signalType,
          enabled: !currentEnabled,
          reason: 'ui toggle',
        }),
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      await load();
      toast = `${signalType} ${!currentEnabled ? 'enabled' : 'disabled'} for ${scopeValue}`;
      setTimeout(() => (toast = ''), 3000);
    } catch (e) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  async function removePolicy(id) {
    saving = true;
    try {
      const r = await fetch(`/api/signal-policy/${encodeURIComponent(id)}`, { method: 'DELETE' });
      if (!r.ok && r.status !== 204) throw new Error(`HTTP ${r.status}`);
      await load();
      toast = 'Policy removed';
      setTimeout(() => (toast = ''), 3000);
    } catch (e) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  async function addPolicy(e) {
    e.preventDefault();
    if (!newScopeValue.trim()) return;
    saving = true;
    try {
      const r = await fetch('/api/signal-policy', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          scope_type: tab,
          scope_value: newScopeValue.trim(),
          signal_type: newSignalType,
          enabled: newEnabled,
          reason: newReason.trim() || 'manual',
        }),
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      newScopeValue = '';
      newReason = '';
      await load();
      toast = `Policy added for ${newScopeValue || '...'}`;
      setTimeout(() => (toast = ''), 3000);
    } catch (e) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  function signalEnabled(scope, sigType) {
    return scope.signals[sigType] !== false;
  }

  function anyOverride(scope) {
    return Object.values(scope.signals).some(v => v === false);
  }
</script>

<div class="sc-page">
  <div class="sc-header">
    <h1 class="sc-title">Stream Controls</h1>
    <p class="sc-subtitle">
      Selectively suppress or enable telemetry signal types per device, site, or role.
      Scope precedence: <strong>device › role › site › default (allow)</strong>.
      Changes take effect within 30 s (next filter cache refresh).
    </p>
  </div>

  {#if error}
    <div class="sc-error" role="alert">{error} <button onclick={() => (error = '')}>×</button></div>
  {/if}
  {#if toast}
    <div class="sc-toast" role="status">{toast}</div>
  {/if}

  <div class="sc-tabs" role="tablist">
    {#each Object.entries(SCOPE_LABELS) as [key, label]}
      <button
        role="tab"
        aria-selected={tab === key}
        class:active={tab === key}
        onclick={() => (tab = key)}
      >{label}</button>
    {/each}
  </div>

  <!-- Add new policy row -->
  <form class="sc-add-row" onsubmit={addPolicy}>
    <input
      class="sc-input"
      type="text"
      placeholder={SCOPE_PLACEHOLDERS[tab]}
      bind:value={newScopeValue}
      required
      aria-label="{SCOPE_LABELS[tab]} value"
    />
    <select class="sc-select" bind:value={newSignalType} aria-label="Signal type">
      {#each SIGNAL_TYPES as s}
        <option value={s}>{s}</option>
      {/each}
    </select>
    <label class="sc-toggle-label">
      <input type="checkbox" bind:checked={newEnabled} /> Enabled
    </label>
    <input
      class="sc-input sc-reason"
      type="text"
      placeholder="Reason (optional)"
      bind:value={newReason}
      aria-label="Reason"
    />
    <button class="sc-btn sc-btn--primary" type="submit" disabled={saving}>Add / Update</button>
  </form>

  {#if loading}
    <div class="sc-loading">Loading…</div>
  {:else}
    {@const rows = scopesForTab()}
    {#if rows.length === 0}
      <div class="sc-empty">No signal policies for {SCOPE_LABELS[tab].toLowerCase()} scope. All signals are allowed by default.</div>
    {:else}
      <div class="sc-matrix-wrap">
        <table class="sc-matrix" aria-label="Signal policy matrix">
          <thead>
            <tr>
              <th>{SCOPE_LABELS[tab]}</th>
              {#each SIGNAL_TYPES as s}
                <th class="sc-sig-col">{s}</th>
              {/each}
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each rows as scope}
              <tr class:has-override={anyOverride(scope)}>
                <td class="sc-scope-cell">
                  <span class="sc-scope-value">{scope.scope_value}</span>
                </td>
                {#each SIGNAL_TYPES as sig}
                  {@const en = signalEnabled(scope, sig)}
                  {@const hasPolicy = sig in scope.signals}
                  <td class="sc-cell">
                    {#if hasPolicy}
                      <button
                        class="sc-pill"
                        class:sc-pill--on={en}
                        class:sc-pill--off={!en}
                        onclick={() => toggleSignal(scope.scope_type, scope.scope_value, sig, en)}
                        disabled={saving}
                        title="{en ? 'Click to disable' : 'Click to enable'} {sig} for {scope.scope_value}"
                        aria-label="{sig} {en ? 'enabled' : 'disabled'} — click to toggle"
                      >{en ? 'ON' : 'OFF'}</button>
                    {:else}
                      <span class="sc-pill sc-pill--default" title="Default (allow) — click to add override"
                        onclick={() => toggleSignal(scope.scope_type, scope.scope_value, sig, true)}
                        role="button" tabindex="0"
                        onkeydown={(e) => e.key === 'Enter' && toggleSignal(scope.scope_type, scope.scope_value, sig, true)}
                      >–</span>
                    {/if}
                  </td>
                {/each}
                <td>
                  {#each Object.keys(scope.signals) as sig}
                    <button
                      class="sc-remove-btn"
                      onclick={() => removePolicy(`${scope.scope_type}:${scope.scope_value}:${sig}`)}
                      disabled={saving}
                      title="Remove {sig} policy for {scope.scope_value}"
                      aria-label="Remove {sig} override for {scope.scope_value}"
                    >✕ {sig}</button>
                  {/each}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  {/if}
</div>

<style>
  .sc-page { padding: 24px; max-width: 1200px; }
  .sc-header { margin-bottom: 20px; }
  .sc-title { font-size: 1.4rem; font-weight: 600; margin: 0 0 6px; }
  .sc-subtitle { font-size: 0.85rem; color: var(--text-muted, #888); margin: 0; line-height: 1.5; }

  .sc-error {
    background: #fee2e2; color: #b91c1c; border: 1px solid #fca5a5;
    border-radius: 6px; padding: 8px 12px; margin-bottom: 12px;
    display: flex; justify-content: space-between; align-items: center; font-size: 0.85rem;
  }
  .sc-error button { background: none; border: none; cursor: pointer; font-size: 1rem; color: #b91c1c; }
  .sc-toast {
    background: #d1fae5; color: #065f46; border: 1px solid #6ee7b7;
    border-radius: 6px; padding: 8px 12px; margin-bottom: 12px; font-size: 0.85rem;
  }

  .sc-tabs { display: flex; gap: 4px; margin-bottom: 16px; border-bottom: 2px solid var(--border, #e5e7eb); }
  .sc-tabs button {
    background: none; border: none; padding: 8px 18px; cursor: pointer;
    font-size: 0.9rem; color: var(--text-muted, #6b7280); border-bottom: 2px solid transparent;
    margin-bottom: -2px; transition: color 0.15s, border-color 0.15s;
  }
  .sc-tabs button.active { color: var(--accent, #2563eb); border-bottom-color: var(--accent, #2563eb); font-weight: 600; }

  .sc-add-row {
    display: flex; gap: 8px; align-items: center; flex-wrap: wrap;
    background: var(--surface-2, #f9fafb); border: 1px solid var(--border, #e5e7eb);
    border-radius: 8px; padding: 12px 16px; margin-bottom: 16px;
  }
  .sc-input {
    padding: 6px 10px; border: 1px solid var(--border, #d1d5db); border-radius: 6px;
    font-size: 0.85rem; min-width: 180px; background: var(--surface, #fff); color: var(--text, #111);
  }
  .sc-reason { min-width: 160px; }
  .sc-select {
    padding: 6px 8px; border: 1px solid var(--border, #d1d5db); border-radius: 6px;
    font-size: 0.85rem; background: var(--surface, #fff); color: var(--text, #111);
  }
  .sc-toggle-label { font-size: 0.85rem; display: flex; align-items: center; gap: 5px; cursor: pointer; }
  .sc-btn {
    padding: 6px 14px; border-radius: 6px; font-size: 0.85rem; cursor: pointer;
    border: none; font-weight: 500; transition: opacity 0.15s;
  }
  .sc-btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .sc-btn--primary { background: var(--accent, #2563eb); color: #fff; }
  .sc-btn--primary:hover:not(:disabled) { opacity: 0.88; }

  .sc-loading, .sc-empty {
    color: var(--text-muted, #6b7280); font-size: 0.9rem; padding: 24px 0;
    text-align: center;
  }

  .sc-matrix-wrap { overflow-x: auto; }
  .sc-matrix { border-collapse: collapse; width: 100%; font-size: 0.85rem; }
  .sc-matrix th, .sc-matrix td {
    border: 1px solid var(--border, #e5e7eb); padding: 7px 10px; text-align: center;
  }
  .sc-matrix th { background: var(--surface-2, #f3f4f6); font-weight: 600; font-size: 0.8rem; }
  .sc-sig-col { min-width: 72px; }
  .sc-scope-cell { text-align: left; min-width: 160px; }
  .sc-scope-value { font-family: monospace; font-size: 0.82rem; }
  tr.has-override { background: #fffbeb; }

  .sc-cell { padding: 4px 8px; }
  .sc-pill {
    display: inline-block; padding: 2px 10px; border-radius: 12px; font-size: 0.75rem;
    font-weight: 600; cursor: pointer; border: none; transition: opacity 0.12s;
    min-width: 40px;
  }
  .sc-pill:disabled { opacity: 0.5; cursor: not-allowed; }
  .sc-pill--on  { background: #d1fae5; color: #065f46; }
  .sc-pill--off { background: #fee2e2; color: #b91c1c; }
  .sc-pill--default { background: transparent; color: var(--text-muted, #9ca3af); cursor: pointer; }
  .sc-pill--default:hover { background: var(--surface-2, #f3f4f6); }

  .sc-remove-btn {
    background: none; border: 1px solid var(--border, #e5e7eb); border-radius: 4px;
    font-size: 0.72rem; padding: 2px 6px; cursor: pointer; color: var(--text-muted, #6b7280);
    display: block; margin: 2px 0; transition: background 0.12s;
  }
  .sc-remove-btn:hover:not(:disabled) { background: #fee2e2; color: #b91c1c; border-color: #fca5a5; }
  .sc-remove-btn:disabled { opacity: 0.4; cursor: not-allowed; }
</style>
