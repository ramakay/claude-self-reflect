import { atom, read, update } from 'claude-code'
import type { EngineInterface, PluginOptions, Register } from 'claude-code'

import type { Change, ContractCard, Dream, Plan, Span, Summary, Tab, Usage } from '../types'
import { cardRows, columnCount, columnsOf, toCards } from './cards'
import type { Block, Card } from './cards'
import { currentFacts, refresh } from './engine'
import type { Seat } from './engine'
import { piecesOf, tokens } from './pieces'
import { HERDR_GRACE_MS, lookForHerdr, standsDown } from './herdr'
import type { HerdrSight, PaneChoice } from './herdr'
import type { Facts } from './transcript'
import type { Pieces } from './pieces'
import { cellsWanted, compacted, drawBand, drawCards, motionFor, noPictures, paletteFor, recalled, schemeFrom, setBanding, setMotion, setPlaced, shapeFor, wake } from './picture'
import type { Model, Setup } from './picture'
import type { Palette } from './theme'
import { css } from './theme'
import { COMMAND, DOCK_COLUMNS, FRAME_COLS, FRAME_ROWS, HIDE_KEY, INLINE_ROWS, PANE, TITLE } from './state'
import { barCells, GLYPH, labelRows, limitLine, sentence } from './usage'

// The atoms of the $.state contract (types/index.d.ts). Keep these in step with register.tsx/engine.ts.
const frameAtom = atom({ plugin: 'csr-pane', key: 'frame' } as const, null, { shape: 'frame-1' })
const summaryAtom = atom({ plugin: 'csr-pane', key: 'summary' } as const, null, { shape: 'summary-3' })
const usageAtom = atom({ plugin: 'csr-pane', key: 'usage' } as const, null, { shape: 'usage-2' })
const planAtom = atom({ plugin: 'csr-pane', key: 'plan' } as const, null, { shape: 'plan-1' })
const failureAtom = atom({ plugin: 'csr-pane', key: 'failure' } as const, null)
const placedAtom = atom({ plugin: 'csr-pane', key: 'isPlaced' } as const, false)
const hiddenAtom = atom({ plugin: 'csr-pane', key: 'isHidden' } as const, false)
const tabAtom = atom({ plugin: 'csr-pane', key: 'tab' } as const, 'now')

const MINUTE = 60_000
const WATCH_MS = 2000
// What the band keeps for its two Buttons: "[ Open pane ]" and "[ hide ]" with gaps.
const BAND_BUTTONS = 26

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']

// The most dreams the Dreams tab draws; the rest are counted in one line.
const MAX_DREAMS = 8
// The herdr pane's keys, where the mod has the data: l changes, the dreams, o and tap open the
// Same page? read and the should-be list (Notes), b the theme. The monologue (t) has no feed here.
const TABS: { tab: Tab; label: string; hotkey: string }[] = [
  { tab: 'now', label: 'Now', hotkey: '1' },
  { tab: 'dreams', label: 'Dreams', hotkey: '2' },
  { tab: 'changes', label: 'Changed', hotkey: '3' },
  { tab: 'notes', label: 'Notes', hotkey: '4' },
  { tab: 'figure', label: 'Figure', hotkey: '5' },
]
/** What the cards say opens their full text. */
const HINTS = { same: '4 opens', should: '4 opens' }

/** A day the engine wrote as 2026-11-10, in words: "Nov 10". */
export function dayWords(day: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(day)
  const month = m ? MONTHS[Number(m[2]) - 1] : undefined
  return m && month ? `${month} ${Number(m[3])}` : day
}

/** A dream's date in words: "Nov 14". */
export function dreamDate(at: number): string {
  const d = new Date(at * 1000)
  return `${MONTHS[d.getMonth()]} ${d.getDate()}`
}

/** A dream's answer in words. */
export function answerWords(answer: string | undefined): string {
  switch (answer) {
    // The Rust pane words these two: "marked it helpful", "handed it to Claude".
    case 'helpful':
      return 'You marked it helpful.'
    case 'sent':
      return 'You handed it to Claude.'
    case 'ignored':
      return 'You did not act on it.'
    case 'unhelpful':
    case 'not_helpful':
      return 'You found it not helpful.'
    case 'wrong':
      return 'You marked it wrong.'
    case undefined:
    case '':
    case 'pending':
    case 'none':
      return 'No answer yet.'
    default:
      return `Your answer: ${answer.replace(/_/g, ' ')}.`
  }
}

