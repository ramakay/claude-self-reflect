import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

const BASE = '/claude-self-reflect/'
const DOCS_BASE = `${BASE}docs/`

const MIME = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8', '.txt': 'text/plain; charset=utf-8',
  '.xml': 'application/xml; charset=utf-8', '.svg': 'image/svg+xml', '.png': 'image/png',
  '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.webp': 'image/webp', '.gif': 'image/gif',
  '.avif': 'image/avif', '.ico': 'image/x-icon', '.mp4': 'video/mp4', '.webm': 'video/webm',
  '.vtt': 'text/vtt; charset=utf-8', '.mp3': 'audio/mpeg', '.woff': 'font/woff',
  '.woff2': 'font/woff2', '.ttf': 'font/ttf', '.pdf': 'application/pdf', '.wasm': 'application/wasm',
}

// Resolve rel inside dir; null when it escapes the dir or is not a file.
function fileIn(dir, rel) {
  const file = path.resolve(dir, rel)
  if (file !== dir && !file.startsWith(dir + path.sep)) return null
  try { return fs.statSync(file).isFile() ? file : null } catch { return null }
}

function send(req, res, file, status = 200) {
  const { size } = fs.statSync(file)
  const headers = {
    'Content-Type': MIME[path.extname(file).toLowerCase()] || 'application/octet-stream',
    'Accept-Ranges': 'bytes',
    'Cache-Control': 'no-cache',
  }
  let start = 0, end = size - 1
  const range = status === 200 && /^bytes=(\d*)-(\d*)$/.exec(req.headers.range || '')
  if (range) {
    if (range[1] === '') { start = Math.max(0, size - Number(range[2])) } // suffix: last N bytes
    else { start = Number(range[1]); if (range[2] !== '') end = Math.min(Number(range[2]), size - 1) }
    if (start > end || start >= size) {
      res.writeHead(416, { 'Content-Range': `bytes */${size}` })
      return res.end()
    }
    status = 206
    headers['Content-Range'] = `bytes ${start}-${end}/${size}`
  }
  headers['Content-Length'] = end - start + 1
  res.writeHead(status, headers)
  if (req.method === 'HEAD') return res.end()
  fs.createReadStream(file, { start, end }).pipe(res)
}

// Dev only: serve the marketing landing (repo site/) at /claude-self-reflect/ the way
// Pages does (.github/workflows/docs.yml), so the root shows the film landing while
// /claude-self-reflect/docs/ stays the hot-reloading docs app. Not part of `vite build`.
function landingDevServer() {
  const siteDir = fileURLToPath(new URL('../site/', import.meta.url)).replace(/\/$/, '')
  const publicDir = fileURLToPath(new URL('./public/', import.meta.url)).replace(/\/$/, '')
  return {
    name: 'csr-landing-dev',
    apply: 'serve',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        if (req.method !== 'GET' && req.method !== 'HEAD') return next()
        let pathname
        try { pathname = decodeURIComponent(new URL(req.url, 'http://dev').pathname) } catch { return next() }
        // Pages adds the trailing slash to a directory; do the same so relative links resolve.
        for (const dir of [BASE, DOCS_BASE]) {
          if (pathname === dir.slice(0, -1)) { res.writeHead(301, { Location: dir }); return res.end() }
        }
        if (!pathname.startsWith(BASE) || pathname.startsWith(DOCS_BASE)) return next()
        let rel = pathname.slice(BASE.length)
        if (rel === '' || rel.endsWith('/')) rel += 'index.html'
        // The landing wins; then the docs' public files that docs.yml copies to the root.
        const file = fileIn(siteDir, rel) || (rel.split('/')[0] !== 'assets' && rel !== 'index.html' && fileIn(publicDir, rel))
        if (file) return send(req, res, file)
        const notFound = fileIn(siteDir, '404.html')
        return notFound ? send(req, res, notFound, 404) : next()
      })
    },
  }
}

export default defineConfig({
  plugins: [react(), tailwindcss(), landingDevServer()],
  // The anatomy landing (site/) owns the Pages root; this app lives under /docs/.
  base: DOCS_BASE,
  build: {
    outDir: 'dist',
  },
})
