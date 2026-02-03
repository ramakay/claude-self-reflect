# Image Generator Plugin Design

**Date:** 2026-02-01
**Status:** Approved

## Overview

Move the global `image-gen` skill to a self-contained plugin with:
- Cost tracking per generation
- AI-powered quality verification
- Style reference image support
- Structured JSON output for agent consumption

## Decisions

| Topic | Decision |
|-------|----------|
| Location | `~/.claude/plugins/local/image-generator/` |
| Old skill | Remove `~/.claude/skills/image-gen/` |
| API | Google AI direct (not OpenRouter) |
| Cost tracking | Fixed per-image pricing table |
| Quality check | Gemini Vision analyzes output vs prompt elements |
| On quality fail | Auto-retry up to 3 times, return best result |
| .env location | Plugin directory (`image-generator/.env`) |
| Output format | Structured JSON with cost + quality |
| Prompts | Project-local first, plugin defaults fallback |
| Dependencies | `@google/genai`, `dotenv` only (no Jimp) |

## Plugin Structure

```
~/.claude/plugins/local/image-generator/
├── manifest.json           # Plugin metadata
├── .env                    # GOOGLE_AI_API_KEY (gitignored)
├── .env.example            # Template for users
├── .gitignore              # Ignore .env and output/
├── package.json            # Node dependencies
├── src/
│   ├── generate.js         # Main generator with quality loop
│   ├── quality-check.js    # Gemini vision analysis
│   └── pricing.js          # Cost calculation table
├── prompts/                # Default prompts (global)
│   └── README.md           # How to create prompts
├── skills/
│   └── image-generator/
│       └── SKILL.md        # Skill instructions for agents
└── output/                 # Default output dir (gitignored)
```

## CLI Interface

```bash
node generate.js <prompt-name> [options]

Options:
  -m, --model <model>       Model: nano|pro|standard|ultra (default: nano)
  -a, --aspect <ratio>      Aspect ratio: 1:1, 16:9, 4:5, etc.
  -c, --count <n>           Number of variations to generate
  -o, --output <name>       Output filename (without extension)
  --style-ref <path>        Style reference image path
  --quality-check           Enable AI quality verification
  --max-retries <n>         Max retries on quality fail (default: 3)
  --output-dir <path>       Output directory
  --prompts-dir <path>      Custom prompts directory
  -f, --force               Overwrite existing files
  --json                    Output JSON only (for programmatic use)
```

## Pricing Table

| Model | API | Cost/Image | Notes |
|-------|-----|------------|-------|
| nano | gemini-2.5-flash-image-preview | $0.039 | Quick iteration |
| pro | gemini-3-pro-image-preview | $0.134 | Better quality |
| standard | imagen-4.0-generate-preview | $0.04 | Photorealistic |
| ultra | imagen-4.0-ultra-generate | $0.06 | Best text rendering |
| quality-check | gemini-2.5-flash | ~$0.01 | Vision analysis |

## Generation Workflow

```
┌─────────────────────────────────────────────────────────────┐
│                    GENERATION WORKFLOW                       │
└─────────────────────────────────────────────────────────────┘

1. PARSE REQUEST
   ├── Load prompt from file (project → plugin fallback)
   ├── Extract required elements from prompt
   ├── Load style reference image if provided
   └── Select model + calculate expected cost

2. GENERATE IMAGE (attempt 1/3)
   ├── Gemini: generateContent with multimodal input
   │   └── Include style-ref as inlineData if provided
   ├── Imagen: generateImages or editImage (for style transfer)
   ├── Save to temp location
   └── Record cost from pricing table

3. QUALITY CHECK (if --quality-check)
   ├── Send image + prompt elements to Gemini Vision
   ├── Prompt: "Does this image contain: [elements]?"
   ├── Get response: { found: [], missing: [], score: 0.0-1.0 }
   └── Add $0.01 to cost

4. DECISION
   ├── No quality check → ACCEPT
   ├── Score ≥ 0.8 AND no critical elements missing → ACCEPT
   ├── Retries < max_retries → Loop to step 2
   └── Retries exhausted → Return BEST result (highest score)

5. OUTPUT
   └── Return structured JSON
```

## Output Format

### JSON Output (--json or programmatic)

