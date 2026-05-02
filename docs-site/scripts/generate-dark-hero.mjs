import { spawn } from "node:child_process";
import { mkdir, readFile, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const docsRoot = path.resolve(__dirname, "..");
const projectRoot = path.resolve(docsRoot, "..");
const outputPath = path.join(docsRoot, "public", "images", "readme-hero-dark.png");

const width = 1200;
const height = 630;

const colors = {
  bg: "#0d1117",
  card: "#161b22",
  cardSoft: "#1c222d",
  border: "#30363d",
  text: "#e6edf3",
  muted: "#8b949e",
  soft: "#c9d1d9",
  amber: "#f2cc60",
  amberDeep: "#d29922",
  blue: "#58a6ff",
  purple: "#a371f7",
  green: "#7ee787",
  rose: "#ff7b72",
  note: "#f7df8d",
};

const facts = {
  product: "Claude Self-Reflect",
  tagline: "Single binary. Perfect memory.",
  binary: "44MB",
  install:
    "curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh",
  components: [
    "SQLite",
    "HNSW (<1ms search)",
    "FastEmbed (384-dim)",
    "AST (6 languages)",
  ],
  hooks: [
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "Stop",
    "PreCompact",
    "SessionEnd",
  ],
  tools: [
    "reflect_on_past",
    "store_reflection",
    "search_by_recency",
    "get_recent_work",
  ],
  stats: ["1,107 conversations", "15,745 chunks", "93ms startup", "<1ms p95 search"],
  enrichment: "0.074 → 0.345 → 0.691",
  lift: "9.3x improvement",
};

const readmeChecks = [
  ["product", "# Claude Self-Reflect"],
  ["tagline source", "Single 44MB binary."],
  ["install command", facts.install],
  ["SQLite", "**SQLite** stores chunks"],
  ["FastEmbed", "generates 384-dim vectors"],
  ["HNSW", "sub-millisecond approximate nearest neighbor search"],
  ["AST", "Rust, Python, TS, JS, Go, TSX"],
  ["SessionStart", "**SessionStart**"],
  ["UserPromptSubmit", "**UserPromptSubmit**"],
  ["PostToolUse", "**PostToolUse**"],
  ["Stop", "**Stop**"],
  ["PreCompact", "**PreCompact**"],
  ["SessionEnd", "**SessionEnd**"],
  ["12 tools", "12 tools available"],
  ["reflect tool", "`csr_reflect_on_past`"],
  ["store_reflection", "`store_reflection`"],
  ["search_by_recency", "`search_by_recency`"],
  ["get_recent_work", "`get_recent_work`"],
  ["93ms", "| **Cached startup** | 93ms |"],
  ["<1ms", "| **Search latency (p95)** | <1ms |"],
  ["44MB", "| **Binary size** | 44MB |"],
  ["0.074", "0.074"],
  ["0.691", "0.691"],
  ["9.3x", "9.3x"],
];

function esc(value) {
  return String(value)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function tag(name, attrs = {}, body = "") {
  const attrText = Object.entries(attrs)
    .filter(([, value]) => value !== undefined && value !== null)
    .map(([key, value]) => `${key}="${esc(value)}"`)
    .join(" ");
  return `<${name}${attrText ? ` ${attrText}` : ""}>${body}</${name}>`;
}

function text(x, y, value, options = {}) {
  const {
    size = 18,
    fill = colors.text,
    weight = 500,
    family = "Inter, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif",
    anchor = "start",
    style = "",
  } = options;

  return tag(
    "text",
    {
      x,
      y,
      fill,
      "font-size": size,
      "font-weight": weight,
      "font-family": family,
      "text-anchor": anchor,
      style,
    },
    esc(value),
  );
}

function rect(x, y, w, h, options = {}) {
  return tag("rect", {
    x,
    y,
    width: w,
    height: h,
    rx: options.rx ?? 8,
    fill: options.fill ?? colors.card,
    stroke: options.stroke ?? colors.border,
    "stroke-width": options.strokeWidth ?? 1,
    filter: options.filter,
    opacity: options.opacity,
  });
}

function line(x1, y1, x2, y2, options = {}) {
  return tag("line", {
    x1,
    y1,
    x2,
    y2,
    stroke: options.stroke ?? colors.border,
    "stroke-width": options.width ?? 1,
    opacity: options.opacity,
  });
}

function circle(cx, cy, r, options = {}) {
  return tag("circle", {
    cx,
    cy,
    r,
    fill: options.fill ?? colors.blue,
    stroke: options.stroke,
    "stroke-width": options.strokeWidth,
    opacity: options.opacity,
  });
}

function pill(x, y, w, label, dotFill) {
  return [
    rect(x, y, w, 38, {
      rx: 19,
      fill: "#0d1117",
      stroke: "rgba(240,246,252,0.10)",
      strokeWidth: 1,
      opacity: 0.98,
    }),
    circle(x + 22, y + 19, 6, { fill: dotFill }),
    text(x + 42, y + 25, label, {
      size: 16,
      fill: colors.text,
      weight: 700,
    }),
  ].join("");
}

function metricPill(x, y, label, value, accent) {
  return [
    rect(x, y, 145, 38, {
      rx: 8,
      fill: "#0d1117",
      stroke: "rgba(240,246,252,0.10)",
      strokeWidth: 1,
    }),
    text(x + 14, y + 24, value, {
      size: 20,
      fill: accent,
      weight: 800,
      family: "'SFMono-Regular', Consolas, 'Liberation Mono', monospace",
    }),
    text(x + 75, y + 24, label, {
      size: 13,
      fill: colors.muted,
      weight: 600,
    }),
  ].join("");
}

function statChip(x, y, w, value, label, accent) {
  return [
    rect(x, y, w, 42, {
      rx: 8,
      fill: "#0d1117",
      stroke: "rgba(240,246,252,0.10)",
      strokeWidth: 1,
    }),
    text(x + 14, y + 21, value, {
      size: 20,
      fill: accent,
      weight: 800,
      family: "'SFMono-Regular', Consolas, 'Liberation Mono', monospace",
    }),
    text(x + 14, y + 36, label, {
      size: 10.5,
      fill: colors.muted,
      weight: 700,
    }),
  ].join("");
}

function componentChip(x, y, w, label, accent) {
  return [
    rect(x, y, w, 24, {
      rx: 6,
      fill: "#0d1117",
      stroke: "rgba(240,246,252,0.10)",
      strokeWidth: 1,
    }),
    circle(x + 13, y + 12, 4.5, { fill: accent }),
    text(x + 25, y + 16, label, {
      size: 11.5,
      fill: colors.soft,
      weight: 700,
    }),
  ].join("");
}

function cardTitle(x, y, title, subtitle) {
  return [
    text(x, y, title, {
      size: 25,
      fill: colors.text,
      weight: 800,
      family: "Georgia, 'Times New Roman', serif",
    }),
    text(x, y + 28, subtitle, {
      size: 15,
      fill: colors.muted,
      weight: 500,
    }),
  ].join("");
}

function leftCard() {
  const x = 72;
  const y = 186;
  return [
    rect(x, y, 310, 310, { rx: 7, fill: colors.card, filter: "url(#cardShadow)" }),
    cardTitle(x + 28, y + 47, "JSONL conversations", "local transcripts become indexed memory"),
    rect(x + 28, y + 102, 96, 126, {
      rx: 6,
      fill: "#0d1117",
      stroke: "rgba(240,246,252,0.10)",
    }),
    text(x + 44, y + 128, "{ role: user }", {
      size: 11,
      fill: colors.soft,
      family: "'SFMono-Regular', Consolas, monospace",
    }),
    line(x + 42, y + 137, x + 110, y + 137, { stroke: colors.border }),
    text(x + 44, y + 155, "{ tool: edit }", {
      size: 11,
      fill: colors.soft,
      family: "'SFMono-Regular', Consolas, monospace",
    }),
    line(x + 42, y + 164, x + 110, y + 164, { stroke: colors.border }),
    text(x + 44, y + 182, "{ result: ok }", {
      size: 11,
      fill: colors.soft,
      family: "'SFMono-Regular', Consolas, monospace",
    }),
    line(x + 42, y + 191, x + 110, y + 191, { stroke: colors.border }),
    text(x + 44, y + 209, "{ note: fix }", {
      size: 11,
      fill: colors.soft,
      family: "'SFMono-Regular', Consolas, monospace",
    }),
    rect(x + 180, y + 98, 94, 50, {
      rx: 6,
      fill: colors.cardSoft,
      stroke: "rgba(240,246,252,0.08)",
    }),
    text(x + 227, y + 129, "parse", { size: 13, fill: colors.soft, anchor: "middle" }),
    line(x + 227, y + 148, x + 227, y + 178, { stroke: colors.border }),
    rect(x + 180, y + 178, 94, 50, {
      rx: 6,
      fill: colors.cardSoft,
      stroke: "rgba(240,246,252,0.08)",
    }),
    text(x + 227, y + 209, "reflect", { size: 13, fill: colors.soft, anchor: "middle" }),
    circle(x + 150, y + 164, 12, { fill: colors.purple, opacity: 0.25 }),
    tag("path", {
      d: `M128 ${y + 251} C155 ${y + 270}, 205 ${y + 268}, 235 ${y + 236}`,
      fill: "none",
      stroke: colors.rose,
      "stroke-width": 1.5,
    }),
    tag("path", {
      d: `M130 ${y + 250} C160 ${y + 259}, 204 ${y + 253}, 235 ${y + 236} C206 ${y + 248}, 166 ${y + 250}, 130 ${y + 250}`,
      fill: colors.soft,
      opacity: 0.35,
    }),
    text(x + 28, y + 256, "memory from every turn", {
      size: 19,
      fill: colors.purple,
      weight: 600,
      family: "Georgia, 'Times New Roman', serif",
    }),
    statChip(x + 28, y + 266, 118, "1,107", "conversations", colors.amber),
    statChip(x + 164, y + 266, 118, "15,745", "chunks", colors.blue),
  ].join("");
}

function centerCard() {
  const x = 445;
  const y = 170;
  const boxX = x + 42;
  const boxY = y + 92;
  return [
    rect(x, y, 310, 332, { rx: 7, fill: colors.card, filter: "url(#cardShadowStrong)" }),
    cardTitle(x + 28, y + 47, "csr-engine · 44MB", "compact indexing core inside one binary"),
    rect(boxX, boxY, 226, 126, {
      rx: 6,
      fill: "#0d1117",
      stroke: "rgba(240,246,252,0.10)",
    }),
    line(boxX + 42, boxY + 32, boxX + 184, boxY + 32, { stroke: colors.border }),
    line(boxX + 113, boxY + 30, boxX + 113, boxY + 126, { stroke: colors.border }),
    circle(boxX + 113, boxY + 68, 48, { fill: colors.purple, opacity: 0.10 }),
    tag("ellipse", {
      cx: boxX + 72,
      cy: boxY + 58,
      rx: 28,
      ry: 8,
      fill: colors.purple,
      opacity: 0.25,
    }),
    circle(boxX + 152, boxY + 62, 6, { fill: colors.rose, opacity: 0.85 }),
    circle(boxX + 184, boxY + 57, 6, { fill: colors.green, opacity: 0.8 }),
    circle(boxX + 193, boxY + 88, 6, { fill: colors.purple, opacity: 0.85 }),
    circle(boxX + 162, boxY + 94, 6, { fill: colors.amber }),
    line(boxX + 152, boxY + 62, boxX + 184, boxY + 57, { stroke: colors.soft, opacity: 0.45 }),
    line(boxX + 152, boxY + 62, boxX + 162, boxY + 94, { stroke: colors.soft, opacity: 0.45 }),
    line(boxX + 162, boxY + 94, boxX + 184, boxY + 57, { stroke: colors.soft, opacity: 0.45 }),
    line(boxX + 162, boxY + 94, boxX + 193, boxY + 88, { stroke: colors.soft, opacity: 0.45 }),
    text(boxX + 62, boxY + 106, "SQLite", { size: 13, fill: colors.soft, anchor: "middle" }),
    text(boxX + 163, boxY + 106, "HNSW", { size: 13, fill: colors.soft, anchor: "middle" }),
    componentChip(x + 32, y + 224, 112, "SQLite", colors.green),
    componentChip(x + 166, y + 224, 112, "HNSW <1ms", colors.blue),
    componentChip(x + 32, y + 252, 112, "FastEmbed 384-dim", colors.purple),
    componentChip(x + 166, y + 252, 112, "AST 6 languages", colors.amber),
    text(x + 155, y + 290, "SQLite + vectors + syntax", {
      size: 17,
      fill: colors.green,
      weight: 650,
      anchor: "middle",
      family: "Georgia, 'Times New Roman', serif",
    }),
    text(x + 32, y + 312, "93ms startup · <1ms p95 search", {
      size: 15,
      fill: colors.soft,
      weight: 800,
      family: "'SFMono-Regular', Consolas, monospace",
    }),
    text(x + 32, y + 330, facts.enrichment, {
      size: 15,
      fill: colors.text,
      weight: 800,
      family: "'SFMono-Regular', Consolas, monospace",
    }),
    text(x + 225, y + 330, facts.lift, {
      size: 12,
      fill: colors.amber,
      weight: 700,
    }),
  ].join("");
}

function hookRow(x, y, dotFill, label) {
  return [
    circle(x, y - 5, 7, { fill: dotFill, opacity: 0.88 }),
    text(x + 20, y, label, { size: 13, fill: colors.soft, weight: 600 }),
  ].join("");
}

function rightCard() {
  const x = 818;
  const y = 186;
  return [
    rect(x, y, 310, 310, { rx: 7, fill: colors.card, filter: "url(#cardShadow)" }),
    cardTitle(x + 28, y + 47, "6 Hooks · 12 MCP Tools", "automatic capture, recall, and reflection"),
    hookRow(x + 37, y + 108, colors.blue, facts.hooks[0]),
    hookRow(x + 37, y + 138, colors.purple, facts.hooks[1]),
    hookRow(x + 37, y + 168, colors.rose, facts.hooks[2]),
    hookRow(x + 178, y + 108, colors.muted, facts.hooks[3]),
    hookRow(x + 178, y + 138, colors.green, facts.hooks[4]),
    hookRow(x + 178, y + 168, colors.amber, facts.hooks[5]),
    rect(x + 28, y + 190, 254, 48, {
      rx: 6,
      fill: "#0d1117",
      stroke: "rgba(240,246,252,0.10)",
    }),
    text(x + 45, y + 212, "reflect_on_past  ·  store_reflection", {
      size: 13,
      fill: colors.text,
      weight: 750,
      family: "'SFMono-Regular', Consolas, monospace",
    }),
    text(x + 45, y + 230, "search_by_recency · get_recent_work · etc.", {
      size: 12,
      fill: colors.muted,
      weight: 650,
      family: "'SFMono-Regular', Consolas, monospace",
    }),
    text(x + 28, y + 259, "install in one command", {
      size: 18,
      fill: colors.rose,
      weight: 600,
      family: "Georgia, 'Times New Roman', serif",
    }),
    rect(x + 28, y + 268, 254, 52, {
      rx: 6,
      fill: "#0d1117",
      stroke: colors.border,
    }),
    text(x + 42, y + 291, "curl -fsSL https://raw.githubusercontent.com/ramakay/", {
      size: 8.2,
      fill: colors.soft,
      weight: 650,
      family: "'SFMono-Regular', Consolas, monospace",
    }),
    text(x + 42, y + 307, "claude-self-reflect/main/scripts/install.sh | sh", {
      size: 8.2,
      fill: colors.soft,
      weight: 650,
      family: "'SFMono-Regular', Consolas, monospace",
    }),
  ].join("");
}

function postIt() {
  return tag(
    "g",
    {},
    [
      rect(926, 58, 194, 88, {
        rx: 5,
        fill: "#000",
        stroke: "none",
        opacity: 0.24,
        filter: "url(#noteShadow)",
      }),
      rect(918, 48, 194, 88, {
        rx: 5,
        fill: colors.note,
        stroke: "rgba(0,0,0,0.08)",
        strokeWidth: 1,
      }),
      rect(918, 48, 194, 30, {
        rx: 5,
        fill: "#f2cc60",
        stroke: "none",
        opacity: 0.2,
      }),
      line(932, 70, 1096, 70, { stroke: "#7d6419", opacity: 0.55 }),
      line(932, 94, 1096, 94, { stroke: "#7d6419", opacity: 0.55 }),
      text(1015, 104, "no more forgetting", {
        size: 18,
        fill: "#3d2b00",
        weight: 600,
        anchor: "middle",
        family: "Georgia, 'Times New Roman', serif",
        style: "font-style: italic",
      }),
    ].join(""),
  );
}

function bottomBadges() {
  const y = 540;
  return [
    rect(72, 518, 1056, 72, {
      rx: 7,
      fill: colors.card,
      stroke: "rgba(240,246,252,0.08)",
      strokeWidth: 1,
    }),
    pill(104, y, 210, "local-first", colors.green),
    pill(352, y, 210, "<1ms search", colors.rose),
    pill(600, y, 232, "384-dim vectors", colors.purple),
    pill(870, y, 210, "zero deps", colors.amber),
  ].join("");
}

function buildSvg() {
  return `<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}">
  <defs>
    <filter id="cardShadow" x="-20%" y="-20%" width="140%" height="140%">
      <feDropShadow dx="8" dy="12" stdDeviation="0" flood-color="#000000" flood-opacity="0.18"/>
    </filter>
    <filter id="cardShadowStrong" x="-20%" y="-20%" width="140%" height="140%">
      <feDropShadow dx="9" dy="13" stdDeviation="0" flood-color="#000000" flood-opacity="0.22"/>
    </filter>
    <filter id="noteShadow" x="-20%" y="-20%" width="140%" height="140%">
      <feGaussianBlur stdDeviation="8"/>
    </filter>
    <radialGradient id="haloOne" cx="50%" cy="50%" r="50%">
      <stop offset="0%" stop-color="#30363d" stop-opacity="0.55"/>
      <stop offset="100%" stop-color="#30363d" stop-opacity="0"/>
    </radialGradient>
    <radialGradient id="haloTwo" cx="50%" cy="50%" r="50%">
      <stop offset="0%" stop-color="#d29922" stop-opacity="0.20"/>
      <stop offset="100%" stop-color="#d29922" stop-opacity="0"/>
    </radialGradient>
  </defs>
  ${rect(0, 0, width, height, { rx: 0, fill: colors.bg, stroke: "none" })}
  ${tag("circle", { cx: 160, cy: 510, r: 105, fill: "#30363d", opacity: 0.18 })}
  ${tag("circle", { cx: 1065, cy: 70, r: 82, fill: "#d29922", opacity: 0.11 })}
  ${text(72, 93, facts.product, {
    size: 58,
    fill: colors.text,
    weight: 800,
    family: "Georgia, 'Times New Roman', serif",
  })}
  ${text(76, 128, facts.tagline, { size: 22, fill: colors.soft, weight: 500 })}
  ${postIt()}
  ${line(72, 160, 1128, 160, { stroke: colors.border, opacity: 0.9 })}
  ${leftCard()}
  ${centerCard()}
  ${rightCard()}
  ${line(72, 508, 1128, 508, { stroke: colors.border, opacity: 0.9 })}
  ${bottomBadges()}
</svg>`;
}

async function verifyReadme() {
  const readme = await readFile(path.join(projectRoot, "README.md"), "utf8");
  const missing = readmeChecks
    .filter(([, snippet]) => !readme.includes(snippet))
    .map(([label]) => label);

  if (missing.length > 0) {
    throw new Error(`README fact verification failed: ${missing.join(", ")}`);
  }
}

async function renderWithSharp(svg) {
  try {
    const sharp = (await import("sharp")).default;
    await sharp(Buffer.from(svg)).png().toFile(outputPath);
    return "sharp";
  } catch (error) {
    if (error?.code !== "ERR_MODULE_NOT_FOUND" && error?.code !== "MODULE_NOT_FOUND") {
      throw error;
    }
    return null;
  }
}

function run(command, args, input) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: ["pipe", "pipe", "pipe"] });
    const chunks = [];
    const errors = [];

    child.stdout.on("data", (chunk) => chunks.push(chunk));
    child.stderr.on("data", (chunk) => errors.push(chunk));
    child.on("error", reject);
    child.on("close", (code) => {
      if (code === 0) {
        resolve(Buffer.concat(chunks));
      } else {
        reject(
          new Error(
            `${command} exited with ${code}: ${Buffer.concat(errors).toString("utf8").trim()}`,
          ),
        );
      }
    });

    child.stdin.end(input);
  });
}

async function renderWithImageMagick(svg) {
  const args = ["svg:-", "-strip", "png32:-"];
  let png;

  try {
    png = await run("magick", args, svg);
  } catch (magickError) {
    try {
      png = await run("convert", args, svg);
    } catch {
      throw magickError;
    }
  }

  await writeFile(outputPath, png);
  return "imagemagick";
}

async function validateOutput() {
  const png = await readFile(outputPath);
  const pngSignature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  if (!png.subarray(0, 8).equals(pngSignature)) {
    throw new Error("Generated output is not a PNG.");
  }

  const { size } = await stat(outputPath);
  if (size <= 10 * 1024) {
    throw new Error(`Generated PNG is too small: ${size} bytes.`);
  }
  return size;
}

async function main() {
  await verifyReadme();
  await mkdir(path.dirname(outputPath), { recursive: true });

  const svg = buildSvg();
  const renderer = (await renderWithSharp(svg)) ?? (await renderWithImageMagick(svg));
  const size = await validateOutput();

  console.log(`Generated ${outputPath}`);
  console.log(`Renderer: ${renderer}`);
  console.log(`Size: ${size} bytes`);
}

main().catch((error) => {
  console.error(error?.message ?? error);
  process.exitCode = 1;
});
