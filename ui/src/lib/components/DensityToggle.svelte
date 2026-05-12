<!--
  DensityToggle — Comfortable / Compact display density.
  Sets data-density="compact" on <html>; tokens.css responds to change --row-height, --card-pad, etc.
  Persists preference to localStorage.
-->
<script>
  import { onMount } from 'svelte';

  let density = $state('comfortable');

  onMount(() => {
    const saved = localStorage.getItem('bonsai-density') ?? 'comfortable';
    density = saved;
    applyDensity(saved);
  });

  function applyDensity(value) {
    if (value === 'compact') {
      document.documentElement.setAttribute('data-density', 'compact');
    } else {
      document.documentElement.removeAttribute('data-density');
    }
  }

  function set(value) {
    density = value;
    applyDensity(value);
    localStorage.setItem('bonsai-density', value);
  }
</script>

<div class="density-toggle" role="group" aria-label="Display density">
  <button
    class:active={density === 'comfortable'}
    onclick={() => set('comfortable')}
    title="Comfortable density"
  >
    Comfortable
  </button>
  <button
    class:active={density === 'compact'}
    onclick={() => set('compact')}
    title="Compact density"
  >
    Compact
  </button>
</div>

<style>
  .density-toggle {
    display: flex;
    gap: 2px;
    background: var(--bg-elevated);
    border: 1px solid var(--border-subtle);
    border-radius: 5px;
    padding: 2px;
  }

  button {
    background: transparent;
    border: none;
    border-radius: 3px;
    color: var(--text-secondary);
    font-size: 11px;
    font-weight: 500;
    padding: 3px 8px;
    cursor: pointer;
    transition:
      background var(--duration-instant) var(--ease-out),
      color    var(--duration-instant) var(--ease-out);
  }

  button.active {
    background: var(--bg-surface);
    color: var(--text-primary);
  }

  button:hover:not(.active) {
    color: var(--text-primary);
  }
</style>
