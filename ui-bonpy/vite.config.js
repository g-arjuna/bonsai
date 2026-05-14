import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

// Bonpy is mounted at `/bonpy/` by the bonsai Axum server (see
// src/http_server.rs CV7 T4-5). All asset URLs must be relative to that
// base so the SPA loads correctly when served from a subpath.
export default defineConfig({
  base: '/bonpy/',
  plugins: [svelte()],
  server: {
    // During `npm run dev` proxy /api to the running bonsai HTTP server so
    // the bonpy SPA can call /api/sidecars without CORS workarounds.
    proxy: {
      '/api': 'http://localhost:3000',
      '/health': 'http://localhost:3000',
    },
  },
})
