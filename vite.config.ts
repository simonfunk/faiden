import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// A globally exported NODE_ENV=production makes React resolve its production
// build inside Vitest, where `act` does not exist. Pin it for test runs so the
// suite does not depend on the developer's ambient environment.
if (process.env['VITEST']) {
  process.env['NODE_ENV'] = 'test'
}

// Tauri serves the frontend from a fixed dev port and expects a static dist/.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5183,
    strictPort: true,
  },
  build: {
    target: 'safari15',
    sourcemap: false,
  },
  test: {
    globals: true,
    environment: 'jsdom',
    setupFiles: ['./vitest.setup.ts'],
    include: ['src/**/*.test.{ts,tsx}'],
  },
})