/** The band's sentence, cut to the room the Buttons leave. */
export function bandText(status: string, cols: number): string {
  const room = Math.max(8, cols - BAND_BUTTONS)
  return status.length <= room ? status : status.slice(0, room - 1).trimEnd() + '…'
}

// Self-Reflect's own tools, and the words its hooks put in what they bring.
const MEMORY_TOOL = /^mcp__(plugin_[a-z0-9-]+_)?claude-self-reflect__/
const MEMORY_WORDS = /\bCSR\b|Self-Reflect|csr_reflect_on_past/

/** How the person set the pictures up, from the plugin's options. */
export function setupOf(given: PluginOptions): Setup {
  const pick = (value: unknown, known: string[], otherwise: string) =>
    typeof value === 'string' && known.includes(value.trim()) ? value.trim() : otherwise
  const raw = typeof given.cellShape === 'string' ? given.cellShape.trim() : given.cellShape
  const shape = raw === '' || raw === 'auto' || raw === undefined ? NaN : Number(raw)
  // The braille drawing is gone; the cell view is its heir.
  const view = given.view === 'braille' ? 'cells' : pick(given.view, ['auto', 'picture', 'cells', 'text'], 'auto')
  return {
    view,
    theme: pick(given.theme, ['auto', 'dark', 'light'], 'auto'),
    cellShape: Number.isFinite(shape) && shape >= 1.2 && shape <= 4 ? shape : undefined,
    motion: pick(given.motion, ['auto', 'on', 'off'], 'auto'),
  }
}

/** The `pane` option: auto (the herdr pane where it runs, else this one), mod, or herdr. */
export function paneChoiceOf(given: PluginOptions): PaneChoice {
  const v = typeof given.pane === 'string' ? given.pane.trim() : ''
  return v === 'mod' || v === 'herdr' ? v : 'auto'
}

// Module state: the options the refresher runs with.
let options: PluginOptions = {}
// The terminal's ground and cell, read once; the theme the person flipped with b, if any.
let scheme: 'light' | 'dark' | undefined
let termProgram: string | undefined
let isForced = false
let isTermRead = false
let flipped: 'light' | 'dark' | undefined
// One pane per session: what the mod saw of the herdr pane, when the session started, and
// whether the person asked for this pane (an ask always wins over standing down).
let herdr: HerdrSight = { inHerdr: false, live: false }
let startedAt = 0
let isAsked = false

/** Whether the mod stays quiet now: no pane by itself, no band. */
async function quiet($: EngineInterface): Promise<boolean> {
  if (isAsked) return false
  return standsDown(paneChoiceOf(options), herdr, (await $.clock.now()) - startedAt)
}

/** Looks again for the herdr pane; opens this pane once the mod no longer stands down. */
async function lookAgain($: EngineInterface): Promise<void> {
  const sid = await $.session.id().catch(() => '')
  herdr = await lookForHerdr($, sid)
  if (await quiet($)) return
  const opened = await $.ui.open({ id: PANE, title: TITLE, columns: DOCK_COLUMNS, rows: INLINE_ROWS })
  await update($, placedAtom, () => opened.isPlaced)
}
// What the next run needs: 'full' (frame and summary) or 'json' (the band's status only).
let mode: 'full' | 'json' = 'full'

// The frame is asked at one fixed, wide size: its rows are paragraphs, and the pane wraps them.
function seat(): Seat | null {
  return mode === 'json' ? null : { cols: FRAME_COLS, rows: FRAME_ROWS }
}

