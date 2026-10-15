import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// `npm run dev` proxies the API to a running `wrapper gui` (default port 12340).
const backend = process.env.WRAPPER_GUI_URL || 'http://127.0.0.1:12340';
const paths = ['/api', '/status', '/health', '/m3u8', '/key', '/lyrics', '/webplayback', '/license'];

export default defineConfig({
  plugins: [svelte()],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    assetsDir: 'assets',
    chunkSizeWarningLimit: 600,
  },
  server: {
    proxy: Object.fromEntries(paths.map((p) => [p, { target: backend }])),
  },
});
