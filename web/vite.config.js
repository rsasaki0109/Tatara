import { defineConfig } from 'vite'

export default defineConfig({
  // TATARA_BASE=./ builds a relocatable static site (GitHub Pages).
  base: process.env.TATARA_BASE || '/',
  server: { proxy: { '/api': 'http://127.0.0.1:3000' } },
  build: { chunkSizeWarningLimit: 1500, outDir: process.env.TATARA_OUT_DIR || 'dist' },
})
