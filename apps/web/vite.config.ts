/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { VitePWA } from "vite-plugin-pwa";

// Dev server proxies the gateway on https://gateway.local:8443. The
// gateway uses a mkcert TLS cert (locally trusted, but the dev
// machine's Node has no preinstalled trust store for it), so we let
// the proxy skip certificate verification — only on the dev origin.
export default defineConfig({
  plugins: [
    react(),
    // PWA shell: precache the built app shell so the SPA boots with no
    // network (the prerequisite for offline playback — IndexedDB blobs are
    // useless if the code that reads them can't load). Audio is deliberately
    // NOT handled here: /rest, /v1, /oauth are NetworkOnly, and cached audio
    // is served from IndexedDB blob: URLs that never touch the SW.
    VitePWA({
      // autoUpdate also kills the stale-bundle white-screen we hit on
      // deploys: a new shell takes over and reloads instead of serving a
      // cached index.html that points at a now-404 asset hash.
      registerType: "autoUpdate",
      // We call registerSW() ourselves in main.tsx — don't also inject a tag.
      injectRegister: false,
      includeAssets: ["icon.svg"],
      manifest: {
        name: "crates music",
        short_name: "crates",
        description: "Self-hosted music player for Navidrome.",
        theme_color: "#0e0e0e",
        background_color: "#0e0e0e",
        display: "standalone",
        start_url: "/",
        scope: "/",
        icons: [
          { src: "/icon.svg", sizes: "any", type: "image/svg+xml", purpose: "any" },
          { src: "/icon.svg", sizes: "any", type: "image/svg+xml", purpose: "maskable" },
        ],
      },
      workbox: {
        globPatterns: ["**/*.{js,css,html,svg,woff,woff2}"],
        // The 3D latent-space view is a ~900 KB lazy chunk used only on one
        // diagnostics page — keep it out of the offline precache (it loads
        // from network when online; that page just won't work offline).
        globIgnores: ["**/LatentSpace3D-*.js"],
        // Deep links / cold offline opens resolve to the SPA shell...
        navigateFallback: "/index.html",
        // ...except gateway-owned paths, which must hit the network. NB:
        // /oauth/callback is intentionally absent — it's the SPA's own route
        // and must fall through to index.html.
        navigateFallbackDenylist: [
          /^\/rest\//,
          /^\/v1\//,
          /^\/oauth\/(setup|login|authorize|token|revoke)\b/,
        ],
        // Never let the SW cache API, auth, or audio responses.
        runtimeCaching: [
          {
            urlPattern: ({ url }) =>
              url.pathname.startsWith("/rest") ||
              url.pathname.startsWith("/v1") ||
              url.pathname.startsWith("/oauth"),
            handler: "NetworkOnly",
          },
        ],
      },
      // Keep the SW out of `vite dev` — test it via `build` + `preview`.
      devOptions: { enabled: false },
    }),
  ],
  server: {
    port: 5173,
    proxy: {
      "/rest": { target: "https://gateway.local:8443", secure: false, changeOrigin: true },
      // ws: true forwards WebSocket upgrades — needed for /v1/sync.
      "/v1": { target: "https://gateway.local:8443", secure: false, changeOrigin: true, ws: true },
      // Allowlist gateway OAuth endpoints. /oauth/callback is the SPA's
      // own route — it must NOT be proxied to the gateway.
      "^/oauth/(setup|login|authorize|token|revoke)(\\b|/)": {
        target: "https://gateway.local:8443",
        secure: false,
        changeOrigin: true,
      },
    },
  },
  // Vitest config — `node` env keeps pure-function tests fast (no jsdom
  // boot). Component tests, when they land, will opt into jsdom per-file
  // via `// @vitest-environment jsdom`.
  test: {
    environment: "node",
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
