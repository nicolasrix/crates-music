import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Dev server proxies the gateway on https://gateway.local:8443. The
// gateway uses a mkcert TLS cert (locally trusted, but the dev
// machine's Node has no preinstalled trust store for it), so we let
// the proxy skip certificate verification — only on the dev origin.
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/rest": { target: "https://gateway.local:8443", secure: false, changeOrigin: true },
      "/v1": { target: "https://gateway.local:8443", secure: false, changeOrigin: true },
      // Allowlist gateway OAuth endpoints. /oauth/callback is the SPA's
      // own route — it must NOT be proxied to the gateway.
      "^/oauth/(setup|login|authorize|token|revoke)(\\b|/)": {
        target: "https://gateway.local:8443",
        secure: false,
        changeOrigin: true,
      },
    },
  },
});
