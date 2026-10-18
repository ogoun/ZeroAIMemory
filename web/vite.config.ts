import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Относительные пути ассетов: работает под любым base_path сервера.
export default defineConfig({
  base: './',
  plugins: [svelte()],
  build: { outDir: 'dist', emptyOutDir: true },
});
