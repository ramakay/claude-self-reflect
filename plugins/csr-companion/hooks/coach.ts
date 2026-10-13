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

// A push whose landing the repository decides: its directory, the remote it names, its git config arguments in the
// order given (`-c name=value` as the string, `--config-env=name=VARIABLE` as { env: 'name=VARIABLE' }, its value read
// when the hook asks git: git takes the last of two, so the order is kept, astra r3 #2), and the variables the
// command line sets that git reads: those --config-env names, and GIT_CONFIG_* (GIT_CONFIG_COUNT/KEY_n/VALUE_n).
// implicit: it names no refspec, so git takes one from the remote's push refspecs or push.default (Codex r2 #5).
export type ConfigArg = string | { env: string }
export type GitPush = {
  dir: string | null
  remote: string | null
  config: ConfigArg[]
  env?: Record<string, string>
  implicit?: true
}
export type GitVerdict = { kind: 'deny'; reason: string } | { kind: 'ask' } | { kind: 'check'; pushes: GitPush[] } | null

export const NO_VERIFY = '--no-verify skips the pre-commit gate. Fix what the gate flags instead.'
// A reason a guard file may give for release/v* (the guard's own tests use it); a push the guard protects by a glob
// with no reason of its own is told PROTECTED. This repository ships no guard: the guard is off here.
export const RELEASE = 'Pushing release/v* publishes to npm. Push a backport/... branch instead, or ask the person to push it.'
export const PROTECTED = (glob: string) => `Pushing to ${glob} is blocked by csr-guard.json. Push another branch, or ask the person to push it.`
export const HOOKS_OFF = 'core.hooksPath set for one git call skips the pre-commit gate. Fix what the gate flags instead.'
export const UNRESOLVED = 'This git command depends on something the check cannot resolve (a $variable or substitution in a setting, a variable it cannot read, or settings git refuses), so it is denied. Write the values out, or ask the person to run it.'
export const AS_TEXT = 'If this is only text (a commit message, a doc), put it in a file and use git commit -F <file>.'

// ---------------------------------------------------------------- what the guard guards
//
// The guard is opt-in: a public plugin never imposes one repository's rules on another (the owner, 10-12: "release/v*
// publishes to npm" was hardcoded for everyone). A repository says what it protects in <git toplevel>/.claude/
// csr-guard.json, a person for every repository in csr-companion's guard.json; the two are one guard, every protection
// of either (register.tsx, guardOf). Neither there: no guard, no git asked anything.
//   denyNoVerify   a commit, merge or push that skips the pre-commit gate (--no-verify, -n, core.hooksPath)
//   protectedPush  globs on the branch a push lands on (`release/v*`, `release/*`: * runs across /)
//   reason         what to tell Claude, per glob
export type Guard = { denyNoVerify: boolean; protectedPush: string[]; reason: Record<string, string> }

// A guard file's words, or null when they are not one (not JSON, or a key of the wrong kind).
export function parseGuard(text: string): Guard | null {
  let value: unknown
  try {
    value = JSON.parse(text)
  } catch {
    return null
  }
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null
  const { denyNoVerify = false, protectedPush = [], reason = {} } = value as Record<string, unknown>
  if (typeof denyNoVerify !== 'boolean' || !Array.isArray(protectedPush) || protectedPush.some(glob => typeof glob !== 'string' || !glob)) return null
  if (!reason || typeof reason !== 'object' || Array.isArray(reason) || Object.values(reason).some(said => typeof said !== 'string')) return null
  return { denyNoVerify, protectedPush: protectedPush as string[], reason: reason as Record<string, string> }
}

// Every protection of each guard; null when there is none at all.
export function mergeGuards(guards: readonly Guard[]): Guard | null {
  const merged: Guard = {
    denyNoVerify: guards.some(guard => guard.denyNoVerify),
    protectedPush: [...new Set(guards.flatMap(guard => guard.protectedPush))],
    reason: Object.assign({}, ...[...guards].reverse().map(guard => guard.reason)) as Record<string, string>, // the first one's wins
  }
  return merged.denyNoVerify || merged.protectedPush.length ? merged : null
}

const globRegex = (glob: string) => new RegExp(`^${glob.replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*').replace(/\?/g, '.')}$`)
// The glob a branch name is protected by, or null.
export const protectedBy = (guard: Guard, branch: string): string | null =>
  guard.protectedPush.find(glob => globRegex(glob).test(branch)) ?? null