/** Refreshes. Unforced, a pane that is not placed costs nothing, or one --json call when the band shows. */
async function kick($: EngineInterface, force = false): Promise<void> {
  if (force || (await read($, placedAtom))) mode = 'full'
  else if ((await read($, hiddenAtom)) || (await quiet($))) return
  else mode = 'json'

  await refresh($, options, seat)
  // The engine says whether the herdr pane runs for this session (schema 1): no pid file read.
  const live = (await read($, summaryAtom))?.herdrPaneLive
  if (live !== undefined && herdr.inHerdr) herdr = { ...herdr, live }
  await readAgents($)
  setPlaced(await read($, placedAtom))
}

/** Memory came into the session: the timeline's newest recall mark glows once the refresh has it. */
async function noteRecall($: EngineInterface): Promise<void> {
  recalled(await $.clock.now())
  await kick($)
  wake($)
}

// The subagents running now, by their task's words: the herdr pane's agents row.
let running: string[] = []

async function readAgents($: EngineInterface): Promise<void> {
  const list = await $.agent.list().catch(() => [])
  const now = list.filter(a => a.status === 'running').map(a => a.description || a.type)
  if (now.join('\n') !== running.join('\n')) {
    running = now
    $.ui.invalidate('ui.render')
  }
}

async function readTerminal($: EngineInterface): Promise<void> {
  if (isTermRead) return
  isTermRead = true
  scheme = schemeFrom(await $.env.get('COLORFGBG').catch(() => undefined))
  termProgram = await $.env.get('TERM_PROGRAM').catch(() => undefined)
  isForced = ((await $.env.get('CLAUDE_CODE_FORCE_TERMINAL_IMAGES').catch(() => undefined)) ?? '') === '1'
  setMotion(motionFor(setupOf(options).motion, termProgram))
  setBanding(termProgram !== 'iTerm.app')
}

async function openAsked($: EngineInterface) {
  isAsked = true
  // The person asked for it: `focus` brings its tab to the front where another pane is open too.
  const opened = await $.ui.open({ id: PANE, title: TITLE, columns: DOCK_COLUMNS, rows: INLINE_ROWS, focus: true })
  await update($, placedAtom, () => opened.isPlaced)
  return opened
}

// Follows a pane the person closed or opened by hand.
async function watch($: EngineInterface): Promise<void> {
  const panes = await $.ui.panes().catch(() => [])
  const placed = panes.some(p => p.id === PANE && p.isPlaced)
  if (placed === (await read($, placedAtom))) return
  await update($, placedAtom, () => placed)
  if (placed) await kick($)
}

async function hide($: EngineInterface): Promise<void> {
  await update($, hiddenAtom, () => true)
  await $.store.set(HIDE_KEY, true)
}

async function openFromBand($: EngineInterface): Promise<void> {
  await openAsked($)
  await kick($, true)
}

/** Writes a dream in the prompt box as a draft. The person sends it, or not. */
async function draft($: EngineInterface, text: string): Promise<void> {
  const filled = await $.prompt.fill({ text }).catch(() => undefined)
  if (!filled?.isFilled) $.ui.toast('Self-Reflect could not write in the prompt now.')
}

// The elements every surface draws. The pane uses no other.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Kit = { Box: any; Text: any; Button: any; Image?: any; Raster?: any; Svg?: any }
type Node = unknown

function spansOf(ui: Kit, spans: Span[]): Node {
  const { Text } = ui
  return spans.map(s => (
    <Text color={s.color} bold={s.bold} dimColor={s.dim}>
      {s.text}
    </Text>
  ))
}

/** An engine contract card in words: its title (and tag), its lines, each bar as "label text". */
export function contractText(c: ContractCard): Card {
  const para = (text: string, dim = false): Span[] => [{ text, dim }]
  return {
    title: [c.title, c.tag].filter(Boolean).join(' · '),
    paras: [
      ...c.lines.map(l => para(l.text, l.style === 'dim' || l.style === 'caption')),
      ...c.rows.map(r => para(`${r.label} ${r.text}`)),
      ...(c.ring ? [para(`dreamer ${tokens(c.ring.spent)} of ${tokens(c.ring.cap)}`, true)] : []),
    ],
  }
}

