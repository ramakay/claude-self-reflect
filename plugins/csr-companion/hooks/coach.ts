// Coaching and the push guard: the rules, apart from the hooks that use them (register.tsx), so a test
// can read them without an engine. No model calls anywhere here.

export const BAND_WINDOW_MS = 30_000 // a bare digit this soon after a band note showed is a hotkey miss
export const AGAIN_MS = 10_000 // the same digit sent again this soon goes through

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

export type GitVerdict = { kind: 'deny'; reason: string } | { kind: 'ask' } | { kind: 'check'; dir: string | null; remote: string | null } | null

export const NO_VERIFY = '--no-verify skips the pre-commit gate. Fix what the gate flags instead.'
export const RELEASE = 'Pushing release/v* publishes to npm. Push a backport/... branch instead, or ask the person to push it.'

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
const LEADER_TAKES_ONE = new Set(['-u', '-C', '-S', '-P', '-g', '-p']) // env -u NAME, sudo -u who, ...
const SHELLS = new Set(['bash', 'sh', 'zsh', 'dash', 'ksh'])
const GIT_TAKES_ONE = new Set(['-C', '-c', '--git-dir', '--work-tree', '--namespace', '--exec-path'])
const PUSH_TAKES_ONE = new Set(['-o', '--push-option', '--repo', '--receive-pack', '--exec'])
const COMMIT_LONG_TAKES_ONE = new Set(['--message', '--file', '--author', '--date', '--template', '--reuse-message',
  '--reedit-message', '--fixup', '--squash', '--trailer', '--cleanup', '--pathspec-from-file', '--gpg-sign'])

// Where a command's own words start: past `if`, `sudo -u x`, `env -i A=1`, `nohup` and the like.
function headAt(words: string[]): number {
  let i = 0
  let led = false
  while (i < words.length) {
    const w = words[i] as string
    if (LEADERS.has(w) || /^\w+=/.test(w)) {
      led = led || LEADERS.has(w)
      i++
    } else if (led && w.startsWith('-')) i += LEADER_TAKES_ONE.has(w) ? 2 : 1
    else break
  }
  return i
}

// What one command's words say about git: the subcommand, what follows it, and the -C directory, or null
// when it is no git.
function gitCall(words: string[]): { sub: string; rest: string[]; dir: string | null } | null {
  let i = headAt(words)
  if ((words[i] ?? '').split('/').pop() !== 'git') return null
  let dir: string | null = null
  i++
  while (i < words.length && (words[i] as string).startsWith('-')) {
    if (words[i] === '-C') dir = words[i + 1] ?? null
    i += GIT_TAKES_ONE.has(words[i] as string) ? 2 : 1
  }
  return i < words.length ? { sub: words[i] as string, rest: words.slice(i + 1), dir } : null
}

// The script a shell is told to run: `bash -c "..."`, `sh -ce '...'`, `zsh -lc ...`; only when the command
// itself is the shell, never a shell's name inside another command's words (`echo sh -c ...`).
function inner(words: string[]): string[] {
  const i = headAt(words)
  if (!SHELLS.has((words[i] ?? '').split('/').pop() ?? '')) return []
  for (let k = i + 1; k < words.length; k++) {
    const w = words[k] as string
    if (w === '-o' || w === '+o') k++ // bash -o pipefail -c ...
    else if (!w.startsWith('-')) return []
    else if (/^-[a-zA-Z]*c[a-zA-Z]*$/.test(w)) return words[k + 1] ? [words[k + 1] as string] : []
  }
  return []
}

