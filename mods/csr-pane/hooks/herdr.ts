// One Self-Reflect pane per session. Setup installs both panes: the herdr pane (drawn by the
// engine in a herdr split, opened by the csr-companion plugin) and this mod's pane. Inside herdr,
// where the herdr pane is live or about to open, the mod stays quiet: it opens no pane by itself
// and draws no band above the prompt. A `/csr-pane` the person types still opens it.
import type { EngineInterface } from 'claude-code'

/** The `pane` option: who shows Self-Reflect's pane in this session. */
export type PaneChoice = 'auto' | 'mod' | 'herdr'

/** How long, after a session starts inside herdr, the mod waits for the herdr pane to appear. */
export const HERDR_GRACE_MS = 20_000

/** What the mod knows of the herdr pane for this session. */
export type HerdrSight = {
  /** This session runs inside a herdr pane (its variables are set). */
  inHerdr: boolean
  /** The herdr CSR pane for this session is running: its pid file names a live csr-engine. */
  live: boolean
}

/**
 * Whether the mod stands down: `herdr` always, `mod` never; `auto` inside herdr while the herdr
 * pane is live, or while it may still be opening (the first `HERDR_GRACE_MS` of the session).
 */
export function standsDown(choice: PaneChoice, sight: HerdrSight, sinceStart: number): boolean {
  if (choice === 'herdr') return true
  if (choice === 'mod') return false
  if (!sight.inHerdr) return false
  return sight.live || sinceStart < HERDR_GRACE_MS
}

/** The pid in a herdr pane's pid file (`{"pid":77287}`), or undefined. */
export function pidOf(text: string): number | undefined {
  try {
    const pid = (JSON.parse(text) as { pid?: unknown }).pid
    return typeof pid === 'number' && Number.isInteger(pid) && pid > 1 ? pid : undefined
  } catch {
    return undefined
  }
}

/**
 * Looks: herdr's variables in this session's environment, then the herdr pane's pid file for
 * this session (`~/.claude-self-reflect/csr-companion/panes/<sid>.json`, written by the pane and
 * removed when it exits; old files can stay behind, so the pid must still be a csr-engine).
 */
export async function lookForHerdr($: EngineInterface, sid: string): Promise<HerdrSight> {
  // Each name spelled out: the engine lists what a mod reads from the environment.
  const values = await Promise.all([
    $.env.get('HERDR_PANE_ID').catch(() => undefined),
    $.env.get('HERDR_SESSION').catch(() => undefined),
    $.env.get('HERDR_SOCKET_PATH').catch(() => undefined),
  ])
  const inHerdr = values.some(v => typeof v === 'string' && v !== '')
  if (!inHerdr || sid === '') return { inHerdr, live: false }
  const home = (await $.env.get('HOME').catch(() => undefined)) ?? ''
  const text = home === '' ? '' : await $.fs.read(`${home}/.claude-self-reflect/csr-companion/panes/${sid}.json`).catch(() => '')
  const pid = pidOf(typeof text === 'string' ? text : '')
  if (pid === undefined) return { inHerdr, live: false }
  // Read-only: what command that pid runs now, if any.
  const out = await $.process.run(['/bin/ps', '-p', String(pid), '-o', 'command='], { timeoutMs: 2000 }).catch(() => undefined)
  return { inHerdr, live: !!out && out.exitCode === 0 && /csr-engine/.test(out.stdout) }
}
