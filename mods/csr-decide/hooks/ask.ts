// Jev's wire format against a LOCAL System One endpoint. Pure helpers only: the validator follows `$` into
// functions of the hooks module itself, never across an import, so every `$` call lives in register.ts.
//   POST { model, state, questions } -> { answers: { name: { choice, probabilities, confidence } | { noul } | ... } }

// Override with CSR_DECIDE_URL. The name is spelled out at each $.env.get call: the validator wants a literal.
export const DEFAULT_URL = 'http://127.0.0.1:8765/v1/systemone'

export type Question =
  | { type: 'noul'; instructions: string }
  | { type: 'choice'; instructions: string; criteria: Record<string, string> }
  | { type: 'score'; instructions: string; criteria: string[] }

export type Answer = {
  noul?: number
  choice?: string
  score?: number
  confidence?: number
  probabilities?: Record<string, number>
}

export type Reply = { answers: Record<string, Answer>; timing_ms?: { total?: number }; state_tokens?: number }

export function requestInit(state: unknown, questions: Record<string, Question>) {
  return {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ model: 'local', state, questions }),
  }
}

/** The reply, or undefined for anything that is not an `answers` object. Never throws. */
export function parseReply(ok: boolean, text: string): Reply | undefined {
  if (!ok) return undefined
  try {
    const parsed = JSON.parse(text) as Reply
    return parsed && typeof parsed.answers === 'object' && parsed.answers !== null ? parsed : undefined
  } catch {
    return undefined
  }
}

/**
 * The compaction rule, as a System One reply: every old call stays, every old result is truncated. Answers
 * fast-jev's own `call_<id>` / `result_<id>` questions in-process, so the library runs unchanged with no endpoint.
 * A blind judge dropped the verbatim result on 55 of 60 sampled calls; this agrees 92%, above both local models.
 */
export function ruleReply(body: string): string {
  const { questions } = JSON.parse(body) as { questions: Record<string, unknown> }
  const answers: Record<string, Answer> = {}
  for (const name of Object.keys(questions)) answers[name] = { noul: name.startsWith('result_') ? 0 : 1 }
  return JSON.stringify({ answers })
}

// Same wording as the offline arm (experiments: a3_react.py), so the mod and the benchmark ask one question.
export const REACTION: Record<string, Question> = {
  reaction: {
    type: 'choice',
    instructions:
      'How does user_turn react to what the assistant just did or proposed in assistant_tail? ' +
      'If it both reacts and adds a new instruction, label the reaction.',
    criteria: {
      correction:
        'The human says the assistant got it wrong, broke something, misunderstood, or that the result still fails.',
      acceptance:
        'The human approves, confirms, or tells the assistant to go ahead with what it just did or proposed.',
      none:
        'Anything else: a new instruction, a follow-up question, added information, a repeated or rephrased request, ' +
        'a switch of topic, or a neutral continuation that neither approves nor corrects.',
    },
  },
}
