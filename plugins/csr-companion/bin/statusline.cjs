#!/usr/bin/env node
// Claude's status line, moved into the CSR pane while one is open beside this session (herdr).
// "/csr statusline on" sets it as settings.json's statusLine command, run from a stable copy at
// ~/.claude-self-reflect/csr-companion/statusline.cjs, never from the plugin's own folder (that
// path changes on every update, and the line must keep working after an uninstall):
//   node statusline.cjs --sh '<the real command>'   one shell string, run through a shell
//   node statusline.cjs <the real command...>        argv, no shell (the form first set up by hand)
// It runs the real command with the same input (asking for its compact line, STATUSLINE_COMPACT=1,
// when the pane will draw it). While a CSR pane's pid file names this session and
// that pid is alive, the line is kept for the pane to draw and nothing is drawn under the prompt;
// otherwise the line passes through untouched. Beside the line it keeps the few raw fields the
// pane's masthead draws (the prompt cache clock, the plan's quotas, the context numbers).
// .cjs: CommonJS whatever "type" the nearest package.json declares.
const { spawnSync } = require('child_process')
const fs = require('fs')
const path = require('path')

const DATA = path.join(process.env.HOME || '', '.claude-self-reflect')
const KEEPS = ['csr-shifts', 'csr-companion'].map(name => path.join(DATA, name)) // the dev mod, the plugin

// The pane touches its pid file every couple of seconds; one untouched this long names a pane that is gone,
// whatever process now holds its old pid.
const HEARTBEAT_S = 30

function paneKeep(session) {
  for (const keep of KEEPS) {
    try {
      const file = path.join(keep, 'panes', `${session}.json`)
      if (Date.now() - fs.statSync(file).mtimeMs > HEARTBEAT_S * 1000) continue
      const { pid } = JSON.parse(fs.readFileSync(file, 'utf8'))
      process.kill(Number(pid), 0)
      return keep
    } catch {}
  }
  return null
}

// The status JSON's fields the pane's masthead draws, kept as they came; absent ones stay absent.
function rawFields(data) {
  const fields = {}
  for (const name of ['prompt_cache', 'rate_limits', 'context_window']) {
    if (data && data[name] && typeof data[name] === 'object') fields[name] = data[name]
  }
  return fields
}

const chunks = []
process.stdin.on('data', chunk => chunks.push(chunk)).on('end', () => {
  const input = Buffer.concat(chunks).toString('utf8') // decoded whole: a character split across chunks stays one
  let session = ''
  let fields = {}
  try {
    const data = JSON.parse(input)
    session = String(data.session_id || '')
    fields = rawFields(data)
  } catch {}
  const keep = /^[\w-]+$/.test(session) ? paneKeep(session) : null
  // For the pane's narrow column the command is asked for its compact line (STATUSLINE_COMPACT=1).
  const env = keep ? { ...process.env, STATUSLINE_COMPACT: '1' } : process.env
  const [command, ...args] = process.argv.slice(2)
  let ran = null
  if (command === '--sh') {
    // One shell string, run through a shell as Claude Code runs a statusLine command.
    const shell = args.join(' ')
    if (shell) ran = spawnSync(shell, { shell: true, input, encoding: 'utf8', timeout: 5000, env })
  } else if (command) {
    ran = spawnSync(command, args, { input, encoding: 'utf8', timeout: 5000, env })
  }
  const line = (ran && ran.stdout) || ''
  if (keep) {
    try {
      const dir = path.join(keep, 'statusline')
      fs.mkdirSync(dir, { recursive: true })
      const file = path.join(dir, `${session}.json`)
      fs.writeFileSync(`${file}.tmp`, JSON.stringify({ at: Date.now() / 1000, line, fields }))
      fs.renameSync(`${file}.tmp`, file)
      return // drawn in the pane
    } catch {}
  }
  process.stdout.write(line)
})
