# BN-019 UI accessibility/bundle warning closeout — 8 November 2026

Base commit `8017090` plus uncommitted working-tree changes (listed below).
Scope: the documented "UI accessibility/bundle warnings remain" release-hygiene
debt under BN-019. This is a build-hygiene slice only — it does not advance
install/upgrade, lab browser acceptance, or soak gates.

## What changed

### Svelte a11y warnings (all eliminated, none suppressed wholesale)

- `ui/src/lib/CommandPalette.svelte` — removed `autofocus`; the palette input is
  now focused programmatically on open (`tick()` + `inputEl.focus()`), which is
  the correct dialog-focus pattern. Verified: Ctrl+K focuses `.palette-input`.
- `ui/src/routes/Incidents.svelte`, `ui/src/lib/CommandPalette.svelte` — existing
  `svelte-ignore` comments used space-separated codes; in runes mode Svelte only
  honors comma-separated lists, so secondary codes were silently dropped. Fixed
  separators so the ignores cover what they declare.
- `ui/src/routes/Profiles.svelte`, `ui/src/routes/SynthesizerRules.svelte` —
  selectable `<li role="button">` rows converted to proper listbox semantics:
  `<ul role="listbox">` + `<li role="option" aria-selected>` with Enter/**Space**
  activation.
- `ui/src/routes/Profiles.svelte`, `ui/src/routes/HA.svelte`,
  `ui/src/routes/Settings.svelte`, `ui/src/routes/SynthesizerRules.svelte` —
  orphan `<label>` elements associated with their controls via `for`/`id`
  (per-item `recv-addr-{r.key}` inside the receivers `{#each}`).
- `ui/src/lib/Onboarding.svelte` + `ui/src/app.css` — the "Archetype" group
  label (which headed a radio group, not a control) changed to a
  `.form-row-label` span with matching styles.
- `ui/src/routes/Integrations.svelte` — "Severity mapping reference" `<label>`
  wrapped a non-control table; changed to `.form-note` div with the same
  `.form-grid` styling rules.
- `ui/src/routes/Explorer.svelte` — save-query modal: backdrop click now uses a
  `target === currentTarget` check (inner `.modal` no longer needs a
  stopPropagation handler), modal has `role="dialog" aria-modal="true"
  aria-labelledby`, backdrop has `role="presentation"` + Escape `onkeydown`, and
  a `<svelte:window>` Escape handler closes the modal from anywhere.
- `ui/src/routes/Certs.svelte` — "Apply to Service" disclosure header gained
  Enter/Space `onkeydown` and `aria-expanded`.
- `ui/src/routes/Syslog.svelte` — deprecated `<script context="module">`
  replaced with `<script module>`.

### Bundle splitting

- `ui/vite.config.js` — `build.rolldownOptions.output.codeSplitting` vendor
  group for `node_modules`.
- `ui/src/App.svelte` + new `ui/src/lib/Lazy.svelte` — all non-Live routes are
  now lazy-loaded through a cached `<Lazy loader={...}>` wrapper (module-level
  `Map` keyed by stable loader functions, so revisits do not refetch or flash).
  Props (`selectedAddress`, `id`, `first_run`, `onComplete`) forward via rest
  props. Live (default route, Topology/d3) stays eager.

## Results

`npm run build` (vite 8.0.14 / rolldown): **zero warnings**. Previously ~20
`vite-plugin-svelte` a11y/deprecation warnings plus the >500 kB chunk warning.

- Entry `index-*.js`: 637.51 kB → **97.72 kB** (gzip 27.69 kB)
- Vendor `vendor-*.js`: **118.08 kB**; largest route chunk `Onboarding` 62.38 kB
- Per-route JS/CSS chunks emitted for all 27 lazy routes.

Browser smoke (system Chrome via Playwright, `vite preview` of `dist/`):
12 hash routes (`/`, `/incidents`, `/devices`, `/devices/10.0.0.1`, `/routing`,
`/integrations`, `/explorer`, `/settings`, `/certs`, `/profiles`, `/syslog`,
`/ha`, `/config-library`, `/approvals`, `/operations`) each render their own
route content; no `pageerror`s. API 404/JSON errors are expected — preview has
no backend. Command palette focuses its input on Ctrl+K; Escape closes.

## Limits

- Smoke test used `vite preview` without the Bonsai backend; it verifies bundle
  loading and route mounting, not live data paths. Full browser journeys remain
  under BN-003/BN-019 lab gates.
- `role="option"` rows in SynthesizerRules contain a nested toggle `<button>`;
  valid ARIA would prefer separate controls, but behavior is unchanged and no
  warning remains.
- Incidents expandable `<article>` keeps its pre-existing `svelte-ignore`
  (now correctly comma-separated); it has tabindex + Enter/Space handling.

## Files

- `ui-build.log` — clean `npm run build` output (sha256
  `d2911acbf51e695857564900babb022aae7279a57d481f8e5466d166e24ef934`)
- `results.json` — commands and hashes
