<script>
  let { onLogin } = $props();

  let username = $state('');
  let password = $state('');
  let error    = $state('');
  let busy     = $state(false);

  async function submit() {
    if (busy || !username.trim() || !password) return;
    busy = true;
    error = '';
    try {
      const r = await fetch('/api/auth/login', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username: username.trim(), password }),
      });
      const data = await r.json().catch(() => ({}));
      if (data.success) {
        onLogin?.(data);
      } else {
        error = data.error || `Login failed (${r.status})`;
      }
    } catch (e) {
      error = `Login failed: ${e.message}`;
    } finally {
      busy = false;
    }
  }
</script>

<div class="login-page">
  <form class="login-card" onsubmit={(e) => { e.preventDefault(); submit(); }}>
    <div class="login-brand">bonsai</div>
    <p class="login-sub">Sign in to continue</p>

    <label class="login-field">
      <span>Username</span>
      <input type="text" bind:value={username} autocomplete="username" required />
    </label>
    <label class="login-field">
      <span>Password</span>
      <input type="password" bind:value={password} autocomplete="current-password" required />
    </label>

    {#if error}
      <p class="login-error" role="alert">{error}</p>
    {/if}

    <button class="btn-primary login-btn" type="submit" disabled={busy || !username.trim() || !password}>
      {busy ? 'Signing in…' : 'Sign in'}
    </button>
  </form>
</div>

<style>
  .login-page {
    min-height: 100vh;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--bg-primary, #0f1115);
    padding: 1rem;
  }
  .login-card {
    width: 100%;
    max-width: 22rem;
    background: var(--bg-secondary, #161a22);
    border: 1px solid var(--border-color, #2a3040);
    border-radius: 10px;
    padding: 2rem;
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }
  .login-brand {
    font-size: 1.6rem;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-align: center;
  }
  .login-sub {
    text-align: center;
    color: var(--text-secondary, #9ca3af);
    margin: 0 0 0.5rem;
    font-size: 0.9rem;
  }
  .login-field {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    font-size: 0.85rem;
    color: var(--text-secondary, #9ca3af);
  }
  .login-field input {
    padding: 0.55rem 0.7rem;
    border-radius: 6px;
    border: 1px solid var(--border-color, #2a3040);
    background: var(--bg-primary, #0f1115);
    color: inherit;
    font-size: 0.95rem;
  }
  .login-error {
    color: #f87171;
    font-size: 0.85rem;
    margin: 0;
  }
  .login-btn {
    margin-top: 0.25rem;
  }
  .btn-primary {
    padding: 0.6rem 1rem;
    border: none;
    border-radius: 6px;
    background: #3b82f6;
    color: #fff;
    font-size: 0.95rem;
    cursor: pointer;
  }
  .btn-primary:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
