// When the mod may tell the session's story again on its own (CSR_STORY_REWRITES=1 only): the Shift Map's automatic
// refresh. One Haiku call each time, so four gates, each decided without a model call from what bin/pane.py
// --story-news reads:
//  1. News. At least 4 new receipts (2 a file your edits touched, 1 a symbol they added or removed, 3 a commit)
//     and at least 2 prompts of yours since the map's current telling. Without both nothing ever starts.
//  2. Spacing. A 2 hour chapter allows 3 refreshes, 1 more for each shift you liked in it, 6 at most. The
//     messages since the last telling must reach what the news earns: 80 usual messages to a chapter, shared
//     over the refreshes left, more when the news is thin (never under 12).
//  3. A share of the work. What story calls spent in the chapter, plus the guess for the next (the last one, else
//     1600 tokens), stays within 2 tokens for every 100 Claude spends. Where the mod has not counted Claude's
//     tokens (dreams off), this gate has nothing to compare and the caps below stand alone.
//  4. Caps. 60 seconds between tellings, 40 calls and $0.10 in a day. A day's spend that is unknown (a model
//     with no price, or a ledger that could not be read) stops the refresh; the person's own button is unchanged.
// The first reason a gate fails is the answer, so a test (and a person asking why) can read it.

export const NEWS_RECEIPTS = 4
export const NEWS_PROMPTS = 2
export const CHAPTER_MS = 2 * 60 * 60 * 1000
export const BASE_REFRESHES = 3
export const MOST_REFRESHES = 6
export const USUAL_MESSAGES = 80
export const LEAST_MESSAGES = 12
export const STORY_SHARE = 0.02
export const FIRST_GUESS = 1600
export const COOLDOWN_MS = 60_000
export const DAY_CALLS = 40
export const DAY_USD = 0.1

// What `pane.py --story-news` says. story false: no map yet (or the kill switch), so nothing to refresh.
export type StoryNews = {
  story: boolean
  at?: number // seconds: when the current telling was written
  messages?: number
  messages_since?: number
  prompts_since?: number
  receipts?: number
  calls_24h?: number
  usd_24h?: number | null // null: a call's model has no price
  tokens_2h?: number // what story calls spent in the last two hours
  last_tokens?: number | null
  liked_2h?: number
  transcript?: string | null
}

export type Why = 'go' | 'no-story' | 'no-transcript' | 'no-news' | 'cooldown' | 'chapter-cap' | 'spacing' | 'share' | 'spend-unknown' | 'day-cap'

export const parseNews = (text: string | null): StoryNews => {
  try {
    const value = JSON.parse(text ?? '')
    return value && typeof value === 'object' && value.story === true ? (value as StoryNews) : { story: false }
  } catch {
    return { story: false }
  }
}

const count = (value: unknown) => (typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : 0)

export const allowance = (liked: number) => Math.min(MOST_REFRESHES, BASE_REFRESHES + Math.min(MOST_REFRESHES - BASE_REFRESHES, count(liked)))

// Messages since the last telling that the news earns a refresh at: the usual chapter over the refreshes left, scaled
// by how thin the news is (4 receipts: the most; 12 or more: the least).
export function messagesDue(receipts: number, left: number): number {
  return Math.max(LEAST_MESSAGES, Math.round((USUAL_MESSAGES / Math.max(1, left)) * (6 / Math.min(12, Math.max(1, receipts)))))
}

// starts: when (ms) each automatic refresh was started this session. claude: tokens the mod counted Claude spending
// (0 when it did not count them).
export function decide(news: StoryNews, starts: readonly number[], now: number, claude: number): Why {
  if (!news.story || news.at === undefined) return 'no-story'
  if (!news.transcript) return 'no-transcript'
  const receipts = count(news.receipts)
  if (receipts < NEWS_RECEIPTS || count(news.prompts_since) < NEWS_PROMPTS) return 'no-news'
  if (starts.some(at => now - at < COOLDOWN_MS) || now - news.at * 1000 < COOLDOWN_MS) return 'cooldown'
  const recent = starts.filter(at => now - at < CHAPTER_MS).length
  const left = allowance(count(news.liked_2h)) - recent
  if (left <= 0) return 'chapter-cap'
  if (count(news.messages_since) < messagesDue(receipts, left)) return 'spacing'
  if (claude > 0) {
    const spent = count(news.tokens_2h)
    const next = count(news.last_tokens) || FIRST_GUESS
    if (spent + next > STORY_SHARE * claude) return 'share'
  }
  // Spend that cannot be known is not free: an unpriced model, or a ledger pane.py could not read, holds the refresh back.
  if (typeof news.usd_24h !== 'number' || !Number.isFinite(news.usd_24h)) return 'spend-unknown'
  if (count(news.calls_24h) >= DAY_CALLS || news.usd_24h >= DAY_USD) return 'day-cap'
  return 'go'
}

// The refresh as an argv list: csr-engine tells this conversation's story again, and skips it itself when nothing was
// asked since the last telling.
export const refreshArgv = (transcript: string, cwd: string) => ['csr-engine', 'generate-story', '--transcript', transcript, '--cwd', cwd, '--rewrite']
