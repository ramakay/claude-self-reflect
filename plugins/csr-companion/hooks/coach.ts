// Coaching and the push guard: the rules, apart from the hooks that use them (register.tsx), so a test
// can read them without an engine. No model calls anywhere here.

export const BAND_WINDOW_MS = 30_000 // a bare digit this soon after a band note showed is a hotkey miss
export const AGAIN_MS = 10_000 // the same digit sent again this soon goes through
export const PUSH_OK_MS = 60_000 // "push" in a prompt lets an identical git push through this long

// A prompt that is only one digit: what a band hotkey would have been.
export const isBareDigit = (text: string): boolean => /^\s*\d\s*$/.test(text)

// One line more under "## Terms" of a CLAUDE.md, the heading made if it is not there. Never twice.
export function addTerm(text: string, line: string): string {
  if (text.includes(line)) return text
  const lines = text.split('\n')
  const at = lines.findIndex(row => /^##\s+Terms\s*$/.test(row))
  if (at === -1) return `${text.trim() ? `${text.replace(/\n*$/, '\n')}\n` : ''}## Terms\n${line}\n`
  let end = lines.length
  for (let i = at + 1; i < lines.length; i++) {
    if (/^#{1,6}\s/.test(lines[i] ?? '')) {
      end = i
      break
    }
  }
  while (end > at + 1 && !(lines[end - 1] ?? '').trim()) end--
  lines.splice(end, 0, line)
  return lines.join('\n')
}

// ---------------------------------------------------------------- the push guard

export type GitVerdict = { kind: 'deny'; reason: string } | { kind: 'ask' } | null

export const NO_VERIFY = '--no-verify skips the pre-commit gate. Fix what the gate flags instead.'
export const RELEASE = 'Pushing release/v* publishes to npm. Push a backport/... branch instead, or ask the person to push it.'
export const SAY_PUSH = 'git push needs the person first. Say push to allow it: the identical command goes through for 60 s after.'

// A heredoc's body is text, not commands: cut from the line after `<<EOF` to the line that ends it.
function withoutHeredocs(src: string): string {
  let out = src
  for (let guard = 0; guard < 20; guard++) {
    const open = /<<-?\s*(['"]?)([A-Za-z_][\w]*)\1[^\n]*\n/.exec(out)
    if (!open) break
    const bodyAt = open.index + open[0].length
    const close = new RegExp(`^[ \\t]*${open[2]}[ \\t]*$`, 'm').exec(out.slice(bodyAt))
    out = out.slice(0, bodyAt) + (close ? out.slice(bodyAt + close.index + close[0].length) : '')
  }
  return out
}

// The simple commands of a shell line, each as its words with quotes taken off: split at ; & | ( ) ` and
// newlines outside quotes. Best effort, not a shell: enough to tell `git push` as a command from the
// words "git push" inside an echo, a quoted message or a heredoc.
export function commands(source: string): string[][] {
  const src = withoutHeredocs(source)
  const out: string[][] = []
  let words: string[] = []
  let word = ''
  let has = false
  let quote = ''
  const endWord = () => {
    if (has) words.push(word)
    word = ''
    has = false
  }
  const endCommand = () => {
    endWord()
    if (words.length) out.push(words)
    words = []
  }
  for (let i = 0; i < src.length; i++) {
    const c = src[i] as string
    if (quote) {
      if (c === quote) quote = ''
      else if (c === '\\' && quote === '"' && i + 1 < src.length) word += src[++i]
      else word += c
    } else if (c === "'" || c === '"') {
      quote = c
      has = true
    } else if (c === '\\') {
      if (i + 1 < src.length) {
        word += src[++i]
        has = true
      }
    } else if (c === '#' && !has) {
      while (i + 1 < src.length && src[i + 1] !== '\n') i++
    } else if (c === ' ' || c === '\t') endWord()
    else if (';&|\n()`{}'.includes(c)) endCommand()
    else {
      word += c
      has = true
    }
  }
  endCommand()
  return out
}

const LEADERS = new Set(['if', 'then', 'else', 'elif', 'while', 'until', 'do', '!', 'time', 'sudo', 'command', 'exec', 'nohup', 'env', 'builtin'])
const GIT_TAKES_ONE = new Set(['-C', '-c', '--git-dir', '--work-tree', '--namespace', '--exec-path'])
const PUSH_TAKES_ONE = new Set(['-o', '--push-option', '--repo', '--receive-pack', '--exec'])

// What one command's words say about git: the subcommand and what follows it, or null when it is no git.
function gitCall(words: string[]): { sub: string; rest: string[] } | null {
  let i = 0
  while (i < words.length && (LEADERS.has(words[i] as string) || /^\w+=/.test(words[i] as string))) i++
  const head = (words[i] ?? '').split('/').pop()
  if (['bash', 'sh', 'zsh'].includes(head ?? '')) return null // handled by the caller, which reads -c
  if (head !== 'git') return null
  i++
  while (i < words.length && (words[i] as string).startsWith('-')) i += GIT_TAKES_ONE.has(words[i] as string) ? 2 : 1
  return i < words.length ? { sub: words[i] as string, rest: words.slice(i + 1) } : null
}

// The shell lines inside `bash -c "..."`, which are commands too.
function inner(words: string[]): string[] {
  const i = words.findIndex(w => ['bash', 'sh', 'zsh'].includes(w.split('/').pop() ?? ''))
  const c = i === -1 ? -1 : words.findIndex((w, k) => k > i && /^-[a-z]*c$/.test(w))
  return c !== -1 && words[c + 1] ? [words[c + 1] as string] : []
}

// A refspec's destination: `+HEAD:refs/heads/release/v9` -> release/v9; `release/v9` -> release/v9.
function destination(spec: string): string {
  const target = spec.replace(/^\+/, '').split(':').pop() ?? ''
  return target.replace(/^refs\/heads\//, '')
}

// Short options of git commit that take a value: in `-uno` or `-mn` what follows the letter is the value,
// not more options, and a cluster ending in one takes the next word as its value.
const COMMIT_TAKES_ONE = 'mFcCtuS'
const COMMIT_TAKES_NEXT = 'mFcCt'

// Whether a git commit's words say -n (the short --no-verify), alone or in a cluster like -an, read the
// way git reads them: up to `--`, and never inside an option's value.
function skipsHooks(rest: string[]): boolean {
  for (let k = 0; k < rest.length; k++) {
    const w = rest[k] as string
    if (w === '--') return false
    if (!/^-[a-zA-Z]/.test(w)) continue
    for (let i = 1; i < w.length; i++) {
      const letter = w[i] as string
      if (letter === 'n') return true
      if (COMMIT_TAKES_ONE.includes(letter)) {
        if (i === w.length - 1 && COMMIT_TAKES_NEXT.includes(letter)) k++ // `-m msg`: msg is the value
        break
      }
    }
  }
  return false
}

// Whether a prompt gives the word to push: it says push, and not as "don't push", "do not push", "never
// push", "no push", "not ... push" or "without pushing" a few words before.
export function saysPush(text: string): boolean {
  if (!/\bpush\b/i.test(text)) return false
  return !/\b(?:don[\'’]?t|do not|never|not|no|without|stop|hold off(?: on)?)\b[^.!?\n]{0,24}\bpush/i.test(text)
}

// What a git push should come to: never for --no-verify and for release/v* (a push there publishes to
// npm), ask for any other push, null for no push at all.
export function gitVerdict(command: string, depth = 0): GitVerdict {
  let worst: GitVerdict = null
  for (const words of commands(command)) {
    for (const text of depth < 2 ? inner(words) : []) {
      const found = gitVerdict(text, depth + 1)
      if (found && (!worst || found.kind === 'deny')) worst = found
    }
    const call = gitCall(words)
    if (!call) continue
    const { sub, rest } = call
    if (['commit', 'push', 'merge'].includes(sub) && (rest.includes('--no-verify') || (sub === 'commit' && skipsHooks(rest))))
      return { kind: 'deny', reason: NO_VERIFY }
    if (sub !== 'push') continue
    if (rest.includes('--dry-run') || rest.includes('-n')) continue // nothing is sent
    const places: string[] = []
    let danger = rest.includes('--all') || rest.includes('--mirror')
    for (let k = 0; k < rest.length; k++) {
      const w = rest[k] as string
      if (PUSH_TAKES_ONE.has(w)) k++
      else if (w.startsWith('-') || /^\d*[<>]/.test(w)) continue
      else places.push(w)
    }
    if (places.some(w => /^release\/v/.test(destination(w)))) danger = true
    if (danger) return { kind: 'deny', reason: RELEASE }
    worst = worst ?? { kind: 'ask' }
  }
  return worst
}
