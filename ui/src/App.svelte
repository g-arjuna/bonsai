<script>
  import { path, navigate, matchRoute } from '$lib/router.svelte.js';
  import { getToasts, dismissToast } from '$lib/toast.svelte.js';
  import Live from './routes/Live.svelte';
  import Incidents from './routes/Incidents.svelte';
  import Devices from './routes/Devices.svelte';
  import Collectors from './routes/Collectors.svelte';
  import Sites from './routes/Sites.svelte';
  import Credentials from './routes/Credentials.svelte';
  import Operations from './routes/Operations.svelte';
  import TraceRoute from './routes/TraceRoute.svelte';
  import Onboarding from '$lib/Onboarding.svelte';
  import CommandPalette from '$lib/CommandPalette.svelte';
  import Environments from './routes/Environments.svelte';
  import Profiles from './routes/Profiles.svelte';
  import Integrations from './routes/Integrations.svelte';
  import Approvals from './routes/Approvals.svelte';
  import Explorer from './routes/Explorer.svelte';
  import Investigations from './routes/Investigations.svelte';
  import Settings from './routes/Settings.svelte';
  import Audit from './routes/Audit.svelte';
  import DensityToggle from '$lib/components/DensityToggle.svelte';
  import HA from './routes/HA.svelte';
  import Syslog from './routes/Syslog.svelte';
  import DbManagement from './routes/DbManagement.svelte';
  import Governance from './routes/Governance.svelte';

  // Cmd-1..9 workspace shortcuts mirror CommandPalette WORKSPACE_SHORTCUTS
  const NAV = [
    { href: '/',              label: 'Live',          icon: '◉', kbd: '1' },
    { href: '/incidents',     label: 'Incidents',     icon: '⚠', kbd: '2' },
    { href: '/devices',       label: 'Devices',       icon: '⊡', kbd: '3' },
    { href: '/operations',    label: 'Operations',    icon: '♡', kbd: '4' },
    { href: '/collectors',    label: 'Collectors',    icon: '⇄', kbd: '5' },
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
    { href: '/settings',      label: 'Settings',      icon: '⚙' },
  ];

  let isFirstRunChecked = $state(false);
  let isFirstRun        = $state(false);
  let healthInfo        = $state(null);

  import { onMount } from 'svelte';
  onMount(async () => {
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
        {#each NAV.slice(0, 5) as item}
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
        {#each NAV.slice(5, 9) as item}
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
        {#each NAV.slice(9) as item}
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
    {#if !isFirstRunChecked}
      <!-- wait for first-run check -->
    {:else if isFirstRun}
      <Onboarding first_run={true} onComplete={() => { isFirstRun = false; }} />
    {:else if traceParams}
      <TraceRoute id={traceParams.id} />
    {:else if path() === '/' || path() === ''}
      <Live />
    {:else if path() === '/incidents'}
      <Incidents />
    {:else if path() === '/devices/new'}
      <Onboarding />
    {:else if deviceParams}
      <Devices selectedAddress={deviceParams.address} />
    {:else if path() === '/devices'}
      <Devices />
    {:else if path() === '/collectors'}
      <Collectors />
    {:else if path() === '/environments'}
      <Environments />
    {:else if path() === '/profiles'}
      <Profiles />
    {:else if path() === '/sites'}
      <Sites />
    {:else if path() === '/credentials'}
      <Credentials />
    {:else if path() === '/operations'}
      <Operations />
    {:else if path() === '/integrations'}
      <Integrations />
    {:else if path() === '/approvals'}
      <Approvals />
    {:else if path() === '/explorer'}
      <Explorer />
    {:else if path() === '/investigations'}
      <Investigations />
    {:else if path() === '/ha'}
      <HA />
    {:else if path() === '/audit'}
      <Audit />
    {:else if path() === '/syslog'}
      <Syslog />
    {:else if path() === '/db'}
      <DbManagement />
    {:else if path() === '/governance'}
      <Governance />
    {:else if path() === '/settings'}
      <Settings />
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
