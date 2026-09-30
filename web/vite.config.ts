import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The dev server proxies API and health calls to the Rust backend so the SPA
// runs same-origin in development, matching production (Axum serves web/dist).
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      // Keep the browser's Host/Origin pair when proxying. The backend rejects
      // missing or cross-origin state-changing requests; preserving both headers
      // makes local dev match the same-origin production shape.
      "/api": {
        target: "http://localhost:8080",
      },
      "/health": "http://localhost:8080",
    },
  },
  build: {
    outDir: "dist",
    // The antd chunk alone is ~670 kB minified (~210 kB gzip); it is loaded once
    // and cached, so warn only above that instead of on every build.
    chunkSizeWarningLimit: 700,
    rolldownOptions: {
      output: {
        // Split third-party code into its own chunks: they change far less often
        // than the app, so browsers keep them cached across releases.
        codeSplitting: {
          groups: [
            {
              name: "react",
              test: /node_modules[\\/](react|react-dom|react-router|react-router-dom|scheduler)[\\/]/,
              priority: 3,
            },
            {
              name: "antd",
              test: /node_modules[\\/](antd|@ant-design|@rc-component|rc-[^\\/]+)[\\/]/,
              priority: 2,
            },
            { name: "vendor", test: /node_modules[\\/]/, priority: 1 },
          ],
        },
      },
    },
  },
});