export const reasonFor = (guard: Guard, glob: string) => guard.reason[glob] ?? PROTECTED(glob)
const anyProtected = (guard: Guard) => reasonFor(guard, guard.protectedPush[0] as string) // every branch: the first glob's

// Before any parsing, over the raw text: a git command with words that are never right in it is denied. Three rounds
// of shell-parsing bypasses (astra r3 #1: `((1<<2))` and a continued `cat <<1 \` hid a push) say the parser cannot
// be the only gate. It reads only where git stands as the command: the text cut at newlines, ; && || | & ( ) { }
// backticks and $(, quotes ignored, each piece's head past its assignments and wrappers (env, sudo, nohup, timeout
// <n>, xargs, ...). So a heredoc's line that starts with git trips it (accepted), and `codex exec "... git ...
// --no-verify ..."` or `grep 'release/v' f && git status` do not (the owner, 10-12). The words are the guard's:
// --no-verify and hooksPath for denyNoVerify; for protectedPush, each glob's fixed part (`release/v` of `release/v*`),
// --mirror, and a ':' or '+:' push refspec. A denial the parse does not confirm adds AS_TEXT.
const GIT_WORD = /(?:^|[^\w.-])git(?![\w-])/
const PIECES = /\n|;|&&|\|\||\||&|\(|\)|\{|\}|`|\$\(/
export function gitPieces(command: string): string[] {
  return command.split(PIECES).filter(piece => {
    const words = piece.trim().split(/\s+/).map(word => word.replace(/['"\\]/g, '')).filter(Boolean)
    const head = words[headAt(words)] ?? ''
    return head === 'git' || head.endsWith('/git')
  })
}
export function tripwire(command: string, guard: Guard): string | null {
  for (const piece of gitPieces(command)) {
    if (guard.denyNoVerify && piece.includes('--no-verify')) return NO_VERIFY
    if (guard.denyNoVerify && /hookspath/i.test(piece)) return HOOKS_OFF
    if (!guard.protectedPush.length) continue
    for (const glob of guard.protectedPush) {
      const fixed = glob.split(/[*?[]/)[0] ?? ''
      if (fixed ? piece.includes(fixed) : /(?:^|\s)push(?:\s|$)/.test(piece)) return reasonFor(guard, glob)
    }
    const matching = piece.split(/\s+/).some(word => /^\+?:/.test(word.replace(/^['"]+/, '')))
    if (piece.includes('--mirror') || (/(?:^|\s)push(?:\s|$)/.test(piece) && matching)) return anyProtected(guard)
  }
  return null
}

// The heredoc words one line declares, in order, as the shell reads the line: every `<<WORD` and `<<-WORD` outside
// quotes, a comment and arithmetic (`$((1<<x))`, a `((1<<2))` command or `for ((...))`, `$[1<<2]`: shifts, astra r3
// #1); `<<<` is a here-string. A word with any quote or backslash in it ('EOF', "EOF", \EOF, E"O"F) is quoted: its
// body is text, $(...) included. `at`: where its `<<` stands on the line.
function heredocWords(line: string): Array<{ word: string; quoted: boolean; at: number }> {
  const out: Array<{ word: string; quoted: boolean; at: number }> = []
  let quote = ''
  for (let i = 0; i < line.length; i++) {
    const c = line[i] as string
    if (quote) {
      if (c === quote) quote = ''
      else if (c === '\\' && quote === '"') i++
    } else if (c === "'" || c === '"') quote = c
    else if (c === '\\') i++
    else if (c === '#' && (i === 0 || /\s/.test(line[i - 1] as string))) break
    else if (line.startsWith('$((', i) || line.startsWith('((', i)) {
      const end = line.indexOf('))', i + 2)
      i = end === -1 ? line.length : end + 1
    } else if (line.startsWith('$[', i)) {
      const end = line.indexOf(']', i + 2)
      i = end === -1 ? line.length : end
    } else if (line.startsWith('<<<', i)) i += 2
    else if (line.startsWith('<<', i)) {
      let j = i + 2 + (line[i + 2] === '-' ? 1 : 0)
      while (line[j] === ' ' || line[j] === '\t') j++
      let word = ''
      let quoted = false
      let inside = ''
      for (; j < line.length; j++) {
        const d = line[j] as string
        if (inside) {
          if (d === inside) inside = ''
          else word += d
        } else if (d === "'" || d === '"') {
          inside = d
          quoted = true
        } else if (d === '\\') {
          quoted = true
          word += line[++j] ?? ''
        } else if (/[\s;&|<>()]/.test(d)) break
        else word += d
      }
      if (word) out.push({ word, quoted, at: i })
      i = j - 1
    }
  }
  return out
}

// A shell line's heredocs, one shared reading for the commands and for the scripts: the source with each body cut
// (the lines after the line that declares it, to the line that is its word), and each body with whether its word was
// quoted and whether a shell on that line reads it (`bash <<EOF`, `cat <<'EOF' | sh`: a script, whatever the quotes).
// Every word a line declares, its bodies one after another: only the first was cut, and `cat <<'A' <<'B'` left B's
// text to be read as commands (Codex r2 #12); `bash <<\EOF` ran its push, read as text (Codex r2 #1). Cut one line
// after another, never rescanned from the top (Codex review #2).
// A line continued by a backslash at its end is one logical line with the next (astra r3 #1: `cat <<1 \` went on
// to `; git push ...`, a command, read as the heredoc's body). A heredoc's body is a script only when a shell is in
// its own pipeline (`bash <<EOF`, `cat <<EOF | sh`), never for a shell elsewhere on the line (astra r3 #9).
type Heredoc = { body: string; quoted: boolean; shell: boolean }
const CONTINUED = /(?<!\\)(?:\\\\)*\\$/
function heredocs(src: string): { text: string; bodies: Heredoc[] } {
  const lines = src.split('\n')
  const kept: string[] = []
  const bodies: Heredoc[] = []
  for (let i = 0; i < lines.length; i++) {
    let logical = lines[i] as string
    kept.push(logical)
    while (CONTINUED.test(logical) && i + 1 < lines.length) {
      logical = logical.slice(0, -1) + (lines[++i] as string)
      kept.push(lines[i] as string)
    }
    for (const { word, quoted, at } of heredocWords(logical)) {
      const shell = split(pipelineOf(logical, at)).some(words => SHELLS.has((words[headAt(words)] ?? '').split('/').pop() ?? ''))
      const body: string[] = []
      for (i++; i < lines.length && (lines[i] as string).trim() !== word; i++) body.push(lines[i] as string)
      bodies.push({ body: body.join('\n'), quoted, shell })
    }
  }
  return { text: kept.join('\n'), bodies }
}

// The pipeline of a line that holds position `at`: the line cut at ; && || and a lone & outside quotes (a pipe |
// keeps a pipeline whole; >&, &> and |& are redirections or pipes).
function pipelineOf(line: string, at: number): string {
  let start = 0
  let quote = ''
  for (let i = 0; i < line.length; i++) {
    const c = line[i] as string
    if (quote) {
      if (c === quote) quote = ''
      else if (c === '\\' && quote === '"') i++
      continue
    }
    if (c === "'" || c === '"') quote = c
    else if (c === '\\') i++
    else {
      const pair = line.slice(i, i + 2)
      const width = pair === '&&' || pair === '||' ? 2
        : c === ';' || (c === '&' && !/[<>|]/.test(line[i - 1] ?? '') && line[i + 1] !== '>') ? 1 : 0
      if (!width) continue
      if (i >= at) return line.slice(start, i)
      start = i + width
      i += width - 1
    }
  }
  return line.slice(start)
}

// Where the `)` that closes a $( at `from` is: nested parentheses and quoted ones counted as the shell counts them.
function closing(text: string, from: number): number {
  let depth = 1
  let quote = ''
  for (let i = from; i < text.length; i++) {
    const c = text[i] as string
    if (quote) {
      if (c === quote) quote = ''
      else if (c === '\\' && quote === '"') i++
    } else if (c === "'" || c === '"') quote = c
    else if (c === '\\') i++
    else if (c === '(') depth++
    else if (c === ')' && --depth === 0) return i
  }
  return text.length
}

// What $(...) and backticks run, read as the shell reads them: never inside single quotes, where they are text
// (inside double quotes they still run). In a heredoc's body quotes are text too (quoting: false). Codex review
// #7: printf '%s' '$(git push origin release/v9)' prints those words, and was denied.
function substitutions(text: string, quoting: boolean): string[] {
  const out: string[] = []
  let quote = ''
  for (let i = 0; i < text.length; i++) {
    const c = text[i] as string
    if (quote === "'") {
      if (c === "'") quote = ''
    } else if (c === '\\') i++
    else if (quoting && c === "'" && !quote) quote = "'"
    else if (quoting && c === '"') quote = quote ? '' : '"'
    else if (c === '$' && text[i + 1] === '(' && text[i + 2] !== '(') { // $(( )) is arithmetic
      const end = closing(text, i + 2)
      out.push(text.slice(i + 2, end))
      i = end
    } else if (c === '`') {
      const end = text.indexOf('`', i + 1)
      out.push(text.slice(i + 1, end === -1 ? undefined : end))
      i = end === -1 ? text.length : end
    }
  }
  return out
}

