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
  import Setup from './routes/Setup.svelte';
  import Profiles from './routes/Profiles.svelte';
  import Enrichment from './routes/Enrichment.svelte';
  import Approvals from './routes/Approvals.svelte';
  import Adapters from './routes/Adapters.svelte';
  import Explorer from './routes/Explorer.svelte';
  import Investigations from './routes/Investigations.svelte';
  import DensityToggle from '$lib/components/DensityToggle.svelte';

  // Cmd-1..9 workspace shortcuts mirror CommandPalette WORKSPACE_SHORTCUTS
  const NAV = [
    { href: '/',              label: 'Live',          icon: '◉', kbd: '1' },
    { href: '/incidents',     label: 'Incidents',     icon: '⚠', kbd: '2' },
    { href: '/devices',       label: 'Devices',       icon: '⊡', kbd: '3' },
    { href: '/operations',    label: 'Operations',    icon: '♡', kbd: '4' },
    { href: '/collectors',    label: 'Collectors',    icon: '⇄', kbd: '5' },
    { href: '/enrichment',    label: 'Enrichment',    icon: '⟳', kbd: '6' },
    { href: '/adapters',      label: 'Adapters',      icon: '⇥', kbd: '7' },
    { href: '/approvals',     label: 'Approvals',     icon: '✓', kbd: '8' },
    { href: '/explorer',      label: 'Explorer',      icon: '⬡', kbd: '9' },
    { href: '/environments',  label: 'Environments',  icon: '⬡' },
    { href: '/profiles',      label: 'Profiles',      icon: '📋' },
    { href: '/sites',         label: 'Sites',         icon: '◎' },
    { href: '/credentials',   label: 'Credentials',   icon: '⚿' },
    { href: '/investigations',label: 'Investigations',icon: '🔍' },
  ];

  let setupChecked = $state(false);
  let showSetup    = $state(false);
  let healthInfo   = $state(null);

  import { onMount } from 'svelte';
  onMount(async () => {
    try {
      const r = await fetch('/api/setup/status');
      if (r.ok) {
        const data = await r.json();
        if (data.is_first_run) showSetup = true;
      }
    } catch (_) {
      // non-fatal
    } finally {
      setupChecked = true;
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

<div class="app-shell">
  <aside class="sidebar">
    <div class="sidebar-brand">bonsai</div>
    <nav>
      {#each NAV as item}
        <a href={'#' + item.href}
           class:active={isActive(item.href)}
           onclick={(e) => { e.preventDefault(); navigate(item.href); }}>
          <span class="nav-icon">{item.icon}</span>
          {item.label}
          {#if item.kbd}
            <kbd class="nav-kbd">⌘{item.kbd}</kbd>
          {/if}
        </a>
      {/each}
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

  <main class="main-content">
    {#if !setupChecked}
      <!-- wait for first-run check -->
    {:else if showSetup && path() !== '/setup'}
      <Setup onComplete={() => { showSetup = false; }} />
    {:else if traceParams}
      <TraceRoute id={traceParams.id} />
    {:else if path() === '/setup'}
      <Setup onComplete={() => { showSetup = false; }} />
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
    {:else if path() === '/enrichment'}
      <Enrichment />
    {:else if path() === '/adapters'}
      <Adapters />
    {:else if path() === '/approvals'}
      <Approvals />
    {:else if path() === '/explorer'}
      <Explorer />
    {:else if path() === '/investigations'}
      <Investigations />
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
