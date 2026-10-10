#!/usr/bin/env node
// Claude's status line, moved into the CSR pane while one is open beside this session (herdr).
// Usage, as settings.json's statusLine command: node csr-pane-statusline.js <the real command...>
// It runs the real command with the same input (asking for its compact line, STATUSLINE_COMPACT=1,
// when the pane will draw it). While a CSR pane's pid file names this session and
// that pid is alive, the line is kept for the pane to draw and nothing is drawn under the prompt;
// otherwise the line passes through untouched.
const { spawnSync } = require('child_process')
const fs = require('fs')
const path = require('path')

const DATA = path.join(process.env.HOME || '', '.claude-self-reflect')
const KEEPS = ['csr-shifts', 'csr-companion'].map(name => path.join(DATA, name)) // the dev mod, the plugin

function paneKeep(session) {
  for (const keep of KEEPS) {
    try {
      const { pid } = JSON.parse(fs.readFileSync(path.join(keep, 'panes', `${session}.json`), 'utf8'))
      process.kill(Number(pid), 0)
      return keep
    } catch {}
  }
  return null
}

let input = ''
process.stdin.on('data', chunk => (input += chunk)).on('end', () => {
  let session = ''
  try {
    session = String(JSON.parse(input).session_id || '')
  } catch {}
  const keep = /^[\w-]+$/.test(session) ? paneKeep(session) : null
  // For the pane's narrow column the command is asked for its compact line (STATUSLINE_COMPACT=1).
  const env = keep ? { ...process.env, STATUSLINE_COMPACT: '1' } : process.env
  const [command, ...args] = process.argv.slice(2)
  const ran = command ? spawnSync(command, args, { input, encoding: 'utf8', timeout: 5000, env }) : null
  const line = (ran && ran.stdout) || ''
  if (keep) {
    try {
      const dir = path.join(keep, 'statusline')
      fs.mkdirSync(dir, { recursive: true })
      const file = path.join(dir, `${session}.json`)
      fs.writeFileSync(`${file}.tmp`, JSON.stringify({ at: Date.now() / 1000, line }))
      fs.renameSync(`${file}.tmp`, file)
      return // drawn in the pane
    } catch {}
  }
  process.stdout.write(line)
})