// The simple commands of a shell line, each as its words with quotes taken off: split at ; & | ( ) ` and
// newlines outside quotes. Best effort, not a shell: enough to tell `git push` as a command from the
// words "git push" inside an echo, a quoted message or a heredoc.
export function commands(source: string): string[][] {
  return split(heredocs(source).text)
}

// The words of each simple command in shell text with no heredoc bodies in it.
function split(src: string): string[][] {
  return splitWords(src).map(words => words.map(word => word.text))
}

// The same, each word with whether the shell expands something in it ($NAME, ${...}, $(...), a backtick) outside
// single quotes: a value the check cannot know.
type Word = { text: string; expands: boolean }
function splitWords(src: string): Word[][] {
  const out: Word[][] = []
  let words: Word[] = []
  let word = ''
  let has = false
  let expands = false
  let quote = ''
  const endWord = () => {
    if (has) words.push({ text: word, expands })
    word = ''
    has = false
    expands = false
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
      else {
        if (quote === '"' && (c === '$' || c === '`')) expands = true
        word += c
      }
    } else if (c === "'" || c === '"') {
      quote = c
      has = true
    } else if (c === '\\') {
      if (src[i + 1] === '\n') i++ // a line continued: the two are one
      else if (i + 1 < src.length) {
        word += src[++i]
        has = true
      }
    } else if (c === '#' && !has) {
      while (i + 1 < src.length && src[i + 1] !== '\n') i++
    } else if (c === ' ' || c === '\t') endWord()
    else if (';&|\n()`{}'.includes(c)) {
      if (has && (c === '`' || (c === '(' && word.endsWith('$')))) expands = true // `X=$(cmd)`, X=`cmd`: X is unknown
      endCommand()
    } else {
      if (c === '$') expands = true
      word += c
      has = true
    }
  }
  endCommand()
  return out
}