// The scripts a shell reads from a heredoc (`bash <<EOF ... EOF`), and what $(...) or backticks run: both
// are commands, though commands() reads the one as text and the other as a word inside quotes.
function scriptsWithin(src: string): string[] {
  const out: string[] = []
  const open = /(^|[;&|\n(]\s*)((?:\w+=\S*\s+)*)(?:\S*\/)?(bash|sh|zsh|dash|ksh)\b[^\n]*?<<-?\s*(['"]?)([A-Za-z_]\w*)\4[^\n]*\n/g
  for (let m = open.exec(src); m; m = open.exec(src)) {
    const body = src.slice(m.index + m[0].length)
    const close = new RegExp(`^[ \\t]*${m[5]}[ \\t]*$`, 'm').exec(body)
    out.push(close ? body.slice(0, close.index) : body)
  }
  for (const m of src.matchAll(/\$\(([^()]*)\)|`([^`]*)`/g)) out.push((m[1] ?? m[2]) as string)
  return out
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

// Whether a git commit skips its hooks, read the way git reads its words: up to `--`, never inside an
// option's value (`-m --no-verify` is a message), the last of --no-verify / --verify winning.
function skipsHooks(rest: string[]): boolean {
  let skip = false
  for (let k = 0; k < rest.length; k++) {
    const w = rest[k] as string
    if (w === '--') break
    if (w === '--no-verify') skip = true
    else if (w === '--verify') skip = false
    else if (COMMIT_LONG_TAKES_ONE.has(w)) k++
    else if (/^-[a-zA-Z]/.test(w)) {
      for (let i = 1; i < w.length; i++) {
        const letter = w[i] as string
        if (letter === 'n') skip = true
        if (COMMIT_TAKES_ONE.includes(letter)) {
          if (i === w.length - 1 && COMMIT_TAKES_NEXT.includes(letter)) k++ // `-m msg`: msg is the value
          break
        }
      }
    }
  }
  return skip
}

// A push's words, read in order: whether it is a dry run (the last of --dry-run / --no-dry-run wins), skips
// hooks, sends everything, and the remote and refspecs it names. Option values are never read as either.
function pushWords(rest: string[]) {
  let dry = false, skip = false, every = false
  const places: string[] = []
  for (let k = 0; k < rest.length; k++) {
    const w = rest[k] as string
    if (w === '--') {
      places.push(...rest.slice(k + 1))
      break
    }
    if (PUSH_TAKES_ONE.has(w)) k++
    else if (w === '--dry-run') dry = true
    else if (w === '--no-dry-run') dry = false
    else if (w === '--no-verify') skip = true
    else if (w === '--verify') skip = false
    else if (w === '--all' || w === '--mirror' || w === '--branches') every = true
    else if (/^-[a-zA-Z]+$/.test(w)) dry = w.includes('n') || dry
    else if (w.startsWith('-') || /^\d*[<>]/.test(w)) continue
    else places.push(w)
  }
  return { dry, skip, every, remote: places[0] ?? null, specs: places.slice(1) }
}

// What a git push should come to: never for --no-verify and for release/v* (a push there publishes to
// npm); "check" when where it lands depends on the repository (no refspec, HEAD, a configured push), which
// the hook settles by asking git; "ask" for any other push (the hook lets it through); null for no push.
export function gitVerdict(command: string, depth = 0, cwd: string | null = null): GitVerdict {
  let worst: GitVerdict = null
  const keep = (found: GitVerdict) => {
    if (found && (!worst || found.kind === 'deny' || (found.kind === 'check' && worst.kind === 'ask'))) worst = found
  }
  if (depth < 3) for (const text of scriptsWithin(command)) {
    const found = gitVerdict(text, depth + 1, cwd)
    if (found?.kind === 'deny') return found
    keep(found)
  }
  let here = cwd
  for (const words of commands(command)) {
    if (words[0] === 'cd' && words[1]) here = words[1].startsWith('/') ? words[1] : null
    for (const text of depth < 3 ? inner(words) : []) {
      const found = gitVerdict(text, depth + 1, here)
      if (found?.kind === 'deny') return found
      keep(found)
    }
    const call = gitCall(words)
    if (!call) continue
    const { sub, rest } = call
    if (sub === 'commit' && skipsHooks(rest)) return { kind: 'deny', reason: NO_VERIFY }
    if (sub === 'merge' && rest.slice(0, rest.includes('--') ? rest.indexOf('--') : undefined).includes('--no-verify'))
      return { kind: 'deny', reason: NO_VERIFY }
    if (sub !== 'push') continue
    const push = pushWords(rest)
    if (push.skip) return { kind: 'deny', reason: NO_VERIFY }
    if (push.dry) continue // nothing is sent
    if (push.every || push.specs.some(s => s.includes('*'))) return { kind: 'deny', reason: RELEASE }
    if (push.specs.some(s => /^release\/v/.test(destination(s)))) return { kind: 'deny', reason: RELEASE }
    const dir = call.dir ? (call.dir.startsWith('/') || !here ? call.dir : `${here}/${call.dir}`) : here
    if (!push.specs.length || push.specs.some(s => ['HEAD', '@'].includes(destination(s))))
      keep({ kind: 'check', dir, remote: push.remote })
    else keep({ kind: 'ask' })
  }
  return worst
}

// What a "check" push lands on, from git itself: the current branch, its upstream, and any configured push
// refspec. Release when any of them is release/v* (or a configured refspec is a wildcard).
export function releaseFromGit(branch: string, upstream: string, pushConfig: string): boolean {
  if (/^release\/v/.test(branch.trim())) return true
  if (/(^|\/)release\/v/.test(upstream.trim().replace(/^[^/]+\//, ''))) return true
  return pushConfig.split('\n').some(line => {
    const spec = line.trim().split(/\s+/).pop() ?? ''
    return spec.includes('*') || /^release\/v/.test(destination(spec))
  })
}
