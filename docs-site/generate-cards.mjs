import { execFile as execFileCallback } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { readFile, stat, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const execFile = promisify(execFileCallback);

loadDotEnv(new URL('../.env', import.meta.url));
loadDotEnv(new URL('./.env', import.meta.url));

const imageDir = fileURLToPath(new URL('./public/images/', import.meta.url));
const openaiEndpoint = 'https://api.openai.com/v1/images';
const model = process.env.OPENAI_IMAGE_MODEL || 'gpt-image-1';
const outputWidth = Number.parseInt(process.env.CARD_OUTPUT_WIDTH || '1200', 10);
const outputHeight = Number.parseInt(process.env.CARD_OUTPUT_HEIGHT || '670', 10);
const imageSize =
  process.env.OPENAI_IMAGE_SIZE || (model === 'dall-e-3' ? '1792x1024' : '1536x1024');
const imageQuality =
  process.env.OPENAI_IMAGE_QUALITY || (model === 'dall-e-3' ? 'hd' : 'high');

const apiKey = process.env.OPENAI_API_KEY;

const sharedStyle = `
Consistent visual style for this whole README bento card set:
- Final artwork is a single polished rounded rectangle card, centered in the image with enough outer margin for its shadow to be visible.
- Bake styling into the pixels: 16px rounded corners, soft medium drop shadow like rgba(0,0,0,0.25) with 8px blur and 4px downward offset, and a 1px solid border.
- Use refined editorial typography: high-contrast serif for large headings and large numeric scores, clean sans serif for labels and supporting text.
- Keep all text crisp, spelled exactly as specified, with no extra labels, no watermark, no logo, and no decorative text.
- Preserve the source card's charts, diagrams, data, labels, numbers, and captions exactly.
- Compose for a 1200x670 final crop; keep every important element inside the central safe area.
`.trim();

const cards = [
  {
    output: 'card-01-hook-dark.png',
    source: 'card-01-hook-dark.png',
    title: 'The Forgetting Problem',
    prompt: `
${sharedStyle}

Theme:
- Dark version. Deep #1a1a2e / charcoal navy card background with subtle texture, cream white text, border rgba(255,255,255,0.12).

Exact content:
- Top-left small section number: "01".
- Large headline: "The Forgetting Problem".
- Subtitle under headline: "Claude has amnesia. Every new session starts from zero."
- Main visual is a red/pink translucent area chart showing context retention dropping across sessions.
- Y-axis label: "Context Retention".
- X-axis label: "Sessions".
- Y-axis tick labels: "100%", "75%", "50%", "20%", "0%".
- X-axis tick labels: "0", "5", "10", "15", "20".
- Area chart shape starts at 100% at session 0, falls to about 58% at session 3, 42% at session 5, 24% at session 7, below 20% after session 10, then flattens near 5% by session 20. Use subtle horizontal grid lines.
- Centered statement below chart: "Average context retention drops below 20% after 10 sessions."
- Centered citation at bottom: "Liu et al. 2024".

Layout:
- Match the source card: number and title at upper left, chart across the middle, statement below, citation at the bottom.
`.trim(),
  },
  {
    output: 'card-01-hook-light.png',
    source: 'card-01-hook-light.png',
    title: 'The Forgetting Problem',
    prompt: `
${sharedStyle}

Theme:
- Light version. White / #f8f9fa card background, black text, border rgba(0,0,0,0.12), subtle paper grain.

Exact content:
- Top-left small section number: "01".
- Large headline: "The Forgetting Problem".
- Subtitle under headline: "Claude has amnesia. Every new session starts from zero."
- Main visual is a red/pink translucent area chart showing context retention dropping across sessions.
- Y-axis label: "Context Retention".
- Y-axis tick labels: "100%", "80%", "60%", "40%", "20%", "0%".
- X-axis tick labels: "Session 1" through "Session 20", angled diagonally.
- Area chart shape starts near 100% at Session 1, slowly declines through Session 7, steeply drops around Session 8 to Session 10, goes below 20% at Session 10, then tapers close to 0% by Session 20.
- Text inside lower-left of the chart area: "Average context retention drops below 20% after 10 sessions."
- Centered citation at bottom: "Liu et al. 2024" with "et al." italicized if possible.

Layout:
- Match the source card: number and title at upper left, wide area chart filling the center, citation at the bottom.
`.trim(),
  },
  {
    output: 'card-02-arch-dark.png',
    source: 'card-02-arch-dark.png',
    title: 'One Binary. 44MB.',
    prompt: `
${sharedStyle}

Theme:
- Dark version. Deep #1a1a2e / charcoal navy card background, warm cream text, border rgba(255,255,255,0.12).

Exact content:
- Top-left small section number: "02".
- Large headline: "One Binary. 44MB."
- Subtitle: "Everything runs locally. No Docker. No database. No API keys."
- Central architecture panel labeled "csr-engine" at the top center.
- Inside the panel, four rounded rectangular modules connected left-to-right by arrows:
  1. Purple module labeled "SQLite".
  2. Magenta module labeled "HNSW" with small sublabel "<1ms".
  3. Blue module labeled "FastEmbed" with small sublabel "384-dim".
  4. Green module labeled "AST" with small sublabel "6 languages".
- Text below the panel: "6 hooks across session lifecycle. 12 MCP tools for search."
- Bottom badges: "local-first" and "93ms startup".

Layout:
- Match the source dark card: headline and subtitle at top, wide horizontal architecture diagram across the middle, statement and two badges centered below.
`.trim(),
  },
  {
    output: 'card-02-arch-light.png',
    source: 'card-02-arch-light.png',
    title: 'One Binary. 44MB.',
    prompt: `
${sharedStyle}

Theme:
- Light version. White / #f8f9fa card background, dark ink text, border rgba(0,0,0,0.12), subtle paper grain.

Exact content:
- Top-left small section number: "02".
- Large headline: "One Binary. 44MB."
- Subtitle: "Everything runs locally. No Docker. No database. No API keys."
- Central architecture diagram with a center rounded rectangle labeled "csr-engine".
- Four rounded module boxes arranged around the center and connected by arrows:
  1. Upper left purple box labeled "SQLite" with a small database cylinder icon.
  2. Upper right green box labeled "HNSW" with small sublabel "<1ms" and a graph/vector icon.
  3. Lower left red/pink box labeled "FastEmbed" with small sublabel "384-dim" and a brain icon.
  4. Lower right green box labeled "AST" with small sublabel "6 languages" and a tree icon.
- Text centered below diagram: "6 hooks across session lifecycle. 12 MCP tools for search."
- Bottom badges: "local-first" and "93ms startup".

Layout:
- Match the source light card: framed card centered on a pale backdrop, divider line near the top, architecture diagram in the middle, statement and badges below.
`.trim(),
  },
  {
    output: 'card-03-pipeline-dark.png',
    source: 'card-03-pipeline-dark.png',
    title: 'The Pipeline',
    prompt: `
${sharedStyle}

Theme:
- Dark version. Deep #1a1a2e / charcoal navy card background, warm cream text, border rgba(255,255,255,0.12).

Exact content:
- Top-left small section number: "03".
- Large headline: "The Pipeline".
- Top-center subtitle: "Three layers.  Each one makes memory better."
- Three columns across the card:
  1. Left column: label "LAYER 1", title "Retrieve", description "Fast semantic recall from your history.", purple dotted semantic cluster diagram, label "Quality Score", large score "0.074".
  2. Middle column: label "LAYER 2", title "Re-rank", description "Cross-encoder precision reorders the top results.", pink dashed re-ranking diagram, label "Quality Score", large score "0.345".
  3. Right column: label "LAYER 3", title "Re-write", description "LLM distills to the essence you need.", green refined semantic cluster diagram, label "Quality Score", large score "0.691".
- Bottom full-width translucent callout bar with italic text: "Higher quality context. Better decisions. Fewer tokens."

Layout:
- Match the source dark card: thin divider line near the top, three evenly spaced columns, no extra arrows, callout bar across the bottom.
`.trim(),
  },
  {
    output: 'card-03-pipeline-light.png',
    source: 'card-03-pipeline-light.png',
    title: 'The Pipeline',
    prompt: `
${sharedStyle}

Theme:
- Light version. White / #f8f9fa card background, dark ink text, border rgba(0,0,0,0.12), subtle paper grain.

Exact content:
- Large headline: "The Pipeline".
- Top-center subtitle: "Three layers.  Each one makes memory better."
- Three framed columns across the card with arrows between columns:
  1. Left card: label "LAYER 1", title "Retrieve", description "Fast semantic recall from your history.", purple dotted semantic cluster diagram, label "Quality Score", large score "0.074".
  2. Middle card: label "LAYER 2", title "Re-rank", description "Cross-encoder precision reorders the top results.", pink dashed re-ranking diagram, label "Quality Score", large score "0.345".
  3. Right card: label "LAYER 3", title "Re-write", description "LLM distills to the essence you need.", green refined semantic cluster diagram, label "Quality Score", large score "0.691".
- Bottom full-width bordered callout bar with italic text: "Higher quality context. Better decisions. Fewer tokens."

Layout:
- Match the source light card: thin divider line near the top, three side-by-side framed panels, large arrows from Retrieve to Re-rank and from Re-rank to Re-write, callout bar across the bottom.
`.trim(),
  },
];

if (!apiKey) {
  throw new Error(
    'OPENAI_API_KEY is not set. Export OPENAI_API_KEY and rerun: node docs-site/generate-cards.mjs',
  );
}

for (const card of cards) {
  const sourcePath = path.join(imageDir, card.source);
  const outputPath = path.join(imageDir, card.output);

  console.log(`Generating ${card.output} with ${model} (${imageSize})`);
  const image = await createImage(card.prompt, sourcePath);
  await writeFile(outputPath, image);
  await cropAndResize(outputPath);

  const result = await stat(outputPath);
  console.log(`Saved ${card.output}: ${formatBytes(result.size)}`);
}

async function createImage(prompt, sourcePath) {
  if (model === 'dall-e-3') {
    return generateImage(prompt);
  }

  if (!existsSync(sourcePath)) {
    throw new Error(`Source image missing: ${sourcePath}`);
  }

  return editImage(prompt, sourcePath);
}

async function editImage(prompt, sourcePath) {
  const imageBuffer = await readFile(sourcePath);
  const form = new FormData();
  form.append('model', model);
  form.append('prompt', prompt);
  form.append('n', '1');
  form.append('size', imageSize);
  form.append('quality', imageQuality);
  form.append('output_format', 'png');

  if (model === 'gpt-image-1' || model === 'gpt-image-1.5' || model === 'gpt-image-1-mini') {
    form.append('input_fidelity', 'high');
  }

  form.append('image', new Blob([imageBuffer], { type: 'image/png' }), path.basename(sourcePath));

  const response = await fetch(`${openaiEndpoint}/edits`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${apiKey}` },
    body: form,
  });

  return parseImageResponse(response);
}

async function generateImage(prompt) {
  const response = await fetch(`${openaiEndpoint}/generations`, {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${apiKey}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      model,
      prompt,
      n: 1,
      size: imageSize,
      quality: imageQuality,
      response_format: 'b64_json',
      style: 'natural',
    }),
  });

  return parseImageResponse(response);
}

async function parseImageResponse(response) {
  const body = await response.text();

  if (!response.ok) {
    throw new Error(`OpenAI Images API failed (${response.status}): ${body}`);
  }

  const json = JSON.parse(body);
  const first = json.data?.[0];

  if (first?.b64_json) {
    return Buffer.from(first.b64_json, 'base64');
  }

  if (first?.url) {
    const imageResponse = await fetch(first.url);
    if (!imageResponse.ok) {
      throw new Error(`Image download failed (${imageResponse.status}): ${await imageResponse.text()}`);
    }
    return Buffer.from(await imageResponse.arrayBuffer());
  }

  throw new Error(`OpenAI Images API response did not contain b64_json or url: ${body}`);
}

async function cropAndResize(filePath) {
  const { width, height } = await getDimensions(filePath);
  const targetRatio = outputWidth / outputHeight;
  const currentRatio = width / height;
  let cropWidth = width;
  let cropHeight = height;

  if (Math.abs(currentRatio - targetRatio) > 0.005) {
    if (currentRatio > targetRatio) {
      cropWidth = Math.round(height * targetRatio);
    } else {
      cropHeight = Math.round(width / targetRatio);
    }

    await execFile('sips', [
      '--cropToHeightWidth',
      String(cropHeight),
      String(cropWidth),
      filePath,
    ]);
  }

  await execFile('sips', ['-z', String(outputHeight), String(outputWidth), filePath]);
}

async function getDimensions(filePath) {
  const { stdout } = await execFile('sips', ['-g', 'pixelWidth', '-g', 'pixelHeight', filePath]);
  const width = Number.parseInt(stdout.match(/pixelWidth:\s*(\d+)/)?.[1] || '', 10);
  const height = Number.parseInt(stdout.match(/pixelHeight:\s*(\d+)/)?.[1] || '', 10);

  if (!width || !height) {
    throw new Error(`Could not read image dimensions for ${filePath}: ${stdout}`);
  }

  return { width, height };
}

function loadDotEnv(url) {
  if (!existsSync(url)) {
    return;
  }

  const lines = readFileSync(url, 'utf8').split(/\r?\n/);
  for (const line of lines) {
    const match = line.match(/^\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)\s*$/);
    if (!match || process.env[match[1]] !== undefined) {
      continue;
    }

    let value = match[2];
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    process.env[match[1]] = value;
  }
}

function formatBytes(bytes) {
  if (bytes < 1024) {
    return `${bytes} B`;
  }

  const kb = bytes / 1024;
  if (kb < 1024) {
    return `${kb.toFixed(1)} KB`;
  }

  return `${(kb / 1024).toFixed(1)} MB`;
}
