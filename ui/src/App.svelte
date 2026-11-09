<script>
  import { path, navigate, matchRoute } from '$lib/router.svelte.js';
  import { getToasts, dismissToast } from '$lib/toast.svelte.js';
  import Live from './routes/Live.svelte';
  import Lazy from '$lib/Lazy.svelte';
  import CommandPalette from '$lib/CommandPalette.svelte';
  import DensityToggle from '$lib/components/DensityToggle.svelte';

  // Route-level code splitting: heavy views load on demand so the entry
  // bundle stays small. Loaders are stable references (Lazy caches by them).
  const loadIncidents      = () => import('./routes/Incidents.svelte');
  const loadDevices        = () => import('./routes/Devices.svelte');
  const loadCollectors     = () => import('./routes/Collectors.svelte');
  const loadSites          = () => import('./routes/Sites.svelte');
  const loadCredentials    = () => import('./routes/Credentials.svelte');
  const loadOperations     = () => import('./routes/Operations.svelte');
  const loadTraceRoute     = () => import('./routes/TraceRoute.svelte');
  const loadOnboarding     = () => import('$lib/Onboarding.svelte');
  const loadEnvironments   = () => import('./routes/Environments.svelte');
  const loadProfiles       = () => import('./routes/Profiles.svelte');
  const loadIntegrations   = () => import('./routes/Integrations.svelte');
  const loadApprovals      = () => import('./routes/Approvals.svelte');
  const loadExplorer       = () => import('./routes/Explorer.svelte');
  const loadInvestigations = () => import('./routes/Investigations.svelte');
  const loadSettings       = () => import('./routes/Settings.svelte');
  const loadAudit          = () => import('./routes/Audit.svelte');
  const loadHA             = () => import('./routes/HA.svelte');
  const loadSyslog         = () => import('./routes/Syslog.svelte');
  const loadDbManagement   = () => import('./routes/DbManagement.svelte');
  const loadGovernance     = () => import('./routes/Governance.svelte');
  const loadSnmp           = () => import('./routes/Snmp.svelte');
  const loadSidecar        = () => import('./routes/Sidecar.svelte');
  const loadUsers          = () => import('./routes/Users.svelte');
  const loadSynthesizerRules = () => import('./routes/SynthesizerRules.svelte');
  const loadEndpoints      = () => import('./routes/Endpoints.svelte');
  const loadCerts          = () => import('./routes/Certs.svelte');
  const loadStreamControl  = () => import('./routes/StreamControl.svelte');
  const loadRouting        = () => import('./routes/Routing.svelte');
  const loadLogin          = () => import('./routes/Login.svelte');

  // Cmd-1..9 workspace shortcuts mirror CommandPalette WORKSPACE_SHORTCUTS
  const NAV = [
    { href: '/',              label: 'Live',          icon: '◉', kbd: '1' },
    { href: '/incidents',     label: 'Incidents',     icon: '⚠', kbd: '2' },
    { href: '/devices',       label: 'Devices',       icon: '⊡', kbd: '3' },
    { href: '/operations',    label: 'Operations',    icon: '♡', kbd: '4' },
    { href: '/collectors',    label: 'Collectors',    icon: '⇄', kbd: '5' },
    { href: '/routing',       label: 'Routing & RIB', icon: '🧭' },
    { href: '/integrations',  label: 'Integrations',  icon: '⇌', kbd: '6' },
    { href: '/approvals',     label: 'Approvals',     icon: '✓', kbd: '7' },
    { href: '/explorer',      label: 'Explorer',      icon: '⬡', kbd: '8' },
    { href: '/investigations',label: 'Investigations',icon: '🔍', kbd: '9' },
    { href: '/ha',            label: 'HA',            icon: '⚡' },
    { href: '/environments',  label: 'Environments',  icon: '⬡' },
    { href: '/profiles',      label: 'Profiles',      icon: '📋' },
    { href: '/sites',         label: 'Sites',         icon: '◎' },
    { href: '/credentials',   label: 'Credentials',   icon: '⚿' },
    { href: '/audit',         label: 'Audit',         icon: '📜' },
    { href: '/syslog',        label: 'Syslog / Shun', icon: '⬇' },
    { href: '/db',             label: 'Database',      icon: '🗄' },
    { href: '/governance',     label: 'Governance',    icon: '⚖' },
    { href: '/snmp',          label: 'SNMP',           icon: '📡' },
    { href: '/sidecars',      label: 'Sidecars',       icon: '🤖' },
    { href: '/users',         label: 'Users &amp; Access', icon: '👤' },
    { href: '/config-library', label: 'Config Library',   icon: '📦' },
    { href: '/settings',      label: 'Settings',      icon: '⚙' },
    { href: '/endpoints',      label: 'Endpoints',     icon: '⬡' },
    { href: '/certs',          label: 'Certificates',  icon: '🔒' },
    { href: '/stream-control', label: 'Stream Control', icon: '⏻' },
  ];

  let isFirstRunChecked = $state(false);
  let isFirstRun        = $state(false);
  let healthInfo        = $state(null);
  let authState         = $state('checking'); // 'checking' | 'anon' | 'authed'
  let sessionUser       = $state('');

  async function checkFirstRun() {
    try {
      const r = await fetch('/api/setup/status');
      if (r.ok) {
        const data = await r.json();
        isFirstRun = !!data.is_first_run;
      }
    } catch (_) {
      // non-fatal
    } finally {
      isFirstRunChecked = true;
    }
  }

  async function handleLogin(data) {
    sessionUser = data.username || '';
    authState = 'authed';
    await checkFirstRun();
  }

  import { onMount } from 'svelte';
  onMount(async () => {
    try {
      const r = await fetch('/api/setup/status');
      if (r.status === 401 || r.status === 403) {
        authState = 'anon';
        return;
      }
      authState = 'authed';
      if (r.ok) {
        const data = await r.json();
        isFirstRun = !!data.is_first_run;
        isFirstRunChecked = true;
      } else {
        isFirstRunChecked = true;
      }
    } catch (_) {
      authState = 'authed';
      isFirstRunChecked = true;
    }

    const pollHealth = async () => {
      try {
        const hr = await fetch('/health');
        if (hr.ok || hr.status === 503) healthInfo = await hr.json();
      } catch (_) { /* non-fatal */ }
    };
    pollHealth();
    const hi = setInterval(pollHealth, 30_000);
    return () => clearInterval(hi);
  });

  function isActive(href) {
    const p = path();
    return href === '/' ? (p === '/' || p === '') : (p === href || p.startsWith(href + '/'));
  }

  let traceParams  = $derived(matchRoute('/trace/:id', path()));
  let deviceParams = $derived(matchRoute('/devices/:address', path()));
