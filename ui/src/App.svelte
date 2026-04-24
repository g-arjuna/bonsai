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

  const NAV = [
    { href: '/',              label: 'Live',         icon: '◉' },
    { href: '/incidents',     label: 'Incidents',    icon: '⚠' },
    { href: '/devices',       label: 'Devices',      icon: '⊡' },
    { href: '/collectors',    label: 'Collectors',   icon: '⇄' },
    { href: '/environments',  label: 'Environments', icon: '⬡' },
    { href: '/profiles',      label: 'Profiles',     icon: '📋' },
    { href: '/sites',         label: 'Sites',        icon: '◎' },
    { href: '/credentials',   label: 'Credentials',  icon: '⚿' },
    { href: '/operations',    label: 'Operations',   icon: '♡' },
  ];

  let setupChecked = $state(false);
  let showSetup    = $state(false);

  import { onMount } from 'svelte';
  onMount(async () => {
    try {
      const r = await fetch('/api/setup/status');
      if (r.ok) {
        const data = await r.json();
        if (data.is_first_run) {
          showSetup = true;
        }
      }
    } catch (_) {
      // non-fatal
    } finally {
      setupChecked = true;
    }
  });

  function isActive(href) {
    const p = path();
    return href === '/' ? (p === '/' || p === '') : (p === href || p.startsWith(href + '/'));
  }

  let traceParams = $derived(matchRoute('/trace/:id', path()));
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
        </a>
      {/each}
    </nav>
    <div class="sidebar-footer">
      <button class="palette-trigger" onclick={() => document.dispatchEvent(new KeyboardEvent('keydown', { ctrlKey: true, key: 'k', bubbles: true }))}>
        <span>⌨</span> Search <kbd>Ctrl+K</kbd>
      </button>
    </div>
  </aside>

  <main class="main-content">
    {#if !setupChecked}
      <!-- wait for first-run check before rendering anything -->
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
