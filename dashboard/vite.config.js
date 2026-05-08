import { defineConfig, loadEnv } from 'vite'
import react from '@vitejs/plugin-react'
import fs from 'node:fs'
import path from 'node:path'

function readTomlListenAddr() {
  const tomlPath = path.resolve(__dirname, '../turbineproxy.toml')
  const fallbackPath = path.resolve(__dirname, '../turbineproxy.example.toml')
  const file = fs.existsSync(tomlPath) ? tomlPath : fs.existsSync(fallbackPath) ? fallbackPath : null
  if (!file) return null
  const content = fs.readFileSync(file, 'utf-8')
  // Find [dashboard] section and extract listen_addr
  const dashboardSection = content.match(/\[dashboard\]([\s\S]*?)(?=\n\[|$)/)
  if (!dashboardSection) return null
  const addrMatch = dashboardSection[1].match(/listen_addr\s*=\s*"([^"]+)"/)
  if (!addrMatch) return null
  // Convert 0.0.0.0:PORT → localhost:PORT
  return addrMatch[1].replace(/^0\.0\.0\.0/, 'localhost')
}

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), '')
  const rawPort = env.FRONTEND_PORT || env.VITE_FRONTEND_PORT || env.VITE_PORT || '5173'
  const devPort = Number.parseInt(rawPort, 10)
  const tomlAddr = readTomlListenAddr()
  const apiOrigin = env.VITE_API_ORIGIN || (tomlAddr ? `http://${tomlAddr}` : 'http://localhost:8080')

  return {
    plugins: [react()],
    server: {
      host: true,
      port: Number.isFinite(devPort) ? devPort : 5173,
      proxy: {
        '/api': apiOrigin,
        '/health': apiOrigin,
      },
    },
    build: {
      outDir: 'dist',
    },
    test: {
      environment: 'jsdom',
      globals: true,
      setupFiles: ['./src/test/setup.js'],
      coverage: {
        provider: 'v8',
        reporter: ['text', 'lcov'],
        include: ['src/**/*.{js,jsx}'],
        exclude: ['src/main.jsx', 'src/test/**'],
      },
    },
  }
})