// Words that run the command after them, each with its options that take the next word as their value, short and
// long: env -u NAME, sudo -u who, timeout -s KILL, nice -n 5, stdbuf -o L. getopt takes a long one by any prefix
// (`timeout --sig TERM 60`). sudo -S and -n take none: one list for all of them read `sudo -S git push` as a
// password "git". PR #317 review: timeout and nice hid a push; Codex review #1: so did `timeout --signal TERM 60`.
const LEADERS = new Map<string, readonly string[]>([
  ...['if', 'then', 'else', 'elif', 'while', 'until', 'do', '!', 'time', 'command', 'nohup', 'builtin'].map(w => [w, []] as const),
  ['sudo', ['-u', '-g', '-p', '-C', '-U', '-r', '-t', '-D', '-R', '-T', '--user', '--group', '--prompt', '--close-from',
    '--other-user', '--role', '--type', '--chdir', '--chroot', '--command-timeout']],
  ['env', ['-u', '-C', '-S', '--unset', '--chdir', '--split-string']], ['exec', ['-a']],
  ['timeout', ['-s', '-k', '--signal', '--kill-after']], ['nice', ['-n', '--adjustment']],
  ['stdbuf', ['-i', '-o', '-e', '--input', '--output', '--error']],
  ['xargs', ['-I', '-n', '-P', '-L', '-s', '-E', '-d', '-a', '--max-args', '--max-procs', '--max-lines', '--delimiter', '--arg-file']],
])
const LEADER_THEN_ONE = new Set(['timeout']) // timeout [options] DURATION command
const leaderTakes = (takes: readonly string[], w: string) =>
  takes.includes(w) || (/^--[^=]{2,}$/.test(w) && takes.some(option => option.startsWith(w)))
const SHELLS = new Set(['bash', 'sh', 'zsh', 'dash', 'ksh'])
const GIT_TAKES_ONE = new Set(['-C', '-c', '--config-env', '--git-dir', '--work-tree', '--namespace', '--exec-path'])
// core.hooksPath set for one call (-c, --config-env, or GIT_CONFIG_* in its environment) points git at other hooks,
// or none: `git -c core.hooksPath=/dev/null commit` runs no pre-commit gate (probed on git 2.50).
const HOOKS_PATH = /^core\.hookspath(=|$)/i
const HOOKS_PATH_ENV = /^GIT_CONFIG_(?:KEY_\d+=core\.hookspath$|PARAMETERS=.*core\.hookspath)/i

