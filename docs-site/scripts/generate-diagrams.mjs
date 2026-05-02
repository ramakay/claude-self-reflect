import OpenAI from "openai";
import { mkdir, readFile, writeFile, stat } from "node:fs/promises";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const docsRoot = path.resolve(__dirname, "..");
const projectRoot = path.resolve(docsRoot, "..");
const outputDir = path.join(docsRoot, "public", "images");

const model = "gpt-image-2";
const sizes = ["1536x1024", "1792x1024", "1024x1024"];
const pngSignature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

const diagrams = [
  {
    name: "Architecture flow diagram",
    outputPath: path.join(outputDir, "arch-diagram.png"),
    prompt:
      "A clean minimal data-flow architecture diagram on a dark navy background (#0a0e1e). Shows: JSONL Files -> Import Pipeline -> FastEmbed 384-dim Embeddings -> HNSW Index -> MCP Server -> Claude Code. SQLite Storage is connected to both Import Pipeline and MCP Server. An Enrichment Pipeline branches from Storage showing three stages: L1 Heuristic -> L2 V3 Extract -> L3 AI Narrative. Use muted purple (#6b5b95), steel blue (#5b7b95), and sage green (#7c9473) for connector lines and node borders. Rounded rectangle nodes with white text labels, thin connector arrows, modern technical diagram aesthetic. No clipart, no icons. 16:9 widescreen ratio. Professional documentation style.",
  },
  {
    name: "Hooks lifecycle diagram",
    outputPath: path.join(outputDir, "hooks-diagram.png"),
    prompt:
      "A clean timeline diagram on a dark navy background (#0a0e1e) showing 6 hooks firing during a Claude Code session. Left to right horizontal timeline with a thin connecting line. Hooks in order: SessionStart (purple dot), UserPromptSubmit (blue dot), PostToolUse (teal dot), Stop (orange dot), PreCompact (yellow dot), SessionEnd (green dot). Each hook is a colored circle node on the timeline with the hook name above and a one-line description below in small text. Labels: SessionStart=inject past context, UserPromptSubmit=predict relevant memory, PostToolUse=track file edits, Stop=store iteration learnings, PreCompact=backup state, SessionEnd=store narrative. Modern minimal technical diagram, white text, dark theme, 16:9 ratio. No clipart.",
  },
  {
    name: "Enrichment pipeline diagram",
    outputPath: path.join(outputDir, "enrichment-diagram.png"),
    prompt:
      "A clean three-stage pipeline visualization on a dark navy background (#0a0e1e). Three stages left to right connected by arrows: Stage 1 'L1 Raw' with relevance score 0.074 shown as a short bar, Stage 2 'L2 Contextualized' with score 0.345 shown as a medium bar, Stage 3 'L3 Reflective' with score 0.691 shown as a tall bar. Each stage has its label, score, and a short description: L1=heuristic extraction, L2=V3 structured extraction, L3=AI narrative. Rising bars colored from muted purple -> steel blue -> bright teal to show quality improvement. A bold callout badge says '9.3x improvement'. Dark theme, modern minimal style, white text, 16:9 ratio. No clipart.",
  },
];

async function loadEnvFile(filePath) {
  if (!existsSync(filePath)) return;

  const contents = await readFile(filePath, "utf8");
  for (const rawLine of contents.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) continue;

    const match = line.match(/^([A-Za-z_][A-Za-z0-9_]*)=(.*)$/);
    if (!match) continue;

    const [, key, rawValue] = match;
    if (process.env[key]) continue;

    let value = rawValue.trim();
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    process.env[key] = value;
  }
}

async function loadEnv() {
  await loadEnvFile(path.join(projectRoot, ".env"));
  await loadEnvFile(path.join(projectRoot, ".env.local"));
  await loadEnvFile(path.join(docsRoot, ".env"));
  await loadEnvFile(path.join(docsRoot, ".env.local"));
}

function getBase64Payload(response) {
  const image = response?.data?.[0];
  return image?.b64_json ?? image?.b64Json;
}

function isUnsupportedSizeError(error) {
  const message = `${error?.message ?? ""} ${error?.error?.message ?? ""}`.toLowerCase();
  return message.includes("size") || message.includes("resolution");
}

function isUnsupportedResponseFormatError(error) {
  const message = `${error?.message ?? ""} ${error?.error?.message ?? ""}`.toLowerCase();
  return message.includes("response_format") || message.includes("response format");
}

async function generateWithFallbacks(client, diagram) {
  let lastError;

  for (const size of sizes) {
    const baseRequest = {
      model,
      prompt: diagram.prompt,
      size,
      response_format: "b64_json",
    };

    try {
      const response = await client.images.generate(baseRequest);
      return { base64: getBase64Payload(response), size };
    } catch (error) {
      lastError = error;

      if (isUnsupportedResponseFormatError(error)) {
        const response = await client.images.generate({
          model,
          prompt: diagram.prompt,
          size,
        });
        return { base64: getBase64Payload(response), size };
      }

      if (!isUnsupportedSizeError(error)) {
        throw error;
      }

      console.warn(`  Size ${size} was rejected; trying the next available size.`);
    }
  }

  throw lastError;
}

async function validatePng(filePath) {
  const handle = await readFile(filePath);
  return handle.length > pngSignature.length && handle.subarray(0, 8).equals(pngSignature);
}

async function generateDiagram(client, diagram) {
  const { base64, size } = await generateWithFallbacks(client, diagram);
  if (!base64) {
    throw new Error("OpenAI response did not include a base64 PNG payload.");
  }

  const imageBuffer = Buffer.from(base64, "base64");
  await writeFile(diagram.outputPath, imageBuffer);

  if (!(await validatePng(diagram.outputPath))) {
    throw new Error(`Generated file is not a valid PNG: ${diagram.outputPath}`);
  }

  const { size: fileSize } = await stat(diagram.outputPath);
  return { size, fileSize };
}

async function main() {
  await loadEnv();
  await mkdir(outputDir, { recursive: true });

  if (!process.env.OPENAI_API_KEY) {
    const message =
      "OPENAI_API_KEY is not set in the shell environment, docs-site/.env, docs-site/.env.local, project .env, or project .env.local.";
    for (const diagram of diagrams) {
      console.error(`Generating ${diagram.name}... failed`);
      console.error(`  ${message}`);
    }
    throw new Error(`${diagrams.length} diagrams failed to generate.`);
  }

  const client = new OpenAI({ apiKey: process.env.OPENAI_API_KEY });
  let failures = 0;

  for (const diagram of diagrams) {
    process.stdout.write(`Generating ${diagram.name}... `);
    try {
      const result = await generateDiagram(client, diagram);
      console.log(`success (${result.size}, ${result.fileSize} bytes) -> ${diagram.outputPath}`);
    } catch (error) {
      failures += 1;
      console.error("failed");
      console.error(`  ${error?.message ?? error}`);
    }
  }

  if (failures > 0) {
    throw new Error(`${failures} diagram${failures === 1 ? "" : "s"} failed to generate.`);
  }
}

main().catch((error) => {
  console.error(error?.message ?? error);
  process.exitCode = 1;
});
