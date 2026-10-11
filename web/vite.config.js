import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// `npm run dev` proxies the API to a running `wrapper gui` (default port 12341).
const backend = process.env.WRAPPER_GUI_URL || 'http://127.0.0.1:12341';

export default defineConfig({
  plugins: [svelte()],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    assetsDir: 'assets',
    chunkSizeWarningLimit: 600,
  },
  server: {
    proxy: { '/api': { target: backend, changeOrigin: true } },
  },
});