// ---------------------------------------------------------------- git's own options
//
// Each subcommand's long options as `git <sub> -h` lists them (git 2.50, hidden ones added): `[no-]` for one git
// also takes as no-<name>, `=` for one whose value is required (a separate next word is it), `=?` for one whose
// value is optional (only after '='; `--gpg-sign --no-verify` skips the hooks, PR #317 review). Git takes a long
// option by any prefix no other name shares, and refuses one that two share: --no-dry-ru is --no-dry-run, --mirr
// --mirror, --dry --dry-run; `git merge --no-veri` could be --no-verify-signatures (Codex review #3; probed).
type Kind = 'flag' | 'value' | 'optional'
function optionTable(spec: string): Map<string, Kind> {
  const table = new Map<string, Kind>()
  for (const entry of spec.split(/\s+/).filter(Boolean)) {
    const bare = entry.replace('[no-]', '')
    const name = bare.replace(/=\??$/, '')
    table.set(name, bare.endsWith('=?') ? 'optional' : bare.endsWith('=') ? 'value' : 'flag')
    if (entry.startsWith('[no-]')) table.set(`no-${name}`, 'flag')
  }
  return table
}
const PUSH_OPTIONS = optionTable(`[no-]verbose [no-]quiet [no-]repo= [no-]all [no-]branches [no-]mirror [no-]delete [no-]tags
  [no-]dry-run [no-]porcelain [no-]force [no-]force-with-lease=? [no-]force-if-includes [no-]recurse-submodules= [no-]thin
  [no-]receive-pack= [no-]exec= [no-]set-upstream [no-]progress [no-]prune no-verify verify [no-]follow-tags [no-]signed=?
  [no-]atomic [no-]push-option= ipv4 ipv6`)
const COMMIT_OPTIONS = optionTable(`[no-]quiet [no-]verbose [no-]file= [no-]author= [no-]date= [no-]message= [no-]reedit-message=
  [no-]reuse-message= [no-]fixup= [no-]squash= [no-]reset-author trailer= [no-]signoff [no-]template= [no-]edit [no-]cleanup=
  [no-]status [no-]gpg-sign=? [no-]all [no-]include [no-]interactive [no-]patch [no-]only no-verify verify [no-]dry-run
  [no-]short [no-]branch [no-]ahead-behind [no-]porcelain [no-]long [no-]null [no-]amend no-post-rewrite post-rewrite
  [no-]untracked-files=? [no-]pathspec-from-file= [no-]pathspec-file-nul allow-empty allow-empty-message`)
const MERGE_OPTIONS = optionTable(`[no-]stat [no-]summary [no-]log=? [no-]squash [no-]commit [no-]edit [no-]cleanup= [no-]ff ff-only
  [no-]rerere-autoupdate [no-]verify-signatures [no-]strategy= [no-]strategy-option= [no-]message= file= [no-]into-name=
  [no-]verbose [no-]quiet [no-]abort [no-]quit [no-]continue [no-]allow-unrelated-histories [no-]progress [no-]gpg-sign=?
  [no-]autostash [no-]overwrite-ignore [no-]signoff no-verify verify`)
// Short options: those whose value is required (the rest of the cluster, else the next word), those whose value is
// optional (the rest of the cluster only), and the long name each flag stands for.
type Shorts = { value: string; optional: string; names: Record<string, string> }
const PUSH_SHORTS: Shorts = { value: 'o', optional: '', names: { n: 'dry-run', d: 'delete', f: 'force', u: 'set-upstream', v: 'verbose', q: 'quiet' } }
const COMMIT_SHORTS: Shorts = { value: 'mFcCt', optional: 'Su', names: { n: 'no-verify' } } // -uno: -u's mode "no"
const MERGE_SHORTS: Shorts = { value: 'msXF', optional: 'S', names: {} } // merge's -n is --no-stat

