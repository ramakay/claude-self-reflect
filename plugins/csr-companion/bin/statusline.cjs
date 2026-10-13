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

// Credentials out of what is kept (a status command may print a token): a port of bin/private.py's redact(), for
// this file runs alone from a stable copy. tests/redaction_table.json is the table both are tested against.
const REDACTED = '[redacted]'
const CRED = '[A-Za-z0-9._~+/=\\-]'
const WORD = '[A-Za-z0-9_.\\-]*?'
const NAMES = '(?:password|passwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|client[_-]?secret)'
const SECRETS = [
  [/-----BEGIN [A-Z0-9 ]+-----.*?(?:-----END [A-Z0-9 ]+-----|$)/gs, REDACTED],
  [/\bsk-[A-Za-z0-9_\-]{16,}/g, REDACTED],
  [/\bxai-[A-Za-z0-9]{16,}/g, REDACTED],
  [/\bgh[pousr]_[A-Za-z0-9]{20,}/g, REDACTED],
  [/\bgithub_pat_[A-Za-z0-9_]{20,}/g, REDACTED],
  [/\b(?:AKIA|ASIA)[A-Z0-9]{16}\b/g, REDACTED],
  [/\bxox[abposr]-[A-Za-z0-9\-]{10,}/g, REDACTED],
  [/\bAIza[0-9A-Za-z_\-]{35}/g, REDACTED],
  [/\b(?:hf|npm)_[A-Za-z0-9]{30,}/g, REDACTED],
  [/\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}/g, REDACTED],
  [new RegExp('\\b(authorization["\']?\\s*[:=]\\s*["\']?(?:bearer|basic|token)\\s+)' + CRED + '{8,}', 'gi'), '$1' + REDACTED],
  [new RegExp('\\b(bearer\\s+)(?=' + CRED + '*[0-9_~+/=])' + CRED + '{8,}', 'gi'), '$1' + REDACTED],
]
const KEYED = new RegExp('(\\b' + WORD + NAMES + '["\']?\\s*[=:]\\s*)(?!\\[redacted\\])("[^"\\n]*"|\'[^\'\\n]*\'|[^\\s,;&"\'`)\\]}]+)', 'gi')
const looksSecret = value => value.length >= 6 && /[0-9@#$%^*!~+=]/.test(value)

function redact(text) {
  if (typeof text !== 'string' || !text) return text
  for (const [pattern, replacement] of SECRETS) text = text.replace(pattern, replacement)
  return text.replace(KEYED, (_all, head, value) =>
    value[0] === '"' || value[0] === "'" ? head + value[0] + REDACTED + value[0] : head + (looksSecret(value) ? REDACTED : value))
}

function redactValue(value) {
  if (typeof value === 'string') return redact(value)
  if (Array.isArray(value)) return value.map(redactValue)
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, inner]) => [key, redactValue(inner)]))
  return value
}

// A folder or file of the companion's own, never a link: chmod and open would act on the link's target.
const isLink = file => { try { return fs.lstatSync(file).isSymbolicLink() } catch { return false } }

function main() {
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
  // What the wrapped command says on stderr, and why it did not start or ran out of time, reach stderr as they
  // would without this wrapper (PR #317 review: a failing line went blank and nothing said why). A line exits 0:
  // Claude Code shows the status line only after exit 0 (its docs, "Script errors or hangs"), so a command that
  // printed its line and then failed must not blank it (Codex review #17). With no line, its own status, or 1.
  if (ran && ran.stderr) process.stderr.write(ran.stderr)
  if (ran && ran.error) process.stderr.write(`statusline: ${ran.error.message}\n`)
  process.exitCode = !ran || line ? 0 : typeof ran.status === 'number' ? ran.status : 1
  if (keep) {
    try {
      const dir = path.join(keep, 'statusline')
      // Private to the person, as everything the companion keeps (bin/private.py): 0700 folders, 0600 files. A mode
      // given to mkdir or writeFile counts only for what they create and the umask can narrow it, so each is set after.
      if (isLink(dir)) throw new Error('refusing to follow a symlink')
      fs.mkdirSync(dir, { recursive: true, mode: 0o700 })
      fs.chmodSync(keep, 0o700)
      fs.chmodSync(dir, 0o700)
      const file = path.join(dir, `${session}.json`)
      if (isLink(file)) throw new Error('refusing to follow a symlink')
      const temp = `${file}.tmp`
      fs.rmSync(temp, { force: true }) // one a crash left, or a link planted there: removed itself, never written through
      const handle = fs.openSync(temp, fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL | (fs.constants.O_NOFOLLOW || 0), 0o600)
      try {
        fs.fchmodSync(handle, 0o600)
        fs.writeSync(handle, JSON.stringify({ at: Date.now() / 1000, line: redact(line), fields: redactValue(fields) }))
      } finally {
        fs.closeSync(handle)
      }
      fs.renameSync(temp, file)
      return // drawn in the pane
    } catch {}
  }
  process.stdout.write(line)
})
}

if (require.main === module) main()
module.exports = { redact, redactValue }
