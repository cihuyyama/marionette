import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  build: {
    target: "es2022",
    sourcemap: false,
    rollupOptions: {
      output: {
        manualChunks: {
          vendor: ["react", "react-dom", "react-router-dom"],
        },
      },
    },
  },
  server: {
    port: 1941,
    proxy: {
      "/admin": {
        target: "http://127.0.0.1:1940",
        changeOrigin: true,
      },
      "/v1": {
        target: "http://127.0.0.1:1940",
        changeOrigin: true,
      },
      "/health": {
        target: "http://127.0.0.1:1940",
        changeOrigin: true,
      },
    },
  },
});