// One git subcommand's words, read as git reads them: up to `--`, never inside an option's value, each flag set
// in order so the last of a pair wins. `on` holds each flag's last state; skip says whether --no-verify won.
function gitOptions(rest: string[], longs: Map<string, Kind>, shorts: Shorts) {
  const on = new Map<string, boolean>()
  let skip = false
  const places: string[] = []
  const set = (name: string) => {
    if (name === 'no-verify' || name === 'verify') skip = name === 'no-verify'
    else if (name.startsWith('no-') && longs.has(name.slice(3))) on.set(name.slice(3), false)
    else on.set(name, true)
  }
  for (let k = 0; k < rest.length; k++) {
    const w = rest[k] as string
    if (w === '--') {
      places.push(...rest.slice(k + 1))
      break
    }
    if (w.startsWith('--')) {
      const eq = w.indexOf('=')
      const typed = w.slice(2, eq === -1 ? undefined : eq)
      const sharing = [...longs.keys()].filter(name => name.startsWith(typed))
      const name = longs.has(typed) ? typed : sharing.length === 1 ? sharing[0] : undefined
      if (!name) continue // unknown, or a prefix two names share: git refuses the command
      set(name)
      if (longs.get(name) === 'value' && eq === -1) k++
    } else if (/^-[^-]/.test(w)) {
      for (let i = 1; i < w.length; i++) {
        const letter = w[i] as string
        if (shorts.names[letter]) set(shorts.names[letter] as string)
        if (shorts.value.includes(letter)) {
          if (i === w.length - 1) k++ // `-m msg`: msg is the value
          break
        }
        if (shorts.optional.includes(letter)) break
      }
    } else if (!/^\d*[<>]/.test(w)) places.push(w) // 2>&1 and the like are no place
  }
  return { on, skip, places }
}

// A push's words: whether it is a dry run, skips hooks, sends every branch (--all, --branches, --mirror), and the
// remote and refspecs it names.
function pushWords(rest: string[]) {
  const { on, skip, places } = gitOptions(rest, PUSH_OPTIONS, PUSH_SHORTS)
  const every = ['all', 'branches', 'mirror'].some(name => on.get(name))
  return { dry: on.get('dry-run') === true, skip, every, remote: places[0] ?? null, specs: places.slice(1) }
}

// Where a command's own words start: past `if`, `sudo -u x`, `env -i A=1`, `nohup` and the like.
function headAt(words: string[]): number {
  let i = 0
  let takes: readonly string[] | null = null // the options of the last leader, once there is one
  let owed = 0 // words a leader takes after its options: timeout's duration
  while (i < words.length) {
    const w = words[i] as string
    const leader = owed ? undefined : LEADERS.get(w)
    if (leader) {
      takes = leader
      owed = LEADER_THEN_ONE.has(w) ? 1 : 0
      i++
    } else if (!owed && /^\w+=/.test(w)) i++
    else if (takes && w.length > 1 && w.startsWith('-')) i += leaderTakes(takes, w) ? 2 : 1
    else if (owed) {
      owed--
      i++
    } else break
  }
  return i
}

// A shell assignment, NAME=value, and the value with whether the shell expands something in it.
const ASSIGNMENT = /^([A-Za-z_]\w*)=([\s\S]*)$/
type Value = { value: string; expands: boolean }
const assigned = (words: readonly Word[]) =>
  Object.fromEntries(words.flatMap(w => {
    const found = ASSIGNMENT.exec(w.text)
    return found ? [[found[1] as string, { value: found[2] as string, expands: w.expands }]] : []
  })) as Record<string, Value>

type GitCall = {
  sub: string
  rest: string[]
  dir: string | null
  dirExpands: boolean // -C $DIR: where it runs is unknown
  hooksOff: boolean
  config: ConfigArg[] // -c and --config-env, in the order given
  configExpands: boolean // a -c or --config-env argument the shell expands: what it sets is unknown
  env: Record<string, Value> // the variables set before it (`DEST=... git`, `env DEST=... git`)
}
// What one command's words say about git: the subcommand, what follows it, the -C directory, the -c and
// --config-env settings in their order, and the variables set before it; null when it is no git.
function gitCall(tokens: Word[]): GitCall | null {
  const words = tokens.map(token => token.text)
  const head = headAt(words)
  let i = head
  if ((words[i] ?? '').split('/').pop() !== 'git') return null
  let dir: string | null = null
  let dirExpands = false
  let hooksOff = words.slice(0, head).some(w => HOOKS_PATH_ENV.test(w))
  const config: ConfigArg[] = []
  let configExpands = false
  i++
  while (i < words.length && (words[i] as string).startsWith('-')) {
    const w = words[i] as string
    const next = tokens[i + 1]
    if (w === '-C') {
      dir = next?.text ?? null
      dirExpands = dirExpands || !!next?.expands
    }
    const joined = w.startsWith('--config-env=')
    const set = w === '-c' || w === '--config-env' ? next?.text ?? '' : joined ? w.slice(13) : ''
    hooksOff = hooksOff || HOOKS_PATH.test(set)
    if (w === '-c' || w === '--config-env' || joined) configExpands = configExpands || !!(joined ? tokens[i]?.expands : next?.expands)
    if (w === '-c' && next) config.push(set)
    else if (w === '--config-env' || joined) config.push({ env: set })
    i += GIT_TAKES_ONE.has(w) ? 2 : 1
  }
  if (i >= words.length) return null
  return { sub: words[i] as string, rest: words.slice(i + 1), dir, dirExpands, hooksOff, config, configExpands, env: assigned(tokens.slice(0, head)) }
}

