<script>
  import { navigate } from '$lib/router.svelte.js';
  import { toast } from '$lib/toast.svelte.js';

  let { onComplete = () => {} } = $props();

  const ARCHETYPES = [
    { value: 'data_center',       label: 'Data Center',      desc: 'DC fabrics, spine/leaf, EVPN/BGP' },
    { value: 'campus_wired',      label: 'Campus Wired',     desc: 'Access/distribution/core LAN' },
    { value: 'campus_wireless',   label: 'Campus Wireless',  desc: 'APs, WLCs, wireless overlay' },
    { value: 'service_provider',  label: 'Service Provider', desc: 'Core, PE/P routers, MPLS, SR' },
    { value: 'home_lab',          label: 'Home Lab',         desc: 'ContainerLab, FRR, any topology' },
  ];

  const STEPS = [
    { id: 1, label: 'Welcome' },
    { id: 2, label: 'Environment' },
    { id: 3, label: 'Sites' },
    { id: 4, label: 'Credentials' },
    { id: 5, label: 'Integrations' },
    { id: 6, label: 'Ready' },
  ];

  let step = $state(1);

  // Step 2 — environment
  let envName      = $state('');
  let envArchetype = $state('home_lab');
  let envSaving    = $state(false);
  let envCreated   = $state(null);

  // Step 3 — site
  let siteName       = $state('');
  let siteKind       = $state('dc');
  let siteSaving     = $state(false);
  let siteCreated    = $state(null);

  // Step 4 — credentials
  let credAlias    = $state('');
  let credUsername = $state('');
  let credPassword = $state('');
  let credSaving   = $state(false);
  let credCreated  = $state(null);

  // Step 5 — ServiceNow integration (optional)
  let snowInstanceUrl     = $state('');
  let snowCredAlias       = $state('servicenow-pdi');
  let snowUsername        = $state('');
  let snowPassword        = $state('');
  let snowSaving          = $state(false);
  let snowTestResult      = $state(null);  // null | { success, message }

  async function createEnvironment() {
    if (!envName.trim()) return;
    envSaving = true;
    try {
      const r = await fetch('/api/environments', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: envName.trim(), archetype: envArchetype }),
      });
      const data = await r.json();
      if (!data.success) throw new Error(data.error);
      envCreated = { name: envName.trim(), archetype: envArchetype };
      step = 3;
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      envSaving = false;
    }
  }

  async function createSite() {
    if (!siteName.trim()) return;
    siteSaving = true;
    try {
      const r = await fetch('/api/sites', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          name: siteName.trim(),
          kind: siteKind,
          parent_id: '',
          lat: 0,
          lon: 0,
          metadata_json: '{}',
        }),
      });
      const data = await r.json();
      if (!data.success) throw new Error(data.error);
      siteCreated = { name: siteName.trim(), kind: siteKind };
      step = 4;
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      siteSaving = false;
    }
  }

  async function createCredential() {
    if (!credAlias.trim() || !credUsername.trim() || !credPassword) return;
    credSaving = true;
    try {
      const r = await fetch('/api/credentials', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          alias: credAlias.trim(),
          username: credUsername.trim(),
          password: credPassword,
        }),
      });
      const data = await r.json();
      if (!data.success) throw new Error(data.error);
      credCreated = { alias: credAlias.trim() };
      step = 5;
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      credSaving = false;
    }
  }

  async function testSnowConnection() {
    if (!snowInstanceUrl.trim() || !snowCredAlias.trim()) return;
    snowSaving = true;
    snowTestResult = null;
    // First, store the credential so the test endpoint can resolve it
    try {
      if (snowUsername.trim() && snowPassword) {
        await fetch('/api/credentials', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            alias: snowCredAlias.trim(),
            username: snowUsername.trim(),
            password: snowPassword,
          }),
        });
      }
      const r = await fetch('/api/integrations/servicenow/test', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          instance_url: snowInstanceUrl.trim(),
          credential_alias: snowCredAlias.trim(),
        }),
      });
      const data = await r.json();
      snowTestResult = { success: data.success, message: data.message };
    } catch (e) {
      snowTestResult = { success: false, message: e.message };
    } finally {
      snowSaving = false;
    }
  }

  function goToDevices() {
    onComplete();
    navigate('/devices/new');
  }

  async function skipSetup() {
    try {
      await fetch('/api/environments', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: 'Home Lab', archetype: 'home_lab' }),
      });
      await fetch('/api/sites', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: 'default-site', kind: 'other', parent_id: '', lat: 0, lon: 0, metadata_json: '{}' }),
      });
    } catch (_) {
      // non-fatal — baseline state creation is best-effort
    }
    onComplete();
    navigate('/');
  }
