// Toast notification store (Svelte 5 runes).
// Import `toast` to trigger a notification; import `ToastList` component to render them.

let _toasts = $state([]);
let _nextId = 0;

export function getToasts() {
  return _toasts;
}

export function toast(message, kind = 'info', durationMs = 4000) {
  const id = ++_nextId;
  _toasts = [..._toasts, { id, message, kind }];
  if (durationMs > 0) {
    setTimeout(() => dismissToast(id), durationMs);
  }
  return id;
}

export function dismissToast(id) {
  _toasts = _toasts.filter(t => t.id !== id);
}
