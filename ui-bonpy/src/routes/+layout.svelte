<script>
  import { onMount, onDestroy } from 'svelte';
  import { page } from '$app/stores';
  import { base } from '$app/paths';
  import { connect, disconnect, sseConnected } from '$lib/sse.js';
  import { QueryClient, QueryClientProvider } from '@tanstack/svelte-query';

  const queryClient = new QueryClient({ defaultOptions: { queries: { staleTime: 30000 } } });

  const NAV = [
    { href: '',           label: 'Dashboard',   icon: '⬡' },
    { href: '/jobs',      label: 'Jobs',         icon: '⚙' },
    { href: '/models',    label: 'Models',       icon: '◈' },
    { href: '/exports',   label: 'Exports',      icon: '⬡' },
    { href: '/gnn',       label: 'GNN',          icon: '◉' },
    { href: '/embeddings',label: 'Embeddings',   icon: '◎' },
    { href: '/detections',label: 'Detections',   icon: '⚑' },
  ];

  onMount(connect);
  onDestroy(disconnect);

  $: currentPath = $page.url.pathname;
</script>

<QueryClientProvider client={queryClient}>
  <div class="bonpy-shell">
    <nav class="sidebar">
      <div class="brand">
        <span class="brand-icon">◈</span>
        <span class="brand-text">bonpy</span>
      </div>
      <ul class="nav-list">
        {#each NAV as item}
          {@const href = base + item.href}
          {@const active = currentPath === href || (item.href !== '' && currentPath.startsWith(href))}
          <li class="nav-item {active ? 'active' : ''}">
            <a {href}><span class="nav-icon">{item.icon}</span><span class="nav-label">{item.label}</span></a>
          </li>
        {/each}
      </ul>
      <div class="sidebar-footer">
        <a href="/" class="back-link">← Network View</a>
        <div class="sse-dot {$sseConnected ? 'green' : 'red'}" title={$sseConnected ? 'Live' : 'Disconnected'}></div>
      </div>
    </nav>

    <main class="content">
      <slot />
    </main>
  </div>
</QueryClientProvider>

<style>
  :global(body) { margin: 0; font-family: 'Inter', sans-serif; background: var(--bg-base, #0d1117); color: var(--text-primary, #e6edf3); }
  .bonpy-shell { display: flex; min-height: 100vh; }
  .sidebar { width: 200px; min-width: 200px; background: var(--bg-surface, #161b22); border-right: 1px solid var(--border, #30363d); display: flex; flex-direction: column; padding: 0; }
  .brand { display: flex; align-items: center; gap: 8px; padding: 18px 16px; border-bottom: 1px solid var(--border, #30363d); font-weight: 700; font-size: 15px; color: var(--accent-primary, #4f8ef7); }
  .brand-icon { font-size: 18px; }
  .nav-list { list-style: none; margin: 0; padding: 8px 0; flex: 1; }
  .nav-item a { display: flex; align-items: center; gap: 10px; padding: 9px 16px; text-decoration: none; color: var(--text-secondary, #8b949e); font-size: 13px; border-left: 3px solid transparent; transition: all 0.15s; }
  .nav-item a:hover { color: var(--text-primary, #e6edf3); background: var(--bg-hover, #21262d); }
  .nav-item.active a { color: var(--accent-primary, #4f8ef7); border-left-color: var(--accent-primary, #4f8ef7); background: var(--bg-hover, #21262d); }
  .nav-icon { font-size: 14px; }
  .sidebar-footer { padding: 12px 16px; border-top: 1px solid var(--border, #30363d); display: flex; align-items: center; justify-content: space-between; }
  .back-link { font-size: 11px; color: var(--text-secondary, #8b949e); text-decoration: none; }
  .back-link:hover { color: var(--text-primary, #e6edf3); }
  .sse-dot { width: 8px; height: 8px; border-radius: 50%; }
  .sse-dot.green { background: #3fb950; box-shadow: 0 0 4px #3fb950; }
  .sse-dot.red { background: #f85149; }
  .content { flex: 1; padding: 24px; overflow: auto; }
</style>
