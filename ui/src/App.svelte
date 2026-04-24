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

  const NAV = [
    { href: '/',            label: 'Live',        icon: '◉' },
    { href: '/incidents',   label: 'Incidents',   icon: '⚠' },
    { href: '/devices',     label: 'Devices',     icon: '⊡' },
    { href: '/collectors',  label: 'Collectors',  icon: '⇄' },
    { href: '/sites',       label: 'Sites',       icon: '◎' },
    { href: '/credentials', label: 'Credentials', icon: '⚿' },
    { href: '/operations',  label: 'Operations',  icon: '♡' },
  ];

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
    {#if traceParams}
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
