<script>
  import { onMount } from 'svelte';
  import { toast } from '$lib/toast.svelte.js';
  import { relativeTime, absoluteTime } from '$lib/timeutil.js';

  let credentials = $state([]);
  let loading = $state(true);

  let alias = $state('');
  let username = $state('');
  let password = $state('');
  let saving = $state(false);

  let rotateAlias = $state('');
  let rotatePassword = $state('');
  let rotating = $state(false);

  let testAlias = $state('');
  let testAddress = $state('');
  let testing = $state(false);

  onMount(loadCredentials);

  async function loadCredentials() {
    loading = true;
    try {
      const r = await fetch('/api/credentials');
      if (!r.ok) throw new Error(await r.text());
      const data = await r.json();
      credentials = data.credentials ?? [];
    } catch (e) {
      toast(e.message, 'error');
      credentials = [];
    } finally {
      loading = false;
    }
  }

  async function addCredential() {
    if (!alias.trim() || !username.trim() || !password) return;
    saving = true;
    try {
      const r = await fetch('/api/credentials', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ alias: alias.trim(), username: username.trim(), password }),
      });
      const data = await r.json();
      if (!data.success) throw new Error(data.error);
      toast(`Alias "${alias.trim()}" saved.`, 'success');
      alias = '';
      username = '';
      password = '';
      await loadCredentials();
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      saving = false;
    }
  }

  async function rotateCredential() {
    if (!rotateAlias || !rotatePassword) return;
    rotating = true;
    try {
      const r = await fetch('/api/credentials/update', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ alias: rotateAlias, username: '', password: rotatePassword }),
      });
      const data = await r.json();
      if (!data.success) throw new Error(data.error);
      toast(`Rotated password for "${rotateAlias}".`, 'success');
      rotatePassword = '';
      await loadCredentials();
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      rotating = false;
    }
  }

  async function testCredential() {
    if (!testAlias || !testAddress.trim()) return;
    testing = true;
    try {
      const r = await fetch('/api/credentials/test', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ alias: testAlias, address: testAddress.trim() }),
      });
      if (!r.ok) throw new Error(await r.text());
      const report = await r.json();
      toast(`Credential test reached ${report.address || testAddress.trim()} successfully.`, 'success');
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      testing = false;
    }
  }

  async function removeCredential(targetAlias) {
    try {
      const r = await fetch('/api/credentials/remove', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ alias: targetAlias }),
      });
      const data = await r.json();
      if (!data.success) throw new Error(data.error);
      toast(`Removed alias "${targetAlias}".`, 'success');
      if (rotateAlias === targetAlias) rotateAlias = '';
      if (testAlias === targetAlias) testAlias = '';
      await loadCredentials();
    } catch (e) {
      toast(e.message, 'error');
    }
  }

  function selectAlias(targetAlias) {
    rotateAlias = targetAlias;
    testAlias = targetAlias;
  }
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">Security</p>
      <h2>Credentials</h2>
    </div>
  </div>

  <div class="notice" style="border-color: rgba(88,166,255,0.3); background: rgba(88,166,255,0.05); margin-bottom: 20px;">
    Credentials are stored in the encrypted vault only. Rotate aliases here, and keep device records pointed at immutable alias names instead of inline passwords.
  </div>

  <div class="cred-grid">
    <div class="card">
      <h3 style="margin-bottom: 14px; font-size: 16px;">Add alias</h3>
      <div class="credential-form">
        <input bind:value={alias} placeholder="Alias (e.g. lab-admin)" autocomplete="off" />
        <input bind:value={username} placeholder="Username" autocomplete="off" />
        <input bind:value={password} placeholder="Password" type="password" autocomplete="new-password" />
        <button onclick={addCredential} disabled={saving || !alias.trim() || !username.trim() || !password}>
          {saving ? 'Saving…' : 'Save alias'}
        </button>
      </div>
    </div>

    <div class="card">
      <h3 style="margin-bottom: 14px; font-size: 16px;">Rotate password</h3>
      <div class="credential-form">
        <select bind:value={rotateAlias}>
          <option value="">Select alias</option>
          {#each credentials as cred}
            <option value={cred.alias}>{cred.alias}</option>
          {/each}
        </select>
        <input bind:value={rotatePassword} placeholder="New password" type="password" autocomplete="new-password" />
        <button onclick={rotateCredential} disabled={rotating || !rotateAlias || !rotatePassword}>
          {rotating ? 'Updating…' : 'Rotate password'}
        </button>
      </div>
    </div>

    <div class="card">
      <h3 style="margin-bottom: 14px; font-size: 16px;">Test alias</h3>
      <div class="credential-form">
        <select bind:value={testAlias}>
          <option value="">Select alias</option>
          {#each credentials as cred}
            <option value={cred.alias}>{cred.alias}</option>
          {/each}
        </select>
        <input bind:value={testAddress} placeholder="Test device address" autocomplete="off" />
        <button onclick={testCredential} disabled={testing || !testAlias || !testAddress.trim()}>
          {testing ? 'Testing…' : 'Test credential'}
        </button>
      </div>
    </div>

    <div class="card">
      <h3 style="margin-bottom: 14px; font-size: 16px;">Stored aliases</h3>
      {#if loading}
        <div class="muted">Loading…</div>
      {:else if credentials.length === 0}
        <div class="empty" style="padding: 20px;">No aliases stored yet.</div>
      {:else}
        <table>
          <thead>
            <tr>
              <th>Alias</th>
              <th>Devices</th>
              <th>Updated</th>
              <th>Last used</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each credentials as cred}
              <tr>
                <td><button class="row-link" onclick={() => selectAlias(cred.alias)}><code>{cred.alias}</code></button></td>
                <td>{cred.device_count ?? 0}</td>
                <td title={absoluteTime(cred.updated_at_ns)}>{relativeTime(cred.updated_at_ns)}</td>
                <td title={absoluteTime(cred.last_used_at_ns)}>{cred.last_used_at_ns ? relativeTime(cred.last_used_at_ns) : '—'}</td>
                <td><button class="danger" onclick={() => removeCredential(cred.alias)}>Remove</button></td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>
  </div>
</div>

<style>
  .cred-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); gap: 16px; }
  .credential-form { display: grid; gap: 10px; }
  .row-link {
    border: 0;
    background: transparent;
    color: inherit;
    cursor: pointer;
    padding: 0;
    font: inherit;
  }
</style>
