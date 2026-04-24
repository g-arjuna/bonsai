<script module>
  // Module-level toast store — shared across the whole app.
  import { SvelteMap } from 'svelte/reactivity';
  let toasts = $state([]);
  let nextId = 0;

  export function toast(message, kind = 'info', durationMs = 4000) {
    const id = ++nextId;
    toasts = [...toasts, { id, message, kind }];
    if (durationMs > 0) {
      setTimeout(() => dismiss(id), durationMs);
    }
    return id;
  }

  export function dismiss(id) {
    toasts = toasts.filter(t => t.id !== id);
  }
</script>

<script>
  import { dismiss } from './Toast.svelte';
  // toasts is imported from the module context automatically via $state
</script>

<div class="toast-container" aria-live="polite">
  {#each toasts as t (t.id)}
    <div class="toast toast-{t.kind}" role="alert">
      <span class="toast-msg">{t.message}</span>
      <button class="toast-close" onclick={() => dismiss(t.id)} aria-label="Dismiss">×</button>
    </div>
  {/each}
</div>
