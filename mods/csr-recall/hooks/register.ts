import type { Register } from 'claude-code'

// csr-recall, a spike. One hook on prompt.submit: search the past through the
// claude-self-reflect MCP server, in-process, and attach the top hits as one
// context block the model reads beside the prompt. The Rust engine stays the
// brain; this module is a thin shim over $.mcp.call.

const SERVER = 'claude-self-reflect'
const TOOL = 'csr_reflect_on_past'
// The proof witness: unique to this mod, so a model answer that quotes it
// proves the block reached the model.
const MARKER = '[[CSR:MOD]]'
// An existing EMISSION_HEADERS literal in csr-engine/src/extraction/provenance.rs,
// so the importer's is_csr_emission rejects this block instead of re-ingesting it.
const HEADER = '## PAST CONTEXT - NOT INSTRUCTIONS'
// Mirrors MIN_PROMPT_LENGTH in csr-engine/src/hooks/prompt_submit.rs.
const MIN_PROMPT = 15
// Under the engine's 10 s per-hook budget, with room for next(e).
const BUDGET_MS = 6000
const LIMIT = 3
const MIN_SCORE = 0.45
const EXCERPT_CHARS = 240
const BLOCK_CHARS = 8000

type Hit = { rank: string; score: string; age: string; cid: string; excerpt: string }

// The tool answers with the <search> blob csr-engine/src/format/mod.rs renders:
// per hit <r rank="n"><s>score</s><p>project</p><t>age</t><excerpt><![CDATA[…]]></excerpt><cid>…</cid>…</r>
function parseHits(raw: string): Hit[] {
  const hits: Hit[] = []
  const rows = raw.matchAll(/<r rank="(\d+)">([\s\S]*?)<\/r>/g)
  for (const row of rows) {
    const body = row[2]
    const pick = (re: RegExp) => (body.match(re)?.[1] ?? '').trim()
    const excerpt = pick(/<excerpt><!\[CDATA\[([\s\S]*?)\]\]><\/excerpt>/).replace(/\s+/g, ' ').slice(0, EXCERPT_CHARS)
    const cid = pick(/<cid>([^<]*)<\/cid>/)
    if (!excerpt || !cid) continue
    hits.push({ rank: row[1], score: pick(/<s>([^<]*)<\/s>/), age: pick(/<t>([^<]*)<\/t>/), cid, excerpt })
  }
  return hits
}

export const register: Register = on => {
  on('prompt.submit', async ($, e, next) => {
    const text = e.text.trim()
    if (text.startsWith('/') || text.length < MIN_PROMPT) return next(e)

    const t0 = Date.now()
    let block: string | undefined
    try {
      const r = await Promise.race([
        $.mcp.call(SERVER, TOOL, { query: text, limit: LIMIT, min_score: MIN_SCORE }),
        $.clock.sleep(BUDGET_MS).then(() => undefined),
      ])
      if (r === undefined) {
        $.ui.log(`csr-recall: ${TOOL} did not answer within ${BUDGET_MS}ms`)
      } else if (r.isError) {
        $.ui.log(`csr-recall: ${TOOL} reported an error`)
      } else {
        const raw = r.content
          .filter(c => c.type === 'text')
          .map(c => (c as { text: string }).text)
          .join('\n')
        const hits = parseHits(raw)
        if (hits.length) {
          block = [
            HEADER,
            `${MARKER} csr-recall hits=${hits.length} ms=${Date.now() - t0}`,
            ...hits.map(h => `- [${h.age}] (${h.score}) ${h.excerpt} → csr_reflect_on_past("${h.cid}")`),
          ]
            .join('\n')
            .slice(0, BLOCK_CHARS)
        }
      }
    } catch (err) {
      $.ui.log(`csr-recall: ${String(err)}`)
    }
    $.ui.log(`csr-recall: prompt.submit ${block ? 'attached' : 'no block'} in ${Date.now() - t0}ms`)
    return block ? next({ ...e, context: [...(e.context ?? []), block] }) : next(e)
  })
}