</script>

<a href="#main-content" class="skip-to-main">Skip to main content</a>
<div class="app-shell">
  <aside class="sidebar" aria-label="Application navigation">
    <div class="sidebar-brand" aria-label="Bonsai">bonsai</div>
    <nav aria-label="Primary navigation">
      <div class="nav-group" role="group" aria-label="Monitoring">
        <span class="nav-group-label">Monitor</span>
        {#each NAV.slice(0, 6) as item}
          <a href={'#' + item.href}
             class:active={isActive(item.href)}
             aria-current={isActive(item.href) ? 'page' : undefined}
             onclick={(e) => { e.preventDefault(); navigate(item.href); }}>
            <span class="nav-icon" aria-hidden="true">{item.icon}</span>
            {item.label}
            {#if item.kbd}
              <kbd class="nav-kbd" aria-label="Shortcut Command {item.kbd}">⌘{item.kbd}</kbd>
            {/if}
          </a>
        {/each}
      </div>
      <div class="nav-divider" role="separator"></div>
      <div class="nav-group" role="group" aria-label="Operations">
        <span class="nav-group-label">Operate</span>
        {#each NAV.slice(6, 10) as item}
          <a href={'#' + item.href}
             class:active={isActive(item.href)}
             aria-current={isActive(item.href) ? 'page' : undefined}
             onclick={(e) => { e.preventDefault(); navigate(item.href); }}>
            <span class="nav-icon" aria-hidden="true">{item.icon}</span>
            {item.label}
            {#if item.kbd}
              <kbd class="nav-kbd" aria-label="Shortcut Command {item.kbd}">⌘{item.kbd}</kbd>
            {/if}
          </a>
        {/each}
      </div>
      <div class="nav-divider" role="separator"></div>
      <div class="nav-group" role="group" aria-label="Configuration">
        <span class="nav-group-label">Configure</span>
        {#each NAV.slice(10) as item}
          <a href={'#' + item.href}
             class:active={isActive(item.href)}
             aria-current={isActive(item.href) ? 'page' : undefined}
             onclick={(e) => { e.preventDefault(); navigate(item.href); }}>
            <span class="nav-icon" aria-hidden="true">{item.icon}</span>
            {item.label}
            {#if item.kbd}
              <kbd class="nav-kbd">⌘{item.kbd}</kbd>
            {/if}
          </a>
        {/each}
      </div>
    </nav>
    <div class="sidebar-footer">
      <button class="palette-trigger"
              onclick={() => document.dispatchEvent(new KeyboardEvent('keydown', { ctrlKey: true, key: 'k', bubbles: true }))}>
        <span>⌨</span> Search <kbd>Ctrl+K</kbd>
      </button>
      <a href="/bonpy/" class="ml-console-link" title="BonPy MLOps Console">
        <span class="nav-icon" aria-hidden="true">🤖</span> ML Console
      </a>
      <div class="density-row">
        <DensityToggle />
      </div>
      {#if healthInfo}
        <div class="version-badge" title="status: {healthInfo.status}  build: {healthInfo.build_ts}">
          <span class="version-dot version-dot--{healthInfo.status === 'ok' ? 'ok' : healthInfo.status === 'degraded' ? 'degraded' : 'unknown'}"></span>
          <span class="version-text">v{healthInfo.version} · {healthInfo.git_sha}</span>
        </div>
      {/if}
    </div>
  </aside>

  <main id="main-content" class="main-content" tabindex="-1">
    {#if authState === 'anon'}
      <Lazy loader={loadLogin} onLogin={handleLogin} />
    {:else if !isFirstRunChecked}
      <!-- wait for first-run check -->
    {:else if isFirstRun}
      <Lazy loader={loadOnboarding} first_run={true} onComplete={() => { isFirstRun = false; }} />
    {:else if traceParams}
      <Lazy loader={loadTraceRoute} id={traceParams.id} />
    {:else if path() === '/' || path() === ''}
      <Live />
    {:else if path() === '/incidents'}
      <Lazy loader={loadIncidents} />
    {:else if path() === '/devices/new'}
      <Lazy loader={loadOnboarding} />
    {:else if deviceParams}
      <Lazy loader={loadDevices} selectedAddress={deviceParams.address} />
    {:else if path() === '/devices'}
      <Lazy loader={loadDevices} />
    {:else if path() === '/collectors'}
      <Lazy loader={loadCollectors} />
    {:else if path() === '/routing'}
      <Lazy loader={loadRouting} />
    {:else if path() === '/environments'}
      <Lazy loader={loadEnvironments} />
    {:else if path() === '/profiles'}
      <Lazy loader={loadProfiles} />
    {:else if path() === '/sites'}
      <Lazy loader={loadSites} />
    {:else if path() === '/credentials'}
      <Lazy loader={loadCredentials} />
    {:else if path() === '/operations'}
      <Lazy loader={loadOperations} />
    {:else if path() === '/integrations'}
      <Lazy loader={loadIntegrations} />
    {:else if path() === '/approvals'}
      <Lazy loader={loadApprovals} />
    {:else if path() === '/explorer'}
      <Lazy loader={loadExplorer} />
    {:else if path() === '/investigations'}
      <Lazy loader={loadInvestigations} />
    {:else if path() === '/ha'}
      <Lazy loader={loadHA} />
    {:else if path() === '/audit'}
      <Lazy loader={loadAudit} />
    {:else if path() === '/syslog'}
      <Lazy loader={loadSyslog} />
    {:else if path() === '/db'}
      <Lazy loader={loadDbManagement} />
    {:else if path() === '/governance'}
      <Lazy loader={loadGovernance} />
    {:else if path() === '/snmp'}
      <Lazy loader={loadSnmp} />
    {:else if path() === '/sidecars'}
      <Lazy loader={loadSidecar} />
    {:else if path() === '/users'}
      <Lazy loader={loadUsers} />
    {:else if path() === '/config-library'}
      <Lazy loader={loadSynthesizerRules} />
    {:else if path() === '/endpoints'}
      <Lazy loader={loadEndpoints} />
    {:else if path() === '/certs'}
      <Lazy loader={loadCerts} />
    {:else if path() === '/stream-control'}
      <Lazy loader={loadStreamControl} />
    {:else if path() === '/settings'}
      <Lazy loader={loadSettings} />
    {:else}
      <div class="empty">Page not found.</div>
    {/if}
  </main>
</div>

<CommandPalette />

<div class="toast-container" aria-live="polite">
  {#each getToasts() as t (t.id)}
    <div class="toast toast-{t.kind}" role="alert">
      <span class="toast-msg">{t.message}</span>
      <button class="toast-close" onclick={() => dismissToast(t.id)} aria-label="Dismiss">×</button>
    </div>
  {/each}
</div>

<style>
  .ml-console-link {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 6px;
    padding: 5px 8px;
    border-radius: 4px;
    font-size: 12px;
    color: var(--text-secondary, #9ca3af);
    text-decoration: none;
    transition: background 0.15s, color 0.15s;
  }
  .ml-console-link:hover {
    background: var(--bg-hover, rgba(255,255,255,0.07));
    color: var(--text-primary, #e5e7eb);
  }
  .density-row {
    margin-top: 8px;
    display: flex;
    justify-content: center;
  }
  .version-badge {
    margin-top: 10px;
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 10px;
    opacity: 0.65;
    padding: 0 6px;
    white-space: nowrap;
    overflow: hidden;
  }
  .version-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .version-dot--ok       { background: #4caf50; }
  .version-dot--degraded { background: #f59e0b; }
  .version-dot--unknown  { background: #6b7280; }
  .version-text {
    font-family: monospace;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