/** One engine card: a round frame, the heading, then each paragraph wrapped to the pane. */
function cardBlock(ui: Kit, card: Card, width: number): Block<Node> {
  const { Box, Text } = ui
  return {
    rows: cardRows(card, width - 4),
    node: (
      <Box flexDirection="column" borderStyle="round" borderDimColor paddingX={1} width={width}>
        {card.title !== '' && (
          <Text bold wrap="truncate-end">
            {card.title}
          </Text>
        )}
        {card.paras.map((para, i) => (
          <Text wrap="wrap" bold={card.title === '' && i === 0}>
            {spansOf(ui, para)}
          </Text>
        ))}
      </Box>
    ),
  }
}

/** The context card: Claude Code's own numbers as a sentence, a bar and its labels. */
function contextBlock(
  ui: Kit,
  usage: Usage | null,
  hasFrame: boolean,
  width: number,
  now: number,
  isShort: boolean,
): Block<Node> | null {
  const { Box, Text } = ui
  const inner = Math.max(8, width - 4)
  const segments = usage?.segments ?? []
  const hasContext = !!usage && usage.max > 0 && segments.length > 0
  // The engine's cost card already carries the weekly limit: draw ours only without a frame.
  const limits = hasFrame ? [] : (usage?.limits ?? [])
  if (!hasContext && limits.length === 0) return null
  const cells = barCells(segments, inner)
  // Above the prompt the rows are few: the sentence and the bar, without a label for each part.
  const labels = isShort ? [] : labelRows(segments, inner)

  return {
    rows: 3 + (hasContext ? 2 + labels.length : 0) + limits.length,
    node: (
      <Box flexDirection="column" borderStyle="round" borderDimColor paddingX={1} width={width}>
        <Text bold>Context</Text>
        {hasContext && usage && <Text wrap="wrap">{sentence(usage)}</Text>}
        {hasContext && (
          <Text wrap="truncate-end">
            {segments.map((s, i) => (
              <Text color={s.color}>{(GLYPH[s.kind] ?? '█').repeat(cells[i] ?? 0)}</Text>
            ))}
          </Text>
        )}
        {hasContext &&
          labels.map(row => (
            <Text wrap="truncate-end">
              {row.map((l, i) => (
                <Text>
                  {i > 0 ? ' · ' : ''}
                  <Text color={l.color}>{l.text}</Text>
                </Text>
              ))}
            </Text>
          ))}
        {limits.map(l => (
          <Text wrap="wrap">{limitLine(l, now)}</Text>
        ))}
      </Box>
    ),
  }
}

/**
 * A card's frame. In words: a round border. Where cells stand in for pictures: the Now tab's card,
 * a ground row above, a lighter top row, the card's flat fill (the Now tab's cells, in Text).
 */
function shell(ui: Kit, width: number, cells: Palette | undefined, children: Node): Node {
  const { Box } = ui
  if (!cells) {
    return (
      <Box flexDirection="column" borderStyle="round" borderDimColor paddingX={1} width={width}>
        {children}
      </Box>
    )
  }
  return (
    <Box flexDirection="column" width={width}>
      <Box height={1} width={width} backgroundColor={css(cells.cells.ground)} />
      <Box height={1} width={width} backgroundColor={css(cells.cells.top)} />
      <Box flexDirection="column" paddingX={1} paddingBottom={1} width={width} backgroundColor={css(cells.cells.card)}>
        {children}
      </Box>
    </Box>
  )
}

/** One dream: its title, the what-if, the answer in words, and a Button that drafts it. */
function dreamBlock($: EngineInterface, ui: Kit, dream: Dream, at: number, width: number, cells?: Palette): Block<Node> {
  const { Box, Text, Button } = ui
  const inner = Math.max(8, width - 4)
  const ink = cells ? css(cells.ink) : undefined
  const quietInk = cells ? css(cells.quiet) : undefined

  return {
    rows: 5 + Math.ceil((dream.line.length + 1) / inner) + (dream.title ? 1 : 0),
    node: shell(
      ui,
      width,
      cells,
      <Box flexDirection="column">
        {dream.title && (
          <Text bold wrap="wrap" color={ink}>
            {dream.title}
          </Text>
        )}
        <Text wrap="wrap" color={ink}>
          {dream.line}
        </Text>
        <Text dimColor={!cells} wrap="wrap" color={quietInk}>{`${dreamDate(dream.at)} · ${answerWords(dream.answer)}`}</Text>
        <Button
          key={`draft-${at}`}
          label="Put in prompt"
          onPress={() => draft($, dream.line)}
        />
      </Box>,
    ),
  }
}

