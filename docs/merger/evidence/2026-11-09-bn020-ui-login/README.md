# BN-020 — UI login page + JWT session (9 November 2026)

Closes the gap found during the BN-013 credential-save investigation: the
SPA had no login page and no token handling, so every `/api/*` call from the
browser returned 401 under `require_auth`.

## Changes

- `ui/src/routes/Login.svelte` (new): username/password form →
  `POST /api/auth/login`; shows server error on failure; on success calls
  `onLogin`.
- `ui/src/App.svelte`: auth gate — on mount, probes `GET
  /api/setup/status`; 401/403 → renders `Login` instead of the app shell.
  After login, re-runs the first-run check and renders normally. Session is
  the HttpOnly `bonsai_token` JWT cookie set by the server (SameSite=Lax) —
  no token is stored in JS.
- Test user created: `admin` (role `admin`) via `POST /api/auth/users`.
  Literal `admin`/`admin` is rejected by the password policy (≥16 chars,
  mixed case, digit, special); lab password is `Bonsai2026!Admin`.
  Note: the env bootstrap admin in `.env` is also username `admin`, so
  both paths resolve to the same login name.

## Verified live (packaged `bonsai:latest`, rebuilt 482 s)

- `GET /api/setup/status` unauthenticated → **401** (`unauth-status.txt`).
- `POST /api/auth/login` admin → **success, role=admin**, `bonsai_token`
  cookie set (`login-envadmin.json` captured the locked variant — see
  below; the successful response was verified in-session).
- Authenticated `GET /api/setup/status` with cookie → **200**
  (`authed-status.txt` shows the post-lockout 401 — see note).
- Wrong password → `success:false, "invalid credentials"`.
- New bundle `index-DmCc5FEd.js` served; `npm run build` clean (6.45 s,
  zero warnings).

## Logout + credential-save verification (same day, second build 491 s)

- Logout added: sidebar "Sign out" button → `POST /api/auth/logout`
  (revokes JWT jti + clears cookie) → UI returns to login screen.
- Post-lockout re-verification on rebuilt image:
  - `admin` login → `success:true, role=admin` (`login-admin-success.json`)
  - Cookie-session `POST /api/ai/providers` → `has_api_key:true`
    (`save-provider.json`) — the exact call the Settings GUI makes; test
    provider removed afterwards.
  - `POST /api/auth/logout` → 204; subsequent API call → **401**.
- Rate-limit lockout from earlier testing expired as designed (~30 min);
  `admin`/`Bonsai2026!Admin` is the working lab login.

## Honest notes

- **Rate limiting observed live**: after repeated login attempts during
  verification, `admin` hit the brute-force lockout (3 attempts/5 min →
  30 min lock). `login-admin-rate-limited.json` /
  `login-bad-rate-limited.json` show `"too many failed attempts — account
  temporarily locked"`. The env admin shares the username `admin`, so it
  was locked too. Lock expired ~30 min later; happy path re-verified above.
- Viewer/api_readonly roles will see the login page then hit 403s on
  write endpoints — RBAC enforcement itself is unchanged and tested
  elsewhere.
