import type { Register } from 'claude-code'

// fast-jev-compaction (MIT, tamaratran/fast-jev-compaction) does the compaction work: it pairs every tool call
// with its result, asks two yes/no questions per old call over the whole conversation, and deletes or truncates
// what is no longer needed. Nothing is rewritten. Its adapter takes an injectable fetch, so the only change
// here is WHO answers: a rule in-process by default (ask.ts ruleReply), or a local System One endpoint instead
// of api.typesafe.ai. Not vendored into this repo; see the README for the one-line clone into vendor/.
import {
  compactSession,
  decisionLogLines,
  resolveHookConfig,
  summarize,
} from '../vendor/fast-jev-compaction/hooks/fast-jev.js'
import { reductionRatio } from '../vendor/fast-jev-compaction/src/index.js'

import { DEFAULT_URL, REACTION, parseReply, requestInit, ruleReply } from './ask.js'

// csr-decide, a spike: typed decisions from a LOCAL endpoint at two points of the session lifecycle.
//
// prompt.submit (shadow mode, injects nothing): label how the user's new turn reacts to the assistant's last
// turn. Why here and not in the shell hook: the engine's reaction labeler only ever saw the user text
// (csr-engine/src/daemon/trained_rerank.rs embeds next_user_text and the previous user turn), a third of what
// lands in the "user" slot of a transcript is harness text, and one user turn is stored once per assistant
// entry. Here e.text is what the human typed, $.session.messages() holds the assistant turn, and the hook
// fires once per prompt.
//
// session.compact: truncate old tool results instead of summarizing them. No model and no endpoint by default.
//
// With the endpoint down, slow, or answering garbage, both hooks pass the event on untouched.

// A local decision answers in ~40-100 ms; this only bounds a wedged endpoint. Under the engine's 10 s budget.
const BUDGET_MS = 1500
const TAIL_CHARS = 600
const TURN_CHARS = 1200
// Below this the local answer is treated as "not sure" and, only when CSR_DECIDE_ESCALATE=1, the same question
// goes to the session's own small model through $.model.classify for comparison.
const ESCALATE_BELOW = 0.6
const LABELS = ['correction', 'acceptance', 'none'] as const

const TRUNCATED = '[fast-jev-compaction truncated'
// A wrong drop should cost one recall, not the fact: the full output is still in the transcript CSR indexes.
const POINTER =
  ' The full output is still in this session\'s transcript: claude-self-reflect csr_transcript (view "tools") returns it.'

const squash = (s: string) => s.replace(/\s+/g, ' ').trim()