/** One past session whose code changed since: its title, when, and what changed. */
function changeBlock(ui: Kit, change: Change, width: number, cells?: Palette): Block<Node> {
  const { Box, Text } = ui
  const commits = `${change.commits} commit${change.commits === 1 ? '' : 's'} since`
  const ink = cells ? css(cells.ink) : undefined

  return {
    rows: 4 + change.shifts.length,
    node: shell(
      ui,
      width,
      cells,
      <Box flexDirection="column">
        <Text bold wrap="truncate-end" color={ink}>
          {change.title}
        </Text>
        <Text dimColor={!cells} wrap="truncate-end" color={cells ? css(cells.quiet) : undefined}>{`Talked ${dayWords(change.talked)} · ${commits}`}</Text>
        {change.shifts.map(s => (
          <Text wrap="truncate-end" color={cells ? css(cells.mint) : undefined}>{`${s.verb} ${s.symbol} · ${s.file.split('/').pop() ?? s.file}`}</Text>
        ))}
      </Box>,
    ),
  }
}

function note(ui: Kit, text: string): Block<Node> {
  const { Text } = ui
  return {
    rows: 1,
    node: (
      <Text dimColor wrap="wrap">
        {text}
      </Text>
    ),
  }
}

/**
 * The transcript's facts with Claude Code's own reading of the context now as the last point, so
 * a compaction's drop shows as soon as it lands, before the next reply is written.
 */
export function withLive(facts: Facts | null, usage: Usage | null, now: number): Facts | null {
  if (!facts || !usage || usage.used <= 0) return facts
  const last = facts.replies[facts.replies.length - 1]
  if (last && (last[0] >= now || last[1] === usage.used)) return facts
  // A compaction after the last reply: the line holds until it, then drops.
  const cut = facts.compacts[facts.compacts.length - 1]
  const hold: [number, number][] = last && cut !== undefined && cut > last[0] && cut < now ? [[cut, last[1]], [cut + 1, usage.used]] : []
  return { ...facts, replies: [...facts.replies, ...hold, [now, usage.used]] }
}

/** The rows the tab Buttons take at `cols`: "1: Now", two columns apart, wrapped. */
export function tabRows(cols: number): number {
  const widths = [...TABS.map(t => `${t.hotkey}: ${t.label}`.length), 'b: theme'.length]
  let rows = 1
  let used = 0
  for (const w of widths) {
    if (used > 0 && used + 2 + w > cols) {
      rows++
      used = 0
    }
    used += (used > 0 ? 2 : 0) + w
  }
  return rows
}

/** The timeline in words, where no picture shows: what the figure would draw. */
function figureWords(facts: ReturnType<typeof currentFacts>): string {
  if (!facts || facts.replies.length === 0) return 'This session has no reply yet: the timeline starts with the first one.'
  const peak = Math.max(...facts.replies.map(r => r[1]))
  const last = facts.replies[facts.replies.length - 1]?.[1] ?? 0
  return `This session: ${facts.prompts.length} prompts, ${facts.replies.length} replies, ${facts.compacts.length} compactions, ${facts.recalls.length} recalls. Context now ${Math.round(last / 1000)}k, peak ${Math.round(peak / 1000)}k.`
}

/** The herdr pane's b: the cards in the other theme, until the session ends. */
function flipTheme($: EngineInterface): void {
  const now = paletteFor(flipped ?? setupOf(options).theme, scheme).name
  flipped = now === 'dark' ? 'light' : 'dark'
  $.ui.invalidate('ui.render')
}