// The variables git reads that a command line sets: those its --config-env settings name, and GIT_* (GIT_CONFIG_COUNT/
// KEY_n/VALUE_n set config). unknown: one of them the shell expands (`GIT_CONFIG_COUNT=$N`, astra r3 #3). Only the
// named ones and GIT_CONFIG_* go to the hook's git: never PATH and the like.
function gitEnv(config: readonly ConfigArg[], set: Record<string, Value>): { env: Record<string, string>; unknown: boolean } {
  const named = new Set(config.flatMap(arg => (typeof arg === 'string' ? [] : [arg.env.slice(arg.env.indexOf('=') + 1)])))
  const read = Object.entries(set).filter(([name]) => named.has(name) || /^GIT_/.test(name))
  return {
    env: Object.fromEntries(read.filter(([name]) => named.has(name) || /^GIT_CONFIG/.test(name)).map(([name, { value }]) => [name, value])),
    unknown: read.some(([, { expands }]) => expands),
  }
}

// The script a shell is told to run: `bash -c "..."`, `sh -ce '...'`, `zsh -lc ...`, or eval's words; only when the
// command itself is the shell, never a shell's name inside another command's words (`echo sh -c ...`).
function inner(words: string[]): string[] {
  const i = headAt(words)
  if (words[i] === 'eval') return words.length > i + 1 ? [words.slice(i + 1).join(' ')] : [] // eval runs its words as a script
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
// are commands, though commands() reads the one as text and the other as a word inside quotes. One reading of
// the heredocs (heredocs) for both: a shell's heredoc is its script whatever its word's quotes.
function scriptsWithin(src: string): string[] {
  const { text, bodies } = heredocs(src)
  const out = substitutions(text, true)
  for (const { body, quoted, shell } of bodies) {
    if (shell) out.push(body)
    if (!quoted) out.push(...substitutions(body, false))
  }
  return out
}

// A refspec's destination: `+HEAD:refs/heads/release/v9` -> release/v9; `release/v9` -> release/v9.
function destination(spec: string): string {
  const target = spec.replace(/^\+/, '').split(':').pop() ?? ''
  return target.replace(/^refs\/heads\//, '')
}

// What a git command should come to under a guard: deny what skips the pre-commit gate (denyNoVerify) and a push
// that lands on a protected branch (protectedPush); "check" when where a push lands depends on the repository (no
// refspec, HEAD, a configured push), which the hook settles by asking git; "ask" for any other push (the hook lets
// it through); null for nothing to say. No guard, or one that protects nothing: null, always.
// The tripwire runs first over the raw text, and the parse can only make its denial more precise; a parse that
// throws denies a command with git in it. Fail closed (astra r3).
export function gitVerdict(command: string, guard: Guard | null, cwd: string | null = null): GitVerdict {
  if (!guard || (!guard.denyNoVerify && !guard.protectedPush.length)) return null
  const tripped = tripwire(command, guard)
  let parsed: GitVerdict
  try {
    parsed = parsedVerdict(command, 0, cwd, guard)
  } catch {
    parsed = GIT_WORD.test(command) ? { kind: 'deny', reason: UNRESOLVED } : null
  }
  if (!tripped || parsed?.kind === 'deny') return parsed
  return { kind: 'deny', reason: `${tripped} ${AS_TEXT}` }
}

// The parse alone, under the tripwire: what the commands are, for the guard's own tests.
export const gitParse = (command: string, guard: Guard, cwd: string | null = null): GitVerdict => parsedVerdict(command, 0, cwd, guard)

// A directory a `cd` with $, ~ or - in it went to: unknown.
const UNKNOWN_DIR = '\u0000unknown'
const COMMITS = new Set(['commit', 'merge', 'push'])

function parsedVerdict(command: string, depth: number, cwd: string | null, guard: Guard): GitVerdict {
  const protects = guard.protectedPush.length > 0
  let worst: GitVerdict = null
  // The worst verdict wins; two checks are one with both pushes in it: each is asked of git (Codex review #6).
  const keep = (found: GitVerdict) => {
    if (found?.kind === 'check' && worst?.kind === 'check') worst = { kind: 'check', pushes: [...worst.pushes, ...found.pushes] }
    else if (found && (!worst || found.kind === 'deny' || (found.kind === 'check' && worst.kind === 'ask'))) worst = found
  }
  if (depth < 3) for (const text of scriptsWithin(command)) {
    const found = parsedVerdict(text, depth + 1, cwd, guard)
    if (found?.kind === 'deny') return found
    keep(found)
  }
  let here = cwd
  const set: Record<string, Value> = {} // `export DEST=...;` or `DEST=...;`: what the commands after it may read
  for (const tokens of splitWords(heredocs(command).text)) {
    const words = tokens.map(token => token.text)
    if (words[0] === 'cd') { // where the commands after it run: a relative one from where it was, $ ~ - unknown
      const to = tokens[1]
      if (!to || to.expands || /^[~-]/.test(to.text)) here = UNKNOWN_DIR
      else if (to.text.startsWith('/')) here = to.text
      else if (here && here !== UNKNOWN_DIR) here = `${here}/${to.text}`
    }
    const plain = words[0] === 'export' ? tokens.slice(1) : tokens
    if (plain.length && plain.every(w => ASSIGNMENT.test(w.text))) Object.assign(set, assigned(plain))
    for (const text of depth < 3 ? inner(words) : []) {
      const found = parsedVerdict(text, depth + 1, here, guard)
      if (found?.kind === 'deny') return found
      keep(found)
    }
    const call = gitCall(tokens)
    if (!call) continue
    const { sub, rest } = call
    const gate = guard.denyNoVerify
    if (gate && call.hooksOff && COMMITS.has(sub)) return { kind: 'deny', reason: HOOKS_OFF }
    const { env, unknown } = gitEnv(call.config, { ...set, ...call.env })
    // its settings unknown: a hooksPath could be among them, or where a push lands
    if ((gate ? COMMITS.has(sub) : protects && sub === 'push') && (call.configExpands || unknown)) return { kind: 'deny', reason: UNRESOLVED }
    if (gate && sub === 'commit' && gitOptions(rest, COMMIT_OPTIONS, COMMIT_SHORTS).skip) return { kind: 'deny', reason: NO_VERIFY }
    if (gate && sub === 'merge' && gitOptions(rest, MERGE_OPTIONS, MERGE_SHORTS).skip) return { kind: 'deny', reason: NO_VERIFY }
    if (sub !== 'push') continue
    const push = pushWords(rest)
    if (gate && push.skip) return { kind: 'deny', reason: NO_VERIFY }
    if (push.dry || !protects) continue // nothing is sent, or no branch is protected
    // every branch, a wildcard, or the matching refspec ':' (every branch both sides have, a protected one among them)
    if (push.every || push.specs.some(s => s.includes('*') || /^\+?:$/.test(s))) return { kind: 'deny', reason: anyProtected(guard) }
    const hit = push.specs.map(s => protectedBy(guard, destination(s))).find(Boolean)
    if (hit) return { kind: 'deny', reason: reasonFor(guard, hit) }
    if (!push.specs.length || push.specs.some(s => ['HEAD', '@'].includes(destination(s)))) {
      const absolute = call.dir?.startsWith('/') && !call.dirExpands
      if (call.dirExpands || (!absolute && here === UNKNOWN_DIR)) return { kind: 'deny', reason: UNRESOLVED } // where it runs: unknown
      const dir = call.dir ? (absolute || !here ? call.dir : `${here}/${call.dir}`) : here
      keep({ kind: 'check', pushes: [{ dir, remote: push.remote, config: call.config, ...(Object.keys(env).length ? { env } : {}),
        ...(push.specs.length ? {} : { implicit: true as const }) }] })
    }
    else keep({ kind: 'ask' })
  }
  return worst
}

// What a "check" push lands on, from git itself: the current branch, its upstream, and the push refspecs set for
// the remote it goes to. The reason to deny when any of them is a protected branch (or a refspec is a wildcard,
// which may be one), or null.
export function protectedFromGit(guard: Guard, branch: string, upstream: string, pushConfig: string): string | null {
  const hit = protectedBy(guard, branch.trim()) ?? protectedBy(guard, upstream.trim().replace(/^[^/]+\//, ''))
  if (hit) return reasonFor(guard, hit)
  for (const line of pushConfig.split('\n')) {
    const spec = line.trim().split(/\s+/).pop() ?? ''
    if (spec.includes('*')) return anyProtected(guard)
    const found = spec ? protectedBy(guard, destination(spec)) : null
    if (found) return reasonFor(guard, found)
  }
  return null
}