export const register: Register = (on, options) => {
  const configured = resolveHookConfig(options)

  on('prompt.submit', async ($, e, next) => {
    const text = squash(e.text)
    if (!text || text.startsWith('/')) return next(e)

    const t0 = Date.now()
    try {
      const messages = await $.session.messages()
      let tail = ''
      for (let i = messages.length - 1; i >= 0; i--) {
        const m = messages[i]
        if (m.role === 'assistant' && m.text.trim()) {
          tail = squash(m.text).slice(-TAIL_CHARS)
          break
        }
      }
      const tMessages = Date.now() - t0
      // First prompt of a session: there is no assistant turn to react to.
      if (!tail) {
        $.ui.log(`csr-decide: prompt.submit no assistant turn yet, skipped (messages ${tMessages}ms)`)
        return next(e)
      }

      const state = `assistant_tail: ${tail}\nuser_turn: ${text.slice(0, TURN_CHARS)}`
      const url = (await $.env.get('CSR_DECIDE_URL')) || DEFAULT_URL
      const t1 = Date.now()
      const r = await Promise.race([
        $.http.fetch(url, requestInit(state, REACTION)),
        $.clock.sleep(BUDGET_MS).then(() => undefined),
      ])
      const ms = Date.now() - t1
      const reply = r === undefined ? undefined : parseReply(r.ok, r.text)
      const a = reply?.answers.reaction
      if (!a || !a.choice) {
        $.ui.log(`csr-decide: prompt.submit endpoint gave no answer in ${ms}ms, passed on untouched`)
        return next(e)
      }
      const p = a.probabilities ?? {}
      $.ui.log(
        `csr-decide: prompt.submit reaction=${a.choice} conf=${(a.confidence ?? 0).toFixed(2)} ` +
          `p=${LABELS.map(l => `${l}:${(p[l] ?? 0).toFixed(2)}`).join(',')} ` +
          `http=${ms}ms engine=${Math.round(reply?.timing_ms?.total ?? -1)}ms messages=${tMessages}ms n=${messages.length}`,
      )

      // 'always' is a proof aid: raw logit confidences saturate near 1.0, so the threshold rarely trips uncalibrated.
      const escalate = await $.env.get('CSR_DECIDE_ESCALATE')
      if (escalate === 'always' || (escalate === '1' && (a.confidence ?? 0) < ESCALATE_BELOW)) {
        const t2 = Date.now()
        const label = await $.model.classify(state, LABELS)
        $.ui.log(
          `csr-decide: escalated local=${a.choice} model=${label ?? 'undefined'} agree=${label === a.choice} in ${Date.now() - t2}ms`,
        )
      }

      // Transport baseline: the proven $.mcp.call round trip to CSR's MCP server, for the same prompt.
      if ((await $.env.get('CSR_DECIDE_PROBE')) === '1') {
        const t3 = Date.now()
        const probe = await Promise.race([
          $.mcp.call('claude-self-reflect', 'csr_reflect_on_past', { query: text.slice(0, 200), limit: 1, min_score: 0.9 }),
          $.clock.sleep(6000).then(() => undefined),
        ])
        $.ui.log(
          `csr-decide: probe mcp.call ${probe === undefined ? 'timed out' : probe.isError ? 'error' : 'ok'} in ${Date.now() - t3}ms vs http ${ms}ms`,
        )
      }
    } catch (err) {
      $.ui.log(`csr-decide: prompt.submit ${String(err)}`)
    }
    return next(e)
  })

  on('session.compact', async ($, e, next) => {
    const t0 = Date.now()
    try {
      // Default is the rule: untuned local deciders scored below it against a blind judge (README). The endpoint
      // stays reachable with CSR_DECIDE_COMPACT=model, for when a decider earns it.
      const useModel = (await $.env.get('CSR_DECIDE_COMPACT')) === 'model'
      const url = (await $.env.get('CSR_DECIDE_URL')) || DEFAULT_URL
      // The library only checks that a key exists; the local endpoint ignores the header.
      const config = { ...configured, apiKey: 'local' }
      const { result, messages } = await compactSession(e.messages, config, async (_cloudUrl, init) => {
        if (!useModel) return { status: 200, ok: true, text: ruleReply(init.body) }
        const r = await $.http.fetch(url, init)
        return { status: r.status, ok: r.ok, text: r.text }
      })
      if (useModel) for (const line of decisionLogLines(result)) $.ui.log(`csr-decide: ${line}`)
      const took = `${Date.now() - t0}ms via ${useModel ? url : 'rule'}`
      if (reductionRatio(result) < config.minReductionRatio) {
        $.ui.log(`csr-decide: session.compact fallback to built-in summary (${summarize(result)}) in ${took}`)
        return next(e)
      }
      const pointed = messages.map(m =>
        m.toolResults?.some(tr => tr.text.includes(TRUNCATED))
          ? { ...m, toolResults: m.toolResults.map(tr => (tr.text.includes(TRUNCATED) ? { ...tr, text: tr.text + POINTER } : tr)) }
          : m,
      )
      $.ui.log(
        `csr-decide: session.compact kept ${pointed.length}/${e.messages.length} messages, no summary (${summarize(result)}) in ${took}`,
      )
      return { messages: pointed }
    } catch (err) {
      $.ui.log(`csr-decide: session.compact fallback to built-in summary (${String(err)}) after ${Date.now() - t0}ms`)
      return next(e)
    }
  })

  // Proof aid only: with CSR_DECIDE_FORCE_COMPACT=1 the first finished turn requests a compaction, so the hook
  // above can be exercised under `claude -p` without filling the context window.
  let forced = false
  on('turn.complete', async ($, e, next) => {
    if (!forced && (await $.env.get('CSR_DECIDE_FORCE_COMPACT')) === '1') {
      forced = true
      try {
        await $.session.compact()
      } catch (err) {
        $.ui.log(`csr-decide: forced compact skipped (${String(err)})`)
      }
    }
    return next(e)
  })
}