</script>

<div class="setup-shell">
  <div class="setup-card">
    <div class="setup-brand">bonsai</div>

    <div class="step-trail">
      {#each STEPS as s}
        <div
          class="step-dot"
          class:done={step > s.id}
          class:active={step === s.id}
        >{s.id}</div>
      {/each}
    </div>

    {#if step === 1}
      <div class="setup-body">
        <h2>Welcome to bonsai</h2>
        <p class="muted">
          A streaming-first network state engine. bonsai ingests gNMI telemetry, builds a
          graph of your network, and closes a detect-predict-heal loop.
        </p>
        <p class="muted">
          This wizard takes about two minutes to configure your first environment, site, and
          credential. You can also skip it and configure everything from the workspaces.
        </p>
        <div class="btn-row">
          <button class="primary" onclick={() => step = 2}>Get started</button>
          <button class="ghost" onclick={skipSetup}>Skip, take me to the dashboard</button>
        </div>
      </div>

    {:else if step === 2}
      <div class="setup-body">
        <h2>Define an environment</h2>
        <p class="muted">
          An environment groups sites and devices by their operational context.
          Choose the archetype that best describes your network — it shapes default
          role options and path-profile recommendations.
        </p>

        <div class="form-stack">
          <div class="form-row">
            <label for="setup-env-name">Name</label>
            <input
              id="setup-env-name"
              bind:value={envName}
              placeholder="e.g. Lab DC Fabric"
              autocomplete="off"
            />
          </div>

          <div class="archetype-grid">
            {#each ARCHETYPES as arch}
              <label class="archetype-option" class:selected={envArchetype === arch.value}>
                <input
                  type="radio"
                  name="archetype"
                  value={arch.value}
                  bind:group={envArchetype}
                />
                <strong>{arch.label}</strong>
                <span class="muted small">{arch.desc}</span>
              </label>
            {/each}
          </div>
        </div>

        <div class="btn-row">
          <button
            class="primary"
            onclick={createEnvironment}
            disabled={envSaving || !envName.trim()}
          >
            {envSaving ? 'Creating…' : 'Create environment'}
          </button>
          <button class="ghost" onclick={() => step = 3}>Skip this step</button>
        </div>
      </div>

    {:else if step === 3}
      <div class="setup-body">
        <h2>Add a top-level site</h2>
        {#if envCreated}
          <p class="muted">
            Environment <strong>{envCreated.name}</strong> created.
            Now define at least one site — a data centre, PoP, campus, or region.
          </p>
        {:else}
          <p class="muted">
            Define at least one site — a data centre, PoP, campus, or region.
            You can build the full hierarchy later from the Sites workspace.
          </p>
        {/if}

        <div class="form-stack">
          <div class="form-row">
            <label for="setup-site-name">Site name</label>
            <input
              id="setup-site-name"
              bind:value={siteName}
              placeholder="e.g. dc-lab-01"
              autocomplete="off"
            />
          </div>
          <div class="form-row">
            <label for="setup-site-kind">Kind</label>
            <select id="setup-site-kind" bind:value={siteKind}>
              <option value="region">region</option>
              <option value="dc">dc</option>
              <option value="pod">pod</option>
              <option value="rack">rack</option>
              <option value="other">other</option>
            </select>
          </div>
        </div>

        <div class="btn-row">
          <button
            class="primary"
            onclick={createSite}
            disabled={siteSaving || !siteName.trim()}
          >
            {siteSaving ? 'Creating…' : 'Create site'}
          </button>
          <button class="ghost" onclick={() => step = 4}>Skip this step</button>
        </div>
      </div>

    {:else if step === 4}
      <div class="setup-body">
        <h2>Add a credential alias</h2>
        {#if siteCreated}
          <p class="muted">
            Site <strong>{siteCreated.name}</strong> created.
            Now store a device credential. bonsai uses aliases — the username and password
            are encrypted in the local vault and never exposed in the API or UI.
          </p>
        {:else}
          <p class="muted">
            Store a device credential. bonsai uses aliases — the username and password
            are encrypted in the local vault and never exposed in the API or UI.
          </p>
        {/if}

        <div class="form-stack">
          <div class="form-row">
            <label for="setup-cred-alias">Alias</label>
            <input
              id="setup-cred-alias"
              bind:value={credAlias}
              placeholder="e.g. lab-admin"
              autocomplete="off"
            />
          </div>
          <div class="form-row">
            <label for="setup-cred-user">Username</label>
            <input
              id="setup-cred-user"
              bind:value={credUsername}
              placeholder="admin"
              autocomplete="off"
            />
          </div>
          <div class="form-row">
            <label for="setup-cred-pass">Password</label>
            <input
              id="setup-cred-pass"
              type="password"
              bind:value={credPassword}
              placeholder="••••••••"
              autocomplete="new-password"
            />
          </div>
        </div>

        <div class="btn-row">
          <button
            class="primary"
            onclick={createCredential}
            disabled={credSaving || !credAlias.trim() || !credUsername.trim() || !credPassword}
          >
            {credSaving ? 'Saving…' : 'Save credential'}
          </button>
          <button class="ghost" onclick={() => step = 5}>Skip this step</button>
        </div>
      </div>

    {:else if step === 5}
      <div class="setup-body">
        <h2>ServiceNow integration</h2>
        <p class="muted">
          Optional — connect bonsai to a ServiceNow PDI or production instance to enrich your
          graph with CMDB business context and push detection events to Event Management.
          You can configure this later from the Enrichment and Integrations workspaces.
        </p>

        <div class="form-stack">
          <div class="form-row">
            <label for="setup-snow-url">Instance URL</label>
            <input
              id="setup-snow-url"
              bind:value={snowInstanceUrl}
              placeholder="https://devXXXXXX.service-now.com"
              autocomplete="off"
            />
          </div>
          <div class="form-row">
            <label for="setup-snow-alias">Cred alias</label>
            <input
              id="setup-snow-alias"
              bind:value={snowCredAlias}
              placeholder="servicenow-pdi"
              autocomplete="off"
            />
          </div>
          <div class="form-row">
            <label for="setup-snow-user">Username</label>
            <input
              id="setup-snow-user"
              bind:value={snowUsername}
              placeholder="admin"
              autocomplete="off"
            />
          </div>
          <div class="form-row">
            <label for="setup-snow-pass">Password</label>
            <input
              id="setup-snow-pass"
              type="password"
              bind:value={snowPassword}
              placeholder="••••••••"
              autocomplete="new-password"
            />
          </div>
        </div>

        {#if snowTestResult}
          <div class="notice {snowTestResult.success ? 'ok' : 'error'}" style="margin-bottom:16px;">
            {snowTestResult.message}
          </div>
        {/if}

        <div class="btn-row">
          <button
            class="primary"
            onclick={testSnowConnection}
            disabled={snowSaving || !snowInstanceUrl.trim() || !snowCredAlias.trim()}
          >
            {snowSaving ? 'Testing…' : 'Save & test connection'}
          </button>
          <button class="ghost" onclick={() => step = 6}>Skip this step</button>
        </div>
      </div>

    {:else if step === 6}
      <div class="setup-body">
        <h2>You're ready</h2>
        {#if credCreated}
          <p class="muted">
            Credential alias <strong>{credCreated.alias}</strong> stored.
          </p>
        {/if}
        <p class="muted">
          Your environment, site, and credential are configured.
          Add your first device to start receiving telemetry.
        </p>

        <ul class="summary-list">
          {#if envCreated}<li>Environment: <strong>{envCreated.name}</strong></li>{/if}
          {#if siteCreated}<li>Site: <strong>{siteCreated.name}</strong></li>{/if}
          {#if credCreated}<li>Credential alias: <strong>{credCreated.alias}</strong></li>{/if}
          {#if snowTestResult?.success}<li>ServiceNow: <strong>{snowInstanceUrl}</strong></li>{/if}
        </ul>

        <div class="btn-row">
          <button class="primary" onclick={goToDevices}>Add my first device</button>
          <button class="ghost" onclick={skipSetup}>Go to dashboard</button>
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .setup-shell {
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 100vh;
    padding: 24px;
    box-sizing: border-box;
  }
  .setup-card {
    background: var(--card-bg, #1a1a2e);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 40px;
    width: 100%;
    max-width: 560px;
  }
  .setup-brand {
    font-size: 22px;
    font-weight: 700;
    letter-spacing: -0.5px;
    margin-bottom: 24px;
    color: var(--accent, #58a6ff);
  }
  .step-trail {
    display: flex;
    gap: 8px;
    margin-bottom: 32px;
  }
  .step-dot {
    width: 28px;
    height: 28px;
    border-radius: 50%;
    border: 2px solid var(--border);
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 12px;
    color: var(--fg-muted, #888);
    font-weight: 600;
  }
  .step-dot.active { border-color: var(--accent, #58a6ff); color: var(--accent, #58a6ff); }
  .step-dot.done { background: var(--accent, #58a6ff); border-color: var(--accent, #58a6ff); color: #fff; }
  .setup-body h2 { margin: 0 0 12px; font-size: 22px; font-weight: 600; }
  .setup-body > p { margin: 0 0 20px; line-height: 1.6; }
  .form-stack { display: grid; gap: 12px; margin-bottom: 24px; }
  .form-row { display: grid; grid-template-columns: 110px 1fr; align-items: center; gap: 8px; }
  .archetype-grid { display: grid; gap: 8px; grid-template-columns: 1fr 1fr; }
  .archetype-option {
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 12px;
    cursor: pointer;
    display: flex;
    flex-direction: column;
    gap: 4px;
    transition: border-color 0.15s;
  }
  .archetype-option input { display: none; }
  .archetype-option:hover { border-color: var(--accent, #58a6ff); }
  .archetype-option.selected { border-color: var(--accent, #58a6ff); background: rgba(88,166,255,0.07); }
  .archetype-option strong { font-size: 14px; }
  .btn-row { display: flex; gap: 10px; flex-wrap: wrap; align-items: center; }
  .primary { background: var(--accent, #58a6ff); color: #fff; border: none; padding: 10px 20px; border-radius: 6px; font-size: 14px; cursor: pointer; font-weight: 600; }
  .primary:disabled { opacity: 0.5; cursor: not-allowed; }
  .ghost { background: transparent; border: 1px solid var(--border); color: var(--fg-muted, #888); padding: 10px 16px; border-radius: 6px; font-size: 13px; cursor: pointer; }
  .ghost:hover { border-color: var(--fg-muted, #888); }
  .summary-list { padding-left: 20px; margin: 0 0 24px; line-height: 2; }
  .small { font-size: 12px; }
</style>