```json
{
  "success": true,
  "image_path": "/absolute/path/to/image.png",
  "cost": {
    "generation": "$0.039",
    "quality_checks": "$0.02",
    "total": "$0.059"
  },
  "quality": {
    "enabled": true,
    "score": 0.92,
    "elements_found": ["chrome logo", "gradient background", "3D effect"],
    "elements_missing": [],
    "retries": 1
  },
  "metadata": {
    "model": "nano",
    "aspect_ratio": "16:9",
    "style_ref": null,
    "prompt_name": "my-logo",
    "timestamp": "2026-02-01T10:30:00Z"
  }
}
```

### Human Output (default)

```
🎨 Image Generator

📝 Prompt: my-logo
🤖 Model: nano (gemini-2.5-flash-image-preview)
📐 Aspect: 16:9
🎨 Style ref: none

🚀 Generating...
✅ Generated: /path/to/my-logo.png

🔍 Quality Check (attempt 1/3)
   Score: 0.72
   Found: chrome logo, gradient
   Missing: 3D effect
   → Retrying...

🚀 Generating (retry 1)...
✅ Generated: /path/to/my-logo.png

🔍 Quality Check (attempt 2/3)
   Score: 0.92
   Found: chrome logo, gradient, 3D effect
   Missing: none
   → Accepted!

💰 Cost Breakdown:
   Generation: $0.078 (2 attempts × $0.039)
   Quality:    $0.020 (2 checks × $0.01)
   Total:      $0.098

✨ Done! /path/to/my-logo.png
```

## Style Reference Support

### For Gemini Models (nano/pro)

```javascript
await ai.models.generateContent({
  model: 'gemini-2.5-flash-image-preview',
  contents: [
    { text: promptText },
    {
      inlineData: {
        mimeType: "image/png",
        data: fs.readFileSync(styleRefPath).toString('base64')
      }
    }
  ]
});
```

### For Imagen Models (standard/ultra)

```javascript
await ai.models.editImage({
  model: 'imagen-4.0-capability-001',
  prompt: promptText,
  referenceImages: [styleReferenceImage],
  config: {
    editMode: 'EDIT_MODE_STYLE',
    numberOfImages: 1
  }
});
```

## Quality Check Implementation

### Prompt Template

```
Analyze this generated image against the requirements.

Required elements from prompt:
{{extracted_elements}}

Respond with JSON only:
{
  "found": ["element1", "element2"],
  "missing": ["element3"],
  "score": 0.85,
  "notes": "Optional observations"
}

Score guidelines:
- 1.0: All elements present and well-executed
- 0.8+: All critical elements present
- 0.5-0.8: Some elements missing or poorly executed
- <0.5: Major elements missing
```

### Element Extraction

Parse prompt for structured elements:
- Lines starting with `-` or `•`
- Content after `Subject:`, `Style:`, `Elements:` headers
- Content in `CRITICAL:` or `REQUIRED:` sections

## Prompt Resolution

1. Check `./prompts/<name>.txt` (current project)
2. Check `~/.claude/plugins/local/image-generator/prompts/<name>.txt`
3. Error if not found in either location

## SKILL.md for Agents

```markdown
---
name: image-generator
description: Generate images with cost tracking and quality verification
---

# Image Generator

Generate images using Google AI models with automatic cost tracking
and optional AI-powered quality verification.

## Quick Start

\`\`\`bash
cd ~/.claude/plugins/local/image-generator
node src/generate.js <prompt-name> [options]
\`\`\`

## When to Use Quality Check

- **Skip** for quick iteration/drafts
- **Enable** for production images, client deliverables

## Cost Guidance

Always report the total cost to the user after generation.
Example: "Generated logo.png (cost: $0.059)"

## Output Handling

Parse the JSON output to get:
- `image_path`: Absolute path to generated image
- `cost.total`: Total cost string to report
- `quality.score`: Quality score (0-1)
- `quality.elements_missing`: What to mention if quality < 0.8
```

## Migration Steps

1. Create plugin directory structure
2. Copy generate.js, enhance with new features
3. Create pricing.js, quality-check.js
4. Move .env from self-branding to plugin
5. Create SKILL.md
6. Copy useful prompts from self-branding/prompts/
7. Remove old skill at ~/.claude/skills/image-gen/
8. Test generation with quality check
9. Update global CLAUDE.md references