/** The Notes tab: what the Same page? and Should be cards cut short, and Lost and found, in full. */
function notesBlocks(ui: Kit, pieces: Pieces, width: number, cells?: Palette): Block<Node>[] {
  const { Box, Text } = ui
  const out: Block<Node>[] = []
  const inner = Math.max(8, width - 4)
  const rowsOf = (texts: string[]) => texts.reduce((n, t) => n + Math.max(1, Math.ceil(t.length / inner)), 0)
  const card = (heading: string, texts: { text: string; dim?: boolean; color?: string }[]): Block<Node> => ({
    rows: 3 + rowsOf(texts.map(t => t.text)),
    node: shell(
      ui,
      width,
      cells,
      <Box flexDirection="column">
        <Text bold color={cells ? css(cells.ink) : undefined}>
          {heading}
        </Text>
        {texts.map(t => (
          <Text wrap="wrap" dimColor={t.dim && !cells} color={t.color ?? (cells ? css(t.dim ? cells.quiet : cells.ink) : undefined)}>
            {t.text}
          </Text>
        ))}
      </Box>,
    ),
  })
  if (pieces.samePage) out.push(card('Same page?', [{ text: pieces.samePage.read }, { text: pieces.samePage.more, dim: true }]))
  if (pieces.shouldBe.length > 0) {
    out.push(card(`Should be a skill / memory / tool · ${pieces.shouldBe.length}`, pieces.shouldBe.map(s => ({ text: `${s.kind}: ${s.text}` }))))
  }
  if (pieces.lost.length > 0) out.push(card('Lost and found', pieces.lost.map(text => ({ text }))))
  return out
}

