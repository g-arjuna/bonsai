<script>
  import Trace from '$lib/Trace.svelte';
  import { navigate } from '$lib/router.svelte.js';

  let { id } = $props();
</script>

<div class="view">
  <div style="margin-bottom: 16px;">
    <button class="ghost" onclick={() => navigate('/')}>← Back to Live</button>
  </div>
  <Trace {id} />
</div>
