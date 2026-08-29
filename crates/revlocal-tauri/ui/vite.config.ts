import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// Built output goes to `dist`, which `tauri.conf.json` serves as `frontendDist`.
// Relative base: the app is loaded from a custom protocol, not from a web root.
export default defineConfig({
  plugins: [react()],
  base: './',
  build: { outDir: 'dist', emptyOutDir: true },
  // Tauri owns the window; a dev server is only for iterating on the UI alone.
  server: { port: 5173, strictPort: true },
});
