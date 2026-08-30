import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// Built output goes to `dist`, which `tauri.conf.json` serves as `frontendDist`.
// Relative base: the app is loaded from a custom protocol, not from a web root.
export default defineConfig({
  // jsdom, because these tests render components. The Rust side is where the
  // logic lives; what is checked here is that the screen renders what it was
  // handed — in particular that a lower-bound figure never renders as a total.
  test: {
    environment: 'jsdom',
    globals: true,
  },
  plugins: [react()],
  base: './',
  build: { outDir: 'dist', emptyOutDir: true },
  // Tauri owns the window; a dev server is only for iterating on the UI alone.
  server: { port: 5173, strictPort: true },
});