export const register: Register = (on, given) => {
  options = given

  on('session.start', async ($, e, next) => {
    await $.command.register({ name: COMMAND, description: 'Open the Self-Reflect pane and refresh it' })
    const hidden = (await $.store.get(HIDE_KEY).catch(() => undefined)) === true
    await update($, hiddenAtom, () => hidden)
    startedAt = await $.clock.now()
    isAsked = false
    herdr = await lookForHerdr($, await $.session.id().catch(() => '')).catch(() => ({ inHerdr: false, live: false }))
    if (await quiet($)) {
      // Inside herdr: the herdr pane is live or may still be opening. Look again after the grace.
      if (paneChoiceOf(options) === 'auto') $.clock.after(HERDR_GRACE_MS + 500, () => void lookAgain($).catch(() => undefined))
    } else {
      const opened = await $.ui.open({ id: PANE, title: TITLE, columns: DOCK_COLUMNS, rows: INLINE_ROWS })
      await update($, placedAtom, () => opened.isPlaced)
    }
    $.clock.after(0, () => void kick($))
    $.clock.every(MINUTE, () => void kick($))
    $.clock.every(WATCH_MS, () => void watch($))

    return next(e)
  })

  on('command.run', { command: 'csr-pane' }, async $ => {
    try {
      const opened = await openAsked($)
      await kick($, true)
      if (!opened.isPlaced) return { text: `Self-Reflect pane is waiting: ${opened.reason}` }
      const failure = await read($, failureAtom)

      return { text: failure ? `Self-Reflect pane is open. ${failure.text}` : 'Self-Reflect pane is open.' }
    } catch {
      return { text: 'Self-Reflect pane could not open.' }
    }
  })

  on('turn.complete', async ($, e, next) => {
    void kick($)

    return next(e)
  })

  // What a hook of Self-Reflect's brings with a prompt is a recall: a mark on the timeline. The
  // refresh runs on its own; the prompt never waits for it.
  on('classic.UserPromptSubmit', async ($, e, next) => {
    const result = await next(e)
    if ((result.additionalContext ?? []).some(text => MEMORY_WORDS.test(text))) void noteRecall($).catch(() => undefined)

    return result
  })

  // A search of memory is a recall too, once it has answered. The tool's result never waits.
  on('tool.call', async ($, e, next) => {
    const ran = await next(e)
    if (MEMORY_TOOL.test(e.tool) && ran.deny === undefined) void noteRecall($).catch(() => undefined)

    return ran
  })

  // A subagent started or ended: the agents row follows.
  on('agent.spawn', async ($, e, next) => {
    const spawned = await next(e)
    void kick($).catch(() => undefined)

    return spawned
  })

  // A compaction: its drop draws in on the timeline once the refresh has read it.
  on('session.compact', async ($, e, next) => {
    try {
      return await next(e)
    } finally {
      void (async () => {
        compacted(await $.clock.now())
        await kick($)
        wake($)
        // The transcript gets its compaction line a moment after the hook: read it again then.
        $.clock.after(2500, () => void kick($).then(() => wake($)).catch(() => undefined))
      })().catch(() => undefined)
    }
  })

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    if (e.props.hasSurvey || (await read($, hiddenAtom)) || (await read($, placedAtom)) || (await quiet($))) return next(e)
    const { Box, Button, Text } = $.ui.resolve(e)
    const summary = await read($, summaryAtom)
    const status = summary?.status || 'Self-Reflect has a pane for this session.'

    return (
      <Box flexDirection="row">
        <Text wrap="truncate-end">{bandText(status, e.props.bodyColumns)} </Text>
        <Button
          key="open"
          label="Open pane"
          hotkey="o"
          onPress={() => openFromBand($)}
        />
        <Text> </Text>
        <Button
          key="hide"
          label="hide"
          onPress={() => hide($)}
        />
      </Box>
    )
  })

  on('ui.render', { component: 'Pane', requestId: 'self-reflect' }, async ($, e) => {
    const ui: Kit = $.ui.resolve(e)
    const { Box, Button, Text } = ui
    const cols = Math.max(24, e.props.bodyColumns)
    const [frame, summary, usage, plan, failure, tab] = await Promise.all([
      read($, frameAtom),
      read($, summaryAtom),
      read($, usageAtom),
      read($, planAtom),
      read($, failureAtom),
      read($, tabAtom),
    ])
    const now = await $.clock.now()
    const isInline = e.props.placement === 'inline'
    const facts = withLive(currentFacts(), usage, now)
    const pieces = piecesOf(frame?.lines ?? null, frame?.cols ?? FRAME_COLS, usage, plan, summary, facts, now)
    // Beside the chat the cards stack; above the prompt, where rows are few, they sit side by side
    // as text: the painted cards need the height a dock has.
    const count = columnCount(e.props.placement, cols)
    const width = Math.floor((cols - (count - 1)) / count)

    // The engine's own cards (schema 1): the pane has its words without the text frame.
    const contractCards = (summary?.schema ?? 0) >= 1 && summary?.cards?.length ? summary.cards : null
    const hasCards = !!frame || !!contractCards
    let pictures: { id: string; node: unknown; rows: number }[] | undefined
    await readTerminal($)
    const setup = setupOf(options)
    const palette = paletteFor(flipped ?? setup.theme, scheme)
    if ((tab === 'now' || tab === 'figure') && !isInline && hasCards) {
      const model: Model = { pieces, facts, hints: HINTS, agents: running }
      pictures = await drawCards(
        $,
        { surface: e.surface, pane: e.requestId ?? PANE, bodyColumns: cols, bodyRows: e.props.scroll.bodyRows - tabRows(cols - 2), page: tab === 'figure' ? 'figure' : 'now', elements: ui, term: termProgram, forced: isForced },
        model,
        now,
        setup,
        palette,
        setup.cellShape ?? shapeFor(termProgram),
      ).catch(() => undefined)
    }
    // Where cells stand in for pictures (Warp), every tab shares their ground (seen in Warp: Dreams
    // and Changed sat on the terminal's own ground while Notes and Now had the cell ground).
    const isCells = e.surface === 'terminal' && cellsWanted(setup.view, noPictures() ? false : undefined, termProgram, isForced, !!ui.Image)
    // Above the prompt, where cells are wanted: the compact band, not the cards in words.
    let band: unknown | undefined
    if (tab === 'now' && isInline && hasCards) {
      band = drawBand(
        { surface: e.surface, pane: e.requestId ?? PANE, bodyColumns: cols, bodyRows: e.props.scroll.bodyRows, page: 'now', elements: ui, term: termProgram, forced: isForced },
        { pieces, facts, hints: HINTS, agents: running },
        setup,
        palette,
        setup.cellShape ?? shapeFor(termProgram),
      )
    }

    const blocks: Block<Node>[] = []
    if (tab === 'dreams') {
      const dreams = summary?.dreams ?? []
      dreams.slice(0, MAX_DREAMS).forEach((d, i) => blocks.push(dreamBlock($, ui, d, i, width, isCells ? palette : undefined)))
      if (dreams.length > MAX_DREAMS) blocks.push(note(ui, `${dreams.length - MAX_DREAMS} older dreams are not shown.`))
      if (dreams.length === 0) blocks.push(note(ui, summary ? 'No dreams yet.' : 'Self-Reflect is reading.'))
    } else if (tab === 'changes') {
      const changes = summary?.changes ?? []
      const total = summary?.changesTotal ?? changes.length
      if (changes.length > 0) {
        blocks.push(note(ui, `Code from ${total} past session${total === 1 ? '' : 's'} changed after you talked about it.`))
      }
      changes.forEach(c => blocks.push(changeBlock(ui, c, width, isCells ? palette : undefined)))
      if (changes.length === 0) blocks.push(note(ui, summary ? 'No code under your past sessions changed.' : 'Self-Reflect is reading.'))
    } else if (tab === 'figure') {
      if (!pictures || pictures.length === 0) blocks.push(note(ui, figureWords(facts)))
    } else if (tab === 'notes') {
      notesBlocks(ui, pieces, width, isCells ? palette : undefined).forEach(b => blocks.push(b))
      if (blocks.length === 0) blocks.push(note(ui, hasCards ? 'No notes for this session yet.' : 'Self-Reflect is reading.'))
    } else if (band) {
      // The band says it.
    } else if (!pictures) {
      // No pictures here (or text chosen): the engine's own cards in words, and the context card.
      const context = contextBlock(ui, usage, hasCards, width, now, isInline)
      if (context) blocks.push(context)
      if (contractCards) contractCards.forEach(c => blocks.push(cardBlock(ui, contractText(c), width)))
      else if (frame) toCards(frame.lines, frame.cols).forEach(c => blocks.push(cardBlock(ui, c, width)))
      if (pieces.csr) blocks.push(note(ui, `Self-Reflect this session: ${pieces.csr}`))
      if (running.length > 0) blocks.push(note(ui, `${running.length} running · ${running[0]}`))
      if (!hasCards && !failure) blocks.push(note(ui, 'Self-Reflect is reading.'))
    }
    if (failure) {
      blocks.push(note(ui, hasCards ? failure.text : `${failure.text} Check that csr-engine is installed, then run /${COMMAND}.`))
    }

    // The tab row in the cards' own palette: on the pane's ground, the open tab in ink, the rest
    // quiet, the hotkeys in the terminal's accent.
    const isPainted = !!pictures && pictures.length > 0
    const tabs = (
      <Box flexDirection="row" flexWrap="wrap" columnGap={2} paddingX={1} backgroundColor={isPainted ? css(palette.mesh) : undefined}>
        {TABS.map(t => (
          <Button
            key={`tab-${t.tab}`}
            label={t.label}
            hotkey={t.hotkey}
            plain
            onPress={() => update($, tabAtom, () => t.tab)}
          >
            <Text color={css(tab === t.tab ? palette.gold : palette.quiet)} bold={tab === t.tab}>
              {t.label}
            </Text>
          </Button>
        ))}
        <Button
          key="theme"
          label="theme"
          hotkey="b"
          plain
          onPress={() => flipTheme($)}
        >
          <Text color={css(palette.faint)}>theme</Text>
        </Button>
      </Box>
    )
    const cards = (
      <Box flexDirection="row" columnGap={1}>
        {columnsOf(blocks, count).map(column => (
          <Box flexDirection="column" width={width}>
            {column.map(b => b.node)}
          </Box>
        ))}
      </Box>
    )

    return (
      <Box flexDirection="column">
        {tabs}
        {pictures && (
          <Box flexDirection="column" width={cols}>
            {pictures.map(p => p.node)}
          </Box>
        )}
        {band}
        {blocks.length > 0 && (isCells ? <Box flexDirection="column" backgroundColor={css(palette.cells.ground)}>{cards}</Box> : cards)}
        {!pictures && noPictures() && setup.view === 'picture' && <Text dimColor>Pictures do not show here: the cards are in words. Choose view cells for the painted cards.</Text>}
      </Box>
    )
  })
}
