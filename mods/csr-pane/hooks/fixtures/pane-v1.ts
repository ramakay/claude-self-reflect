// Written by csr-engine (companion/contract_tests.rs, CSR_BLESS=1). Do not edit.
export const PANE_V1 = {
  "schema": 1,
  "cards": [
    {
      "id": "dreamer",
      "title": "Self-Reflect's dreamer",
      "tag": "Looks as you work",
      "lines": [
        {
          "text": "The last look found nothing. 4 of 45 looks end this way.",
          "style": "dim"
        }
      ],
      "ring": {
        "spent": 607000.0,
        "cap": 5900000.0,
        "turning": false
      }
    },
    {
      "id": "same_page",
      "title": "Same page?",
      "lines": [
        {
          "text": "Model's read: 3 of 9 asks missed. You were unclear on 1.",
          "style": "body"
        },
        {
          "text": "You keep sending screenshots to show what Claude could not see.",
          "style": "dim"
        }
      ]
    },
    {
      "id": "should_be",
      "title": "Should be a skill / memory / tool",
      "tag": "1 of 2",
      "lines": [
        {
          "text": "memory: Look at a pane change on a real screen before reporting it. · evidence",
          "style": "body"
        },
        {
          "text": "skill: A clean probe for a live mod was rebuilt by hand. · evidence",
          "style": "body"
        }
      ]
    },
    {
      "id": "story",
      "title": "Where the work stands",
      "tag": "refreshed 5m ago",
      "lines": [
        {
          "text": "The work lives in the feat/pane-mod worktree.",
          "style": "body"
        },
        {
          "text": "1 note kept",
          "style": "mint"
        },
        {
          "text": "+ picture.ts · drawPicture  unverified",
          "style": "code"
        }
      ]
    },
    {
      "id": "limits",
      "title": "Early in the week. Weekly is at 7% so far.",
      "tone": "quiet",
      "lines": [
        {
          "text": "Early · earlier weeks peaked around 78% · resets in 6d 16h",
          "style": "caption"
        },
        {
          "text": "Codex weekly limit",
          "style": "caption"
        }
      ],
      "rows": [
        {
          "label": "Weekly",
          "value": 0.07,
          "text": "7%",
          "tone": "ok"
        },
        {
          "label": "Codex 7D",
          "value": 0.22,
          "text": "22%",
          "tone": "ok"
        }
      ]
    }
  ],
  "csr_row": {
    "lead": "Self-Reflect this session:",
    "parts": [
      "3 compactions",
      "~1.8M tokens saved (est.)"
    ]
  },
  "context": {
    "used": 130000.0,
    "max": 1000000.0,
    "free": 870000.0,
    "parts": [
      {
        "name": "Summary + setup",
        "tokens": 75562.0,
        "ink": "setup"
      },
      {
        "name": "Messages",
        "tokens": 23633.0,
        "ink": "messages"
      },
      {
        "name": "Tool results",
        "tokens": 31222.0,
        "ink": "tools"
      }
    ],
    "mix": {
      "shares": [
        0.23940130504458776,
        0.7605986949554122,
        0.0
      ],
      "verdict": "context-heavy"
    }
  }
}
