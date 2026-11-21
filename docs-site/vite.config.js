import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  // The anatomy landing (site/) owns the Pages root; this app lives under /docs/.
  base: '/claude-self-reflect/docs/',
  build: {
    outDir: 'dist',
  },
})
