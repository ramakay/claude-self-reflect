#!/usr/bin/env node
// journal-smoke.mjs — headless-browser smoke test for the mailbox dream
// journal (journal-v2-mailbox-plan.md §7.0/§7.1).
//
// A dev script, not shipped in the binary, not a build dependency. Drives a
// real Chrome instance over the Chrome DevTools Protocol (CDP) using only
// Node's global `WebSocket` (Node >= 22) — zero npm dependencies. If the
// global `WebSocket` is unavailable, this falls back to
// `npx -y chrome-remote-interface` (see `runWithFallback` below) rather than
// silently failing.
//
// Usage:
//   node scripts/journal-smoke.mjs                 # self-seeds a fixture DB,
//                                                    # renders a report from
//                                                    # it, and checks that.
//   node scripts/journal-smoke.mjs <report.html>    # checks an already
//                                                    # rendered report file
//                                                    # directly (no fixture
//                                                    # DB, no re-render).
//
// Every check prints `PASS <name>` or `FAIL <name> <detail>`. Exits non-zero
// on any failure.

import { spawn, spawnSync } from "node:child_process";
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  readFileSync,
  rmSync,
  existsSync,
  openSync,
  ftruncateSync,
  closeSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const ENGINE_DIR = path.dirname(SCRIPT_DIR);
const CHROME_BIN =
  process.env.CSR_SMOKE_CHROME ||
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const BINARY =
  process.env.CSR_SMOKE_BINARY || path.join(ENGINE_DIR, "target", "release", "csr-engine");
const DEBUG_PORT = Number(process.env.CSR_SMOKE_PORT || 9222);

if (typeof WebSocket === "undefined") {
  console.error(
    "FAIL harness global WebSocket is unavailable (need Node >= 22) — " +
      "falling back to `npx -y chrome-remote-interface` is not implemented " +
      "in this harness revision; upgrade Node instead.",
  );
  process.exit(1);
}

let failures = 0;
function check(name, ok, detail) {
  if (ok) {
    console.log(`PASS ${name}`);
  } else {
    failures += 1;
    console.log(`FAIL ${name}${detail ? " " + detail : ""}`);
  }
}

// --- fixture DB -------------------------------------------------------

/** Short-oid-style id: the Rust renderer takes the first 8 chars of the raw
 * session id verbatim (no hashing), so fixture ids are kept <= 8 chars where
 * a stable `data-session` value is asserted against below. */
function sqlString(value) {
  return `'${String(value).replace(/'/g, "''")}'`;
}

function isoDaysAgo(days, hour, minute) {
  const now = new Date();
  const day = new Date(
    Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate() - days, hour, minute, 0),
  );
  return day.toISOString().replace(/\.\d{3}Z$/, "Z");
}

function buildFixtureSql() {
  const episodes = [
    // id, days-ago, hour, minute, project, request, outcome, todos, investigated
    {
      id: "rich1",
      daysAgo: 0,
      hour: 20,
      minute: 0,
      project: "proj-alpha",
      request: "Ship the mailbox layout",
      outcome: "Shipped to production",
      todos: [
        ["wire the sort controls", "completed"],
        ["wire the thin rollup", "pending"],
      ],
      investigated: ["/repo/src/dream/report.rs", "/repo/src/dream/report_template.html.jinja"],
      artifacts: ["render_episode", "render_pipe"],
    },
    {
      id: "rich2",
      daysAgo: 0,
      hour: 9,
      minute: 0,
      project: "proj-beta",
      request: "Debug the flaky import test",
      outcome: "Failed after three attempts",
      todos: [],
      investigated: [],
      artifacts: [],
    },
    {
      id: "rich3",
      daysAgo: 1,
      hour: 14,
      minute: 0,
      project: "proj-alpha",
      request: "Investigate the cache miss",
      outcome: "partial - two of four fixed",
      todos: [["fix the primary miss", "completed"]],
      investigated: ["/repo/src/cache.rs"],
      artifacts: [],
    },
    {
      id: "rich4",
      daysAgo: 1,
      hour: 8,
      minute: 0,
      project: "proj-gamma",
      request: "Rewrite the onboarding copy",
      outcome: "shipped and merged",
      todos: [],
      investigated: [],
      artifacts: [],
    },
    {
      id: "rich5",
      daysAgo: 2,
      hour: 11,
      minute: 0,
      project: "proj-beta",
      request: "Look at the noisy alert",
      outcome: "parked for review",
      todos: [],
      investigated: [],
      artifacts: [],
    },
    {
      id: "rich6",
      daysAgo: 2,
      hour: 7,
      minute: 0,
      project: "proj-gamma",
      request: "Fix the broken webhook",
      outcome: "failed: webhook still 500s",
      todos: [],
      investigated: [],
      artifacts: [],
    },
    // Phase 2 (§7.2): one instrumented session — tool-verified error count,
    // top errors, mid-flight steers (5 reported, only 3 stored, exercising
    // the cap), tasks, files, and artifacts all present together so a
    // single row exercises the full four-segment instrumentation line.
    {
      id: "rich7",
      daysAgo: 0,
      hour: 21,
      minute: 0,
      project: "proj-alpha",
      request: "Wire the queue worker retry path",
      outcome: "Shipped the queue worker",
      todos: [["wire the retry path", "completed"]],
      investigated: ["/repo/src/queue.rs"],
      artifacts: ["queue_worker"],
      instrumentation: {
        error_count: 2,
        top_errors: [
          { turn: 5, tool: "Bash", preview: "npm test failed: 3 tests" },
          { turn: 9, tool: "Read", preview: "file not found" },
        ],
        steer_count: 5,
        steers: [
          { turn: 3, text: "no, use bash not zsh" },
          { turn: 7, text: "actually revert that" },
          { turn: 12, text: "retry with backoff instead" },
        ],
        // Delta-review F2: only a version-stamped total (measured by the
        // current steer-filter generation) may render beyond the persisted
        // quotes — unstamped legacy episodes show the surviving-quote count.
        instrumentation_version: 2,
      },
    },
    // Phase 3 (§7.3): a two-sentence request — the fused sentence's first
    // clause covers "Traced the TTS chain...", leaving a genuine second
    // sentence as the detail-pane subtitle. "rich1" through "rich7" are all
    // single-clause asks, so their deterministic subtitle is empty and the
    // element must be entirely absent (no empty <p>).
    {
      id: "rich8",
      daysAgo: 0,
      hour: 22,
      minute: 0,
      project: "proj-alpha",
      request:
        "Traced the TTS chain and found the hindi voice id unset. Fell back to the english preset twice.",
      outcome: "Shipped the hindi voice fix",
      todos: [],
      investigated: [],
      artifacts: [],
    },
    // Journal v2 Phase 5: a verdict-bearing session for the SINCE THEN
    // payback panel — no episode_anchors artifacts here, since the panel's
    // primary symbol source is `code_node_attribution` (seeded separately
    // below via `sinceThenSql`), and mixing the two would make the "which
    // source won" assertion ambiguous.
    {
      id: "rich9",
      daysAgo: 0,
      hour: 23,
      minute: 0,
      project: "proj-alpha",
      request: "Refactor the retry backoff",
      outcome: "shipped the backoff fix",
      todos: [["adjust compute_delay clamp", "pending"]],
      investigated: [],
      artifacts: [],
    },
  ];
  // Phase 2 (§7.2): "rich2" is deliberately left uninstrumented at the
  // episode layer (no `instrumentation` above) AND given an oversized
  // on-disk transcript below (see `makeOversizedTranscript`) — the
  // report-time backfill must resolve it, decline to parse it, and mark it
  // "not scanned" without ever fabricating an errors segment for it.
  // "rich2" also carries no code_node_attribution/episode_anchors rows at
  // all, making it the SINCE THEN "absent" fixture case.

  const thinEntries = [
    { id: "thin1", daysAgo: 0, hour: 7, minute: 0, project: "thin-alpha", prompt: "check the logs" },
    { id: "thin2", daysAgo: 0, hour: 7, minute: 30, project: "thin-alpha", prompt: "nvm" },
    { id: "thin3", daysAgo: 1, hour: 6, minute: 0, project: "thin-beta", prompt: "quick look" },
    { id: "thin4", daysAgo: 1, hour: 6, minute: 15, project: "thin-beta", prompt: "" },
    { id: "thin5", daysAgo: 2, hour: 5, minute: 0, project: "thin-gamma", prompt: "spot check" },
  ];

  const lines = [];
  for (const ep of episodes) {
    const ts = isoDaysAgo(ep.daysAgo, ep.hour, ep.minute);
    const content = JSON.stringify({
      schema: "v2",
      session_id: ep.id,
      project: ep.project,
      timestamp: ts,
      request: ep.request,
      outcome: ep.outcome,
      investigated: ep.investigated,
      todos: ep.todos.map(([c, s]) => ({ content: c, status: s })),
      // Phase 2 (§7.2) instrumentation feeds — only present on episodes that
      // opt in via `ep.instrumentation`; every other episode deserializes
      // these as their honest `None`/empty defaults.
      ...(ep.instrumentation || {}),
    });
    const tags = JSON.stringify(["session_episode", "schema_v2", `conv_${ep.id}`]);
    lines.push(
      `INSERT INTO reflections (id, content, tags, timestamp) VALUES (${sqlString(
        "episode-" + ep.id,
      )}, ${sqlString(content)}, ${sqlString(tags)}, ${sqlString(ts)});`,
    );
    for (const symbol of ep.artifacts) {
      lines.push(
        `INSERT INTO episode_anchors (session_id, project, file, node_kind, name, body_hash, created_at) VALUES (${sqlString(
          ep.id,
        )}, ${sqlString(ep.project)}, '/repo/src/lib.rs', 'function_item', ${sqlString(
          symbol,
        )}, ${sqlString("hash-" + symbol)}, ${sqlString(ts)});`,
      );
    }
  }
  for (const thin of thinEntries) {
    const ts = isoDaysAgo(thin.daysAgo, thin.hour, thin.minute);
    lines.push(
      `INSERT INTO session_registry (session_id, project, first_prompt, first_ts, last_ts, prompt_count) VALUES (${sqlString(
        thin.id,
      )}, ${sqlString(thin.project)}, ${sqlString(thin.prompt)}, ${sqlString(ts)}, ${sqlString(
        ts,
      )}, 1);`,
    );
  }

  // Journal v2 Phase 5 SINCE THEN fixture: "rich9" touched three symbols via
  // `code_node_attribution` (not `episode_anchors`) — one superseded (with a
  // receipt), one witnessed-but-unverdicted ("still live"), one attributed
  // but never witnessed at all ("unverified"). The superseded verdict is
  // dated the same calendar day as rich9's own episode timestamp, so the
  // day-header digest ("while away: N anchors retired") has real evidence.
  const rich9Ts = isoDaysAgo(0, 23, 0);
  const rich9Day = rich9Ts.slice(0, 10);
  lines.push(
    `INSERT INTO code_nodes (id, project, file, kind, name, first_conv_id, last_conv_id) VALUES
      ('node-rich9-retry_backoff', 'proj-alpha', '/repo/src/retry.rs', 'function', 'retry_backoff', '', ''),
      ('node-rich9-compute_delay', 'proj-alpha', '/repo/src/retry.rs', 'function', 'compute_delay', '', ''),
      ('node-rich9-parse_config', 'proj-alpha', '/repo/src/retry.rs', 'function', 'parse_config', '', '');`,
    `INSERT INTO code_node_attribution (node_id, channel, source_id, observed_ts, evidence) VALUES
      ('node-rich9-retry_backoff', 'transcript', 'rich9', ${sqlString(rich9Ts)}, 'coedit_event'),
      ('node-rich9-compute_delay', 'transcript', 'rich9', ${sqlString(rich9Ts)}, 'coedit_event'),
      ('node-rich9-parse_config', 'transcript', 'rich9', ${sqlString(rich9Ts)}, 'coedit_event');`,
    `INSERT INTO witness_ledger
        (project, file, symbol, span_start, span_end, stamp, tier, at_oid, source_kind, source_id)
     VALUES
      ('proj-alpha', '/repo/src/retry.rs', 'retry_backoff', 1, 3, 'b3:retry_backoff', 'committed', 'oldoid-retry', 'backfill', 'oldoid-retry'),
      ('proj-alpha', '/repo/src/retry.rs', 'compute_delay', 1, 3, 'b3:compute_delay', 'committed', 'oldoid-delay', 'backfill', 'oldoid-delay');`,
    `INSERT INTO witness_verdicts
        (witness_id, verdict, successor_witness_id, receipt_oid, observed_head_oid, created_at)
     VALUES (
       (SELECT id FROM witness_ledger WHERE symbol = 'retry_backoff'),
       'superseded_by', NULL, 'deadbeefcafe0123', 'headoid',
       ${sqlString(rich9Day + " 12:00:00")}
     );`,
  );

  return { sql: lines.join("\n") + "\n", episodes, thinEntries, rich9Day };
}

/** 64 MiB + 1 byte — one past `MAX_TRANSCRIPT_SCAN_BYTES` (must track
 * `csr-engine/src/transcript/instrumentation.rs`). A sparse `ftruncateSync`
 * makes the file report this size without writing real bytes to disk. */
const OVERSIZED_TRANSCRIPT_BYTES = 64 * 1024 * 1024 + 1;

/** Phase 2 (§7.2): create a transcript file on disk that exists but is too
 * large for the report-time backfill to parse — the fixture case for the
 * "⚠ not scanned" warning chip. */
function makeOversizedTranscript(projectsDir, project, sessionId) {
  const dir = path.join(projectsDir, project);
  mkdirSync(dir, { recursive: true });
  const filePath = path.join(dir, `${sessionId}.jsonl`);
  const fd = openSync(filePath, "w");
  try {
    ftruncateSync(fd, OVERSIZED_TRANSCRIPT_BYTES);
  } finally {
    closeSync(fd);
  }
}

function runBinary(args, env) {
  const result = spawnSync(BINARY, args, {
    encoding: "utf8",
    env: { ...process.env, CSR_NO_AI_NARRATIVES: "1", ...env },
  });
  if (result.status !== 0) {
    throw new Error(
      `csr-engine ${args.join(" ")} exited ${result.status}\nstdout: ${result.stdout}\nstderr: ${result.stderr}`,
    );
  }
  return result;
}

function seedFixtureAndRender(workDir) {
  const dbPath = path.join(workDir, "fixture.db");
  const projectsDir = path.join(workDir, "projects");
  const reportPath = path.join(workDir, "journal-fixture.html");

  if (!existsSync(BINARY)) {
    throw new Error(
      `csr-engine release binary not found at ${BINARY} — run \`cargo build --release\` first`,
    );
  }

  // Bootstrap: opening the (nonexistent) db path runs migrations and creates
  // an empty, schema-complete database — the same path `Storage::open` takes
  // for a brand-new install.
  runBinary(["--db-path", dbPath, "--projects-dir", projectsDir, "dream", "--report", "--no-open", "--out", path.join(workDir, "bootstrap.html")]);

  const { sql, episodes, thinEntries, rich9Day } = buildFixtureSql();
  const sqlPath = path.join(workDir, "fixture.sql");
  writeFileSync(sqlPath, sql);
  const seed = spawnSync("sqlite3", [dbPath], { input: sql, encoding: "utf8" });
  if (seed.status !== 0) {
    throw new Error(`sqlite3 seeding failed: ${seed.stderr}`);
  }

  // Phase 2 (§7.2): "rich2" (proj-beta) has no episode-level instrumentation
  // — give it an on-disk transcript too large to scan, so the report-time
  // backfill must resolve it, decline to parse it, and mark it "not
  // scanned" rather than fabricate an errors segment for it.
  makeOversizedTranscript(projectsDir, "proj-beta", "rich2");

  // Real render against the seeded fixture — this is the file the headless
  // checks below actually load.
  runBinary([
    "--db-path",
    dbPath,
    "--projects-dir",
    projectsDir,
    "dream",
    "--report",
    "--no-open",
    "--out",
    reportPath,
  ]);

  return { reportPath, episodes, thinEntries, rich9Day };
}

// --- Journal v3 "Dreams Gateway" Phase 2 fixture -------------------------
//
// A dedicated, small DB — kept separate from `buildFixtureSql`'s mailbox
// fixture (which, as seeded today, qualifies zero dream items) so the new
// gateway checks below have real evidence-backed cards to click through,
// without perturbing a single existing mailbox assertion. Two projects, one
// item each: "dream-alpha" is item-grade (the todo names the witnessed
// symbol verbatim in a backtick span), "dream-beta" is session-grade (the
// todo names nothing code-shaped, but the episode's `files_modified`
// overlaps a witnessed file) — exercising both grade channels and both
// card/detail render paths in one render.
function buildDreamFixtureSql() {
  const alphaLeftOpen = isoDaysAgo(1, 10, 0);
  const betaLeftOpen = isoDaysAgo(2, 9, 0);
  const witnessedAt = isoDaysAgo(0, 3, 0);

  const alphaContent = JSON.stringify({
    schema: "v2",
    session_id: "dream-alpha-1",
    project: "dream-alpha",
    timestamp: alphaLeftOpen,
    todos: [{ content: "fix `parse_widget` before ship", status: "pending" }],
  });
  const betaContent = JSON.stringify({
    schema: "v2",
    session_id: "dream-beta-1",
    project: "dream-beta",
    timestamp: betaLeftOpen,
    todos: [{ content: "investigate the onboarding drop-off", status: "pending" }],
    files_modified: ["/repo/src/beta_flow.rs"],
  });

  const lines = [
    `INSERT INTO reflections (id, content, tags, timestamp) VALUES (${sqlString(
      "episode-dream-alpha-1",
    )}, ${sqlString(alphaContent)}, ${sqlString(JSON.stringify(["session_episode", "schema_v2"]))}, ${sqlString(alphaLeftOpen)});`,
    `INSERT INTO reflections (id, content, tags, timestamp) VALUES (${sqlString(
      "episode-dream-beta-1",
    )}, ${sqlString(betaContent)}, ${sqlString(JSON.stringify(["session_episode", "schema_v2"]))}, ${sqlString(betaLeftOpen)});`,
    `INSERT INTO witness_ledger
        (project, file, symbol, span_start, span_end, stamp, tier, at_oid, source_kind, source_id)
     VALUES
      ('dream-alpha', '/repo/src/widget.rs', 'parse_widget', 1, 3, 'b3:dream-alpha-1', 'committed', 'oldoid-alpha', 'backfill', 'oldoid-alpha'),
      ('dream-beta', '/repo/src/beta_flow.rs', 'handle_flow', 1, 3, 'b3:dream-beta-1', 'committed', 'oldoid-beta', 'backfill', 'oldoid-beta');`,
    `INSERT INTO witness_verdicts
        (witness_id, verdict, successor_witness_id, receipt_oid, observed_head_oid, created_at)
     VALUES
      ((SELECT id FROM witness_ledger WHERE symbol = 'parse_widget'), 'superseded_by', NULL, 'deadbeef01', 'headoid-a', ${sqlString(
        witnessedAt,
      )}),
      ((SELECT id FROM witness_ledger WHERE symbol = 'handle_flow'), 'anchor_obsolete', NULL, 'cafebabe02', 'headoid-b', ${sqlString(
        witnessedAt,
      )});`,
  ];
  return { sql: lines.join("\n") + "\n" };
}

function seedDreamFixtureAndRender(workDir) {
  const dbPath = path.join(workDir, "dream-fixture.db");
  const projectsDir = path.join(workDir, "dream-projects");
  const reportPath = path.join(workDir, "dream-fixture.html");

  runBinary([
    "--db-path",
    dbPath,
    "--projects-dir",
    projectsDir,
    "dream",
    "--report",
    "--no-open",
    "--out",
    path.join(workDir, "dream-bootstrap.html"),
  ]);

  const { sql } = buildDreamFixtureSql();
  const seed = spawnSync("sqlite3", [dbPath], { input: sql, encoding: "utf8" });
  if (seed.status !== 0) {
    throw new Error(`sqlite3 dream-fixture seeding failed: ${seed.stderr}`);
  }

  runBinary([
    "--db-path",
    dbPath,
    "--projects-dir",
    projectsDir,
    "dream",
    "--report",
    "--no-open",
    "--out",
    reportPath,
  ]);

  return { reportPath };
}

// --- minimal CDP client -------------------------------------------------

class CDP {
  constructor(wsUrl) {
    this.ws = new WebSocket(wsUrl);
    this.nextId = 0;
    this.pending = new Map();
    this.listeners = new Map();
    this.ready = new Promise((resolve, reject) => {
      this.ws.addEventListener("open", () => resolve());
      this.ws.addEventListener("error", (e) => reject(e));
    });
    this.ws.addEventListener("message", (event) => {
      const msg = JSON.parse(event.data);
      if (msg.id !== undefined && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        if (msg.error) reject(new Error(JSON.stringify(msg.error)));
        else resolve(msg.result);
      } else if (msg.method) {
        const handlers = this.listeners.get(msg.method) || [];
        for (const handler of handlers) handler(msg.params);
      }
    });
  }

  async send(method, params = {}) {
    await this.ready;
    const id = ++this.nextId;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  on(method, handler) {
    if (!this.listeners.has(method)) this.listeners.set(method, []);
    this.listeners.get(method).push(handler);
  }

  /** Resolve the next time `method` (a CDP *event*, not a command) fires. */
  once(method) {
    return new Promise((resolve) => {
      const handler = (params) => {
        const handlers = this.listeners.get(method) || [];
        const idx = handlers.indexOf(handler);
        if (idx !== -1) handlers.splice(idx, 1);
        resolve(params);
      };
      this.on(method, handler);
    });
  }

  close() {
    try {
      this.ws.close();
    } catch {
      /* best effort */
    }
  }
}

/** Evaluate `expression` in the page and return its JSON-serializable value. */
async function evalJS(cdp, expression) {
  const result = await cdp.send("Runtime.evaluate", {
    expression,
    returnByValue: true,
    awaitPromise: true,
  });
  if (result.exceptionDetails) {
    throw new Error(
      `page eval failed: ${result.exceptionDetails.text} — ${JSON.stringify(
        result.exceptionDetails.exception,
      )}\nexpression: ${expression}`,
    );
  }
  return result.result.value;
}

async function waitFor(cdp, expression, timeoutMs = 3000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const value = await evalJS(cdp, expression);
    if (value) return value;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  throw new Error(`timed out waiting for: ${expression}`);
}

async function fetchJson(url) {
  const response = await fetch(url);
  return response.json();
}

async function main() {
  const workDir = mkdtempSync(path.join(tmpdir(), "csr-journal-smoke-"));
  let reportPath = process.argv[2];
  let fixture = null;
  try {
    if (!reportPath) {
      const seeded = seedFixtureAndRender(workDir);
      reportPath = seeded.reportPath;
      fixture = seeded;
    }
    reportPath = path.resolve(reportPath);
    if (!existsSync(reportPath)) {
      throw new Error(`report file not found: ${reportPath}`);
    }

    const userDataDir = mkdtempSync(path.join(tmpdir(), "csr-journal-chrome-"));
    const chrome = spawn(
      CHROME_BIN,
      [
        "--headless=new",
        `--remote-debugging-port=${DEBUG_PORT}`,
        "--disable-gpu",
        "--no-first-run",
        "--no-default-browser-check",
        `--user-data-dir=${userDataDir}`,
        "about:blank",
      ],
      { stdio: "ignore" },
    );

    try {
      let target = null;
      const deadline = Date.now() + 10000;
      while (Date.now() < deadline) {
        try {
          const list = await fetchJson(`http://127.0.0.1:${DEBUG_PORT}/json/list`);
          target = list.find((t) => t.type === "page");
          if (target) break;
        } catch {
          /* Chrome not ready yet */
        }
        await new Promise((resolve) => setTimeout(resolve, 100));
      }
      if (!target) throw new Error("Chrome DevTools endpoint never became ready");

      const cdp = new CDP(target.webSocketDebuggerUrl);
      await cdp.ready;

      const requests = [];
      cdp.on("Network.requestWillBeSent", (params) => requests.push(params.request.url));
      const consoleErrors = [];
      cdp.on("Runtime.consoleAPICalled", (params) => {
        if (params.type === "error") consoleErrors.push(params.args);
      });
      const exceptions = [];
      cdp.on("Runtime.exceptionThrown", (params) => exceptions.push(params));

      await cdp.send("Network.enable");
      await cdp.send("Runtime.enable");
      await cdp.send("Page.enable");

      const fileUrl = "file://" + reportPath;
      const navigated = cdp.once("Page.loadEventFired");
      await cdp.send("Page.navigate", { url: fileUrl });
      await navigated;
      // Give the mailbox script's initial listeners a moment to attach.
      await new Promise((resolve) => setTimeout(resolve, 150));

      // ---- counts ---------------------------------------------------
      const counts = await evalJS(
        cdp,
        `({
          indexRows: document.querySelectorAll(".index-row").length,
          detailPanes: document.querySelectorAll(".detail-pane").length,
          thinGroups: document.querySelectorAll(".thin-rollup details").length,
          visibleStageCards: document.querySelectorAll(".detail-pane:not([hidden]) .stage-card").length,
        })`,
      );
      const expectRich = fixture ? fixture.episodes.length : counts.indexRows;
      const expectThinGroups = fixture
        ? new Set(fixture.thinEntries.map((t) => t.project)).size
        : counts.thinGroups;
      check(
        "counts",
        counts.indexRows === expectRich &&
          counts.detailPanes === expectRich &&
          counts.thinGroups === expectThinGroups &&
          counts.visibleStageCards > 0,
        JSON.stringify(counts),
      );

      // ---- no-external-requests --------------------------------------
      const external = requests.filter((url) => !url.startsWith("file:") && !url.startsWith("data:"));
      check("no-external-requests", external.length === 0, JSON.stringify(external));

      // ---- console-clean ----------------------------------------------
      check(
        "console-clean",
        consoleErrors.length === 0 && exceptions.length === 0,
        JSON.stringify({ consoleErrors, exceptions }),
      );

      // ---- default-selection -------------------------------------------
      const initialSelection = await evalJS(
        cdp,
        `(() => {
          const visible = document.querySelectorAll(".detail-pane:not([hidden])");
          const rows = Array.from(document.querySelectorAll(".index-row"));
          const newest = rows.reduce((best, row) => (!best || row.dataset.ts > best.dataset.ts ? row : best), null);
          return {
            visibleCount: visible.length,
            visibleSession: visible.length === 1 ? visible[0].dataset.session : null,
            newestSession: newest ? newest.dataset.session : null,
          };
        })()`,
      );
      check(
        "default-selection",
        initialSelection.visibleCount === 1 &&
          initialSelection.visibleSession === initialSelection.newestSession,
        JSON.stringify(initialSelection),
      );

      // ---- pane-swap -----------------------------------------------------
      const paneSwap = await evalJS(
        cdp,
        `(() => {
          const rows = Array.from(document.querySelectorAll(".index-row"));
          if (rows.length < 4) return { skipped: true };
          const before = rows[0].dataset.session;
          rows[3].click();
          const visible = document.querySelectorAll(".detail-pane:not([hidden])");
          const selectedRows = document.querySelectorAll(".index-row[aria-selected=\\"true\\"]");
          const target = rows[3].dataset.session;
          const beforePane = document.querySelector('.detail-pane[data-session="' + before + '"]');
          const targetPane = document.querySelector('.detail-pane[data-session="' + target + '"]');
          const hashOk = location.hash.slice(1) === target;
          const result = {
            visibleCount: visible.length,
            selectedCount: selectedRows.length,
            targetVisible: targetPane && !targetPane.hidden,
            beforeHidden: beforePane && beforePane.hidden,
            hashOk,
          };
          history.back();
          return result;
        })()`,
      );
      await new Promise((resolve) => setTimeout(resolve, 150));
      const backRestored = await evalJS(
        cdp,
        `(() => {
          const rows = Array.from(document.querySelectorAll(".index-row"));
          const first = rows[0];
          const pane = document.querySelector('.detail-pane[data-session="' + first.dataset.session + '"]');
          return pane && !pane.hidden;
        })()`,
      );
      check(
        "pane-swap",
        paneSwap.skipped ||
          (paneSwap.visibleCount === 1 &&
            paneSwap.selectedCount === 1 &&
            paneSwap.targetVisible &&
            paneSwap.beforeHidden &&
            paneSwap.hashOk &&
            backRestored),
        JSON.stringify({ paneSwap, backRestored }),
      );

      // ---- sort-recency (default, before any sort click) -----------------
      const recencyState = await evalJS(
        cdp,
        `(() => {
          const rows = Array.from(document.querySelectorAll(".index-row"));
          const ts = rows.map(r => r.dataset.ts);
          const nonIncreasing = ts.every((t, i) => i === 0 || t <= ts[i - 1]);
          const headers = Array.from(document.querySelectorAll(".group-header")).map(h => h.textContent);
          return { nonIncreasing, headerCount: headers.length, firstHeader: headers[0] || null, sequence: rows.map(r => r.dataset.session) };
        })()`,
      );
      check(
        "sort-recency",
        recencyState.nonIncreasing && recencyState.headerCount > 0,
        JSON.stringify(recencyState),
      );
      const initialSequence = recencyState.sequence;

      // ---- day-digest (journal v2 Phase 5) -----------------------------------
      // Server-rendered only, in the default recency view before any sort
      // button is clicked — the client-side JS re-sort (used for group/type,
      // and for clicking back to recency) rebuilds `.group-header`s from
      // scratch with no digest data at all, so this must run before
      // `sort-group` below touches anything. Exactly one day-header in the
      // fixture carries real dream activity (rich9's superseded verdict,
      // dated the same day as its episode) — every other day-header must
      // show no digest line.
      const dayDigest = await evalJS(
        cdp,
        `(() => {
          const headers = Array.from(document.querySelectorAll(".group-header"));
          const digests = headers.map((header) => {
            const next = header.nextElementSibling;
            return next && next.classList.contains("dream-digest") ? next.textContent : null;
          });
          return { digestCount: digests.filter(Boolean).length, digests };
        })()`,
      );
      check(
        "day-digest",
        dayDigest.digestCount === 1 && /1 anchor retired/.test(dayDigest.digests.find(Boolean) || ""),
        JSON.stringify(dayDigest),
      );

      // ---- sort-group ------------------------------------------------
      await evalJS(cdp, `document.querySelector('[data-sort="group"]').click(); true`);
      const groupState = await evalJS(
        cdp,
        `(() => {
          const rows = Array.from(document.querySelectorAll(".index-row"));
          const projects = rows.map(r => r.dataset.project);
          let switches = 0;
          for (let i = 1; i < projects.length; i++) if (projects[i] !== projects[i - 1]) switches++;
          const distinctProjects = new Set(projects).size;
          const headers = Array.from(document.querySelectorAll(".group-header")).map(h => h.textContent);
          return { rowCount: rows.length, switches, distinctProjects, headerCount: headers.length, headers };
        })()`,
      );
      check(
        "sort-group",
        groupState.switches + 1 === groupState.distinctProjects &&
          groupState.headerCount === groupState.distinctProjects &&
          (fixture ? groupState.rowCount === fixture.episodes.length : true),
        JSON.stringify(groupState),
      );

      // ---- sort-type ---------------------------------------------------
      await evalJS(cdp, `document.querySelector('[data-sort="type"]').click(); true`);
      const typeState = await evalJS(
        cdp,
        `(() => {
          const order = ["success", "partial", "failed", "noted"];
          const rows = Array.from(document.querySelectorAll(".index-row"));
          const outcomes = rows.map(r => r.dataset.outcome);
          let lastIdx = -1;
          let monotonic = true;
          for (const o of outcomes) {
            const idx = order.indexOf(o);
            if (idx < lastIdx) monotonic = false;
            lastIdx = Math.max(lastIdx, idx);
          }
          const headers = Array.from(document.querySelectorAll(".group-header")).map(h => h.textContent);
          return { rowCount: rows.length, monotonic, headers };
        })()`,
      );
      check(
        "sort-type",
        typeState.monotonic && (fixture ? typeState.rowCount === fixture.episodes.length : true),
        JSON.stringify(typeState),
      );

      // ---- sort-round-trip ------------------------------------------------
      await evalJS(cdp, `document.querySelector('[data-sort="recency"]').click(); true`);
      const roundTripSequence = await evalJS(
        cdp,
        `Array.from(document.querySelectorAll(".index-row")).map(r => r.dataset.session)`,
      );
      check(
        "sort-round-trip",
        JSON.stringify(roundTripSequence) === JSON.stringify(initialSequence),
        JSON.stringify({ roundTripSequence, initialSequence }),
      );

      // ---- sort-preserves-selection ---------------------------------------
      const selectionAcrossSorts = await evalJS(
        cdp,
        `(() => {
          const results = [];
          for (const mode of ["group", "type", "recency"]) {
            document.querySelector('[data-sort="' + mode + '"]').click();
            const visible = document.querySelectorAll(".detail-pane:not([hidden])");
            const selectedRows = document.querySelectorAll('.index-row[aria-selected="true"]');
            results.push({ mode, visibleCount: visible.length, selectedCount: selectedRows.length });
          }
          return results;
        })()`,
      );
      check(
        "sort-preserves-selection",
        selectionAcrossSorts.every((r) => r.visibleCount === 1 && r.selectedCount === 1),
        JSON.stringify(selectionAcrossSorts),
      );

      // ---- pin ------------------------------------------------------------
      // The <details> `toggle` event (which the page uses to sync
      // aria-expanded) is dispatched via a queued task, not synchronously
      // with `.click()` — a real tick (setTimeout) has to elapse before
      // aria-expanded reflects the new `open` state, even though `open`
      // itself flips synchronously.
      const pinResult = await evalJS(
        cdp,
        `(async () => {
          const pane = document.querySelector(".detail-pane:not([hidden])");
          const card = pane.querySelector(".stage-card");
          if (!card) return { skipped: true };
          const wasOpen = card.open;
          card.querySelector("summary").click();
          const toggledOpen = card.open !== wasOpen;
          await new Promise(resolve => setTimeout(resolve, 20));
          const ariaOk = card.getAttribute("aria-expanded") === String(card.open);
          const session = pane.dataset.session;
          // swap away
          const rows = Array.from(document.querySelectorAll(".index-row"));
          const other = rows.find(r => r.dataset.session !== session);
          if (other) other.click();
          // swap back
          const originalRow = rows.find(r => r.dataset.session === session);
          originalRow.click();
          const stateAfter = card.open;
          return { toggledOpen, ariaOk, survivedSwap: stateAfter === card.open };
        })()`,
      );
      check(
        "pin",
        pinResult.skipped || (pinResult.toggledOpen && pinResult.ariaOk && pinResult.survivedSwap),
        JSON.stringify(pinResult),
      );

      // ---- popover ----------------------------------------------------
      const popoverResult = await evalJS(
        cdp,
        `(() => {
          const chip = document.querySelector("[data-popover]");
          if (!chip) return { skipped: true };
          const rect = chip.getBoundingClientRect();
          const event1 = new PointerEvent("pointerenter", { clientX: rect.left + 4, clientY: rect.top + 4, bubbles: true });
          chip.dispatchEvent(event1);
          const popover = document.querySelector(".chip-popover");
          const shownRect = popover.getBoundingClientRect();
          const shownInViewport =
            !popover.hidden &&
            shownRect.left >= 0 &&
            shownRect.top >= 0 &&
            shownRect.right <= window.innerWidth &&
            shownRect.bottom <= window.innerHeight;
          chip.dispatchEvent(new PointerEvent("pointerleave", { bubbles: true }));
          const hiddenAfterLeave = popover.hidden;

          // Repeat with cursor pinned to the bottom-right viewport corner —
          // the case that actually exercises positionPopover's clamp.
          const event2 = new PointerEvent("pointerenter", {
            clientX: window.innerWidth - 2,
            clientY: window.innerHeight - 2,
            bubbles: true,
          });
          chip.dispatchEvent(event2);
          const cornerRect = popover.getBoundingClientRect();
          const cornerInViewport =
            !popover.hidden &&
            cornerRect.left >= 0 &&
            cornerRect.top >= 0 &&
            cornerRect.right <= window.innerWidth + 1 &&
            cornerRect.bottom <= window.innerHeight + 1;
          chip.dispatchEvent(new PointerEvent("pointerleave", { bubbles: true }));
          return { shownInViewport, hiddenAfterLeave, cornerInViewport };
        })()`,
      );
      check(
        "popover",
        popoverResult.skipped ||
          (popoverResult.shownInViewport && popoverResult.hiddenAfterLeave && popoverResult.cornerInViewport),
        JSON.stringify(popoverResult),
      );

      // ---- instrumentation-segments (§7.2, Phase 2) -----------------------
      // "rich7" carries tasks + artifacts + files + tool-verified errors —
      // all four segments in one row. "rich1" carries no error evidence at
      // all and must show no "error(s)" token.
      const instrumentationSegments = await evalJS(
        cdp,
        `(() => {
          const rows = Array.from(document.querySelectorAll(".index-row"));
          const instrumented = rows.find(r => r.dataset.session === "rich7");
          const uninstrumented = rows.find(r => r.dataset.session === "rich1");
          return {
            instrumentedText: instrumented ? instrumented.querySelector(".instrumentation").textContent : null,
            uninstrumentedText: uninstrumented ? uninstrumented.querySelector(".instrumentation").textContent : null,
          };
        })()`,
      );
      const segmentsRe = /✓\d+ ○\d+ tasks · \d+ artifacts? · \d+ files? · \d+ errors?/;
      check(
        "instrumentation-segments",
        Boolean(instrumentationSegments.instrumentedText) &&
          segmentsRe.test(instrumentationSegments.instrumentedText) &&
          instrumentationSegments.uninstrumentedText !== null &&
          !/errors?/.test(instrumentationSegments.uninstrumentedText),
        JSON.stringify(instrumentationSegments),
      );

      // ---- steer-lines (§7.2, Phase 2) -------------------------------------
      // "rich7" reports steer_count=5 but only stores 3 steers — the STEER
      // stage must show the true count line and exactly min(5, 3) = 3
      // "↳ turn N" children.
      const steerLines = await evalJS(
        cdp,
        `(() => {
          document.querySelector('.index-row[data-session="rich7"]').click();
          const pane = document.querySelector('.detail-pane[data-session="rich7"]');
          const steerCard = pane ? pane.querySelector(".stage-card.steer") : null;
          const countEl = steerCard ? steerCard.querySelector(".steer-count") : null;
          return {
            countText: countEl ? countEl.textContent : null,
            lineCount: steerCard ? steerCard.querySelectorAll(".steer-line").length : 0,
          };
        })()`,
      );
      check(
        "steer-lines",
        Boolean(steerLines.countText) &&
          steerLines.countText.includes("5") &&
          steerLines.lineCount === 3,
        JSON.stringify(steerLines),
      );

      // ---- error-popover (§7.2, Phase 2) -----------------------------------
      // Hovering "rich7"'s errors chip must show the top-error preview,
      // clamped fully inside the viewport (same clamp mechanism as the
      // generic `popover` check above, targeted at the errors segment).
      const errorPopover = await evalJS(
        cdp,
        `(() => {
          const row = document.querySelector('.index-row[data-session="rich7"]');
          const chips = row ? Array.from(row.querySelectorAll(".chip")) : [];
          const errorChip = chips.find(c => /errors?/.test(c.textContent) && c.dataset.popover);
          if (!errorChip) return { skipped: true };
          const rect = errorChip.getBoundingClientRect();
          errorChip.dispatchEvent(new PointerEvent("pointerenter", { clientX: rect.left + 2, clientY: rect.top + 2, bubbles: true }));
          const popover = document.querySelector(".chip-popover");
          const shown = !popover.hidden;
          const text = popover.textContent;
          const shownRect = popover.getBoundingClientRect();
          const inViewport =
            shownRect.left >= 0 &&
            shownRect.top >= 0 &&
            shownRect.right <= window.innerWidth &&
            shownRect.bottom <= window.innerHeight;
          errorChip.dispatchEvent(new PointerEvent("pointerleave", { bubbles: true }));
          return { shown, text, inViewport };
        })()`,
      );
      check(
        "error-popover",
        errorPopover.skipped ||
          (errorPopover.shown && errorPopover.inViewport && /Bash|Read/.test(errorPopover.text || "")),
        JSON.stringify(errorPopover),
      );

      // ---- warning-chip (§7.2, Phase 2) ------------------------------------
      // "rich2" has an oversized on-disk transcript (see
      // `makeOversizedTranscript`): the backfill resolves it, declines to
      // parse it, and must show "⚠ not scanned". "rich1" has no transcript
      // on disk at all and must show no chip (plan §3.4 row 1: nothing to
      // disclose "not scanned" about).
      const warningChip = await evalJS(
        cdp,
        `(() => {
          document.querySelector('.index-row[data-session="rich2"]').click();
          const scannedPane = document.querySelector('.detail-pane[data-session="rich2"]');
          const hasWarnOversized = !!(scannedPane && scannedPane.querySelector(".warn-chip"));
          document.querySelector('.index-row[data-session="rich1"]').click();
          const otherPane = document.querySelector('.detail-pane[data-session="rich1"]');
          const hasWarnNoTranscript = !!(otherPane && otherPane.querySelector(".warn-chip"));
          return { hasWarnOversized, hasWarnNoTranscript };
        })()`,
      );
      check(
        "warning-chip",
        warningChip.hasWarnOversized && !warningChip.hasWarnNoTranscript,
        JSON.stringify(warningChip),
      );

      // ---- one-line-index (§7.3, Phase 3) ----------------------------------
      // The index row's fused sentence must render as at most 3 visual lines
      // (`-webkit-line-clamp: 3`), and no `.description` element may appear
      // anywhere inside `.index-row` — the subtitle is a detail-pane-only
      // surface.
      const oneLineIndex = await evalJS(
        cdp,
        `(() => {
          const rows = Array.from(document.querySelectorAll(".index-row"));
          const lineHeightCap = rows.map((row) => {
            const sentence = row.querySelector(".sentence");
            if (!sentence) return { skipped: true };
            const style = getComputedStyle(sentence);
            const lineHeight = parseFloat(style.lineHeight) || sentence.clientHeight;
            const maxLines = lineHeight > 0 ? sentence.scrollHeight / lineHeight : 0;
            return { session: row.dataset.session, maxLines };
          });
          const anyDescription = rows.some((row) => row.querySelector(".description"));
          return { lineHeightCap, anyDescription };
        })()`,
      );
      const withinThreeLines = oneLineIndex.lineHeightCap.every(
        (r) => r.skipped || r.maxLines <= 3.5,
      );
      check(
        "one-line-index",
        withinThreeLines && !oneLineIndex.anyDescription,
        JSON.stringify(oneLineIndex),
      );

      // ---- detail-subtitle (§7.3, Phase 3) ---------------------------------
      // "rich8" has a genuine second sentence in its request, so its detail
      // pane must show a `.description` subtitle under the fused sentence.
      // "rich1" is a single-clause ask, so its deterministic subtitle is
      // empty and the element must be entirely absent — never an empty `<p>`.
      const detailSubtitle = await evalJS(
        cdp,
        `(() => {
          document.querySelector('.index-row[data-session="rich8"]').click();
          const withSubtitlePane = document.querySelector('.detail-pane[data-session="rich8"]');
          const subtitleEl = withSubtitlePane ? withSubtitlePane.querySelector(".description") : null;
          document.querySelector('.index-row[data-session="rich1"]').click();
          const withoutSubtitlePane = document.querySelector('.detail-pane[data-session="rich1"]');
          const noSubtitleEl = withoutSubtitlePane ? withoutSubtitlePane.querySelector(".description") : null;
          return {
            hasSubtitle: !!subtitleEl,
            subtitleText: subtitleEl ? subtitleEl.textContent : null,
            hasNoSubtitle: !noSubtitleEl,
          };
        })()`,
      );
      check(
        "detail-subtitle",
        detailSubtitle.hasSubtitle &&
          Boolean(detailSubtitle.subtitleText && detailSubtitle.subtitleText.trim().length > 0) &&
          detailSubtitle.hasNoSubtitle,
        JSON.stringify(detailSubtitle),
      );

      // ---- thin-expand ---------------------------------------------------
      const thinExpand = await evalJS(
        cdp,
        `(() => {
          const group = document.querySelector(".thin-rollup details.thin-group");
          if (!group) return { skipped: true };
          const rowsBefore = group.querySelectorAll(".thin-row").length;
          const visibleBefore = Array.from(group.querySelectorAll(".thin-row")).every(r => r.offsetParent !== null) && !group.open;
          group.querySelector("summary").click();
          const opened = group.open;
          const paneBefore = document.querySelector(".detail-pane:not([hidden])").dataset.session;
          const thinRow = group.querySelector(".thin-row");
          if (thinRow) thinRow.click();
          const paneAfter = document.querySelector(".detail-pane:not([hidden])").dataset.session;
          return { rowsBefore, opened, unchanged: paneBefore === paneAfter };
        })()`,
      );
      check(
        "thin-expand",
        thinExpand.skipped || (thinExpand.rowsBefore > 0 && thinExpand.opened && thinExpand.unchanged),
        JSON.stringify(thinExpand),
      );

      // ---- no-horizontal-scroll ------------------------------------------
      // Canvas finding 5: page max-width moved 880px -> 1160px, so the
      // 1160-wide viewport (where the mailbox first reaches its full grid
      // width) is checked alongside the three pre-existing viewports.
      const viewports = [
        [1440, 900],
        [1160, 900],
        [1024, 768],
        [390, 844],
      ];
      const scrollResults = [];
      for (const [width, height] of viewports) {
        await cdp.send("Emulation.setDeviceMetricsOverride", {
          width,
          height,
          deviceScaleFactor: 1,
          mobile: width < 500,
        });
        await new Promise((resolve) => setTimeout(resolve, 80));
        const overflow = await evalJS(
          cdp,
          `document.documentElement.scrollWidth <= window.innerWidth + 1`,
        );
        scrollResults.push({ width, height, overflow });
      }
      await cdp.send("Emulation.clearDeviceMetricsOverride");
      check(
        "no-horizontal-scroll",
        scrollResults.every((r) => r.overflow),
        JSON.stringify(scrollResults),
      );

      // ---- glass-tokens (journal v2 Phase 4, brand polish) -----------------
      // The index pane must actually carry the glassmorphic blur — not just
      // a translucent background color.
      const glassTokens = await evalJS(
        cdp,
        `(() => {
          const pane = document.querySelector(".index-pane");
          if (!pane) return { skipped: true };
          const style = getComputedStyle(pane);
          const backdropFilter = style.backdropFilter || style.webkitBackdropFilter || "";
          return { backdropFilter };
        })()`,
      );
      check(
        "glass-tokens",
        glassTokens.skipped || /blur/.test(glassTokens.backdropFilter),
        JSON.stringify(glassTokens),
      );

      // ---- font-embedded (journal v2 Phase 4, brand polish) ----------------
      // Playfair Display must load from the embedded data: URI (no network
      // request — already asserted by no-external-requests above) and
      // actually be usable once loaded.
      await evalJS(cdp, `document.fonts.ready.then(() => true)`);
      const fontEmbedded = await evalJS(
        cdp,
        `document.fonts.check('16px "Playfair Display"')`,
      );
      check("font-embedded", fontEmbedded === true, JSON.stringify(fontEmbedded));

      // ---- postit-steers (journal v2 Phase 4, brand polish) ----------------
      // "rich7" carries stored steer quotes (see steer-lines above) — each
      // rendered steer-line element must also carry the post-it class.
      const postitSteers = await evalJS(
        cdp,
        `(() => {
          document.querySelector('.index-row[data-session="rich7"]').click();
          const pane = document.querySelector('.detail-pane[data-session="rich7"]');
          const lines = pane ? Array.from(pane.querySelectorAll(".steer-line")) : [];
          return {
            lineCount: lines.length,
            allPostIt: lines.length > 0 && lines.every((el) => el.classList.contains("post-it")),
          };
        })()`,
      );
      check(
        "postit-steers",
        postitSteers.lineCount > 0 && postitSteers.allPostIt,
        JSON.stringify(postitSteers),
      );

      // ---- since-then-panel (journal v2 Phase 5) ----------------------------
      // "rich9" carries 3 attributed symbols: retry_backoff (superseded, with
      // a receipt), compute_delay (witnessed, no verdict -> honest
      // "witnessed <date> · unverified since" (F1: never a fabricated
      // HEAD-verified "still live"), and named by an open todo -> gets a
      // so-clause), parse_config (attributed but never witnessed ->
      // unverified, no so-clause).
      const sinceThenPanel = await evalJS(
        cdp,
        `(() => {
          document.querySelector('.index-row[data-session="rich9"]').click();
          const pane = document.querySelector('.detail-pane[data-session="rich9"]');
          const panel = pane ? pane.querySelector(".since-then") : null;
          if (!panel) return { skipped: true };
          const rows = Array.from(panel.querySelectorAll(".since-then-row"));
          const bySymbol = {};
          for (const row of rows) {
            const symbol = row.querySelector(".symbol")?.textContent || "";
            const conclusion = row.querySelector(".conclusion");
            bySymbol[symbol] = {
              conclusionSlug: conclusion ? conclusion.className.replace("conclusion ", "") : null,
              conclusionText: conclusion ? conclusion.textContent : null,
              hasSoClause: !!row.querySelector(".so-clause"),
              whyHint: row.querySelector(".why-hint")?.textContent || null,
            };
          }
          const kpiText = panel.querySelector(".since-then-kpi")?.textContent || "";
          return { rowCount: rows.length, bySymbol, kpiText };
        })()`,
      );
      const sinceThenOk =
        !sinceThenPanel.skipped &&
        sinceThenPanel.rowCount === 3 &&
        sinceThenPanel.bySymbol.retry_backoff?.conclusionSlug === "superseded" &&
        /superseded ⌗deadbeef/.test(sinceThenPanel.bySymbol.retry_backoff?.conclusionText || "") &&
        sinceThenPanel.bySymbol.retry_backoff?.hasSoClause &&
        sinceThenPanel.bySymbol.compute_delay?.conclusionSlug === "witnessed_unverified" &&
        /^witnessed \d{4}-\d{2}-\d{2} · unverified since$/.test(
          sinceThenPanel.bySymbol.compute_delay?.conclusionText || "",
        ) &&
        sinceThenPanel.bySymbol.compute_delay?.hasSoClause &&
        sinceThenPanel.bySymbol.parse_config?.conclusionSlug === "unverified" &&
        !sinceThenPanel.bySymbol.parse_config?.hasSoClause &&
        sinceThenPanel.bySymbol.retry_backoff?.whyHint === 'csr_why("retry_backoff")' &&
        /3 symbols? touched/.test(sinceThenPanel.kpiText) &&
        /1 superseded since/.test(sinceThenPanel.kpiText) &&
        !/still live/.test(sinceThenPanel.kpiText) &&
        /1 witnessed, unverified/.test(sinceThenPanel.kpiText) &&
        /1 unverified/.test(sinceThenPanel.kpiText);
      check("since-then-panel", sinceThenOk, JSON.stringify(sinceThenPanel));

      // ---- since-then-absent (journal v2 Phase 5) ----------------------------
      // "rich2" has no episode_anchors and no code_node_attribution rows at
      // all — the panel must be entirely absent, never an empty shell.
      const sinceThenAbsent = await evalJS(
        cdp,
        `(() => {
          document.querySelector('.index-row[data-session="rich2"]').click();
          const pane = document.querySelector('.detail-pane[data-session="rich2"]');
          return { hasPanel: !!(pane && pane.querySelector(".since-then")) };
        })()`,
      );
      check("since-then-absent", sinceThenAbsent.hasPanel === false, JSON.stringify(sinceThenAbsent));

      // ---- dark-mode-emulated (journal v2 Phase 6) -------------------------
      // Headless Chrome's own default color-scheme preference is not
      // reliably "light" (it follows the host OS), so both ends of this
      // comparison are explicitly emulated rather than assuming the
      // pre-emulation render is the light one. Confirm the background
      // actually moves between the two and the dark render lands on the
      // dark sky-bottom token (#26263e = rgb(38, 38, 62)).
      await cdp.send("Emulation.setEmulatedMedia", {
        features: [{ name: "prefers-color-scheme", value: "light" }],
      });
      await new Promise((resolve) => setTimeout(resolve, 80));
      const lightBg = await evalJS(cdp, `getComputedStyle(document.body).backgroundImage`);
      await cdp.send("Emulation.setEmulatedMedia", {
        features: [{ name: "prefers-color-scheme", value: "dark" }],
      });
      await new Promise((resolve) => setTimeout(resolve, 80));
      const darkBg = await evalJS(cdp, `getComputedStyle(document.body).backgroundImage`);
      check(
        "dark-mode-emulated",
        darkBg !== lightBg && /38, ?38, ?62/.test(darkBg),
        JSON.stringify({ lightBg, darkBg }),
      );

      // ---- theme-toggle (journal v2 Phase 6) --------------------------------
      // Force the OS-level scheme back to light so only the in-page toggle
      // drives the attribute; walk one full auto -> dark -> light -> auto
      // cycle and check the `data-theme` attribute and the computed
      // background move together at every step.
      await cdp.send("Emulation.setEmulatedMedia", {
        features: [{ name: "prefers-color-scheme", value: "light" }],
      });
      await new Promise((resolve) => setTimeout(resolve, 80));
      const cycle = await evalJS(
        cdp,
        `(async () => {
          const toggle = document.querySelector(".theme-toggle");
          const bg = () => getComputedStyle(document.body).backgroundImage;
          const initialBg = bg();
          const steps = [];
          for (let i = 0; i < 3; i++) {
            toggle.click();
            await new Promise(resolve => setTimeout(resolve, 20));
            steps.push({ theme: document.documentElement.getAttribute("data-theme"), bg: bg() });
          }
          return { initialBg, steps };
        })()`,
      );
      const [afterDark, afterLight, afterAuto] = cycle.steps;
      check(
        "theme-toggle",
        afterDark?.theme === "dark" &&
          /38, ?38, ?62/.test(afterDark.bg) &&
          afterLight?.theme === "light" &&
          afterLight.bg === cycle.initialBg &&
          afterAuto?.theme === null &&
          afterAuto.bg === cycle.initialBg,
        JSON.stringify(cycle),
      );
      await cdp.send("Emulation.setEmulatedMedia", { features: [] });

      // ---- touch-next (Codex adversarial-review finding 1) -----------------
      // "rich9" has one open todo naming `compute_delay` and a superseded
      // symbol (`retry_backoff`, receipt `deadbeefcafe0123`) with room left
      // in the 3-line cap — both must appear in the strip. "rich2" has no
      // todos, no blockers/next_steps, and no attributed symbols at all
      // (see since-then-absent above) — it must render the honest
      // abstention sentence and no `<ul>` at all.
      const touchNext = await evalJS(
        cdp,
        `(() => {
          document.querySelector('.index-row[data-session="rich9"]').click();
          const withEvidencePane = document.querySelector('.detail-pane[data-session="rich9"]');
          const strip = withEvidencePane ? withEvidencePane.querySelector(".touch-next-list") : null;
          document.querySelector('.index-row[data-session="rich2"]').click();
          const abstainPane = document.querySelector('.detail-pane[data-session="rich2"]');
          const abstainEl = abstainPane ? abstainPane.querySelector(".touch-next-empty") : null;
          const abstainStrip = abstainPane ? abstainPane.querySelector(".touch-next-list") : null;
          return {
            stripText: strip ? strip.textContent : null,
            abstainText: abstainEl ? abstainEl.textContent : null,
            abstainHasStrip: !!abstainStrip,
          };
        })()`,
      );
      check(
        "touch-next",
        Boolean(touchNext.stripText) &&
          /compute_delay/.test(touchNext.stripText) &&
          /deadbeef/.test(touchNext.stripText) &&
          Boolean(touchNext.abstainText) &&
          /nothing evidenced to touch/.test(touchNext.abstainText) &&
          !touchNext.abstainHasStrip,
        JSON.stringify(touchNext),
      );

      // ---- ended-now-badges (Codex adversarial-review finding 3) -----------
      // "rich9" has one superseded symbol (`retry_backoff`) and nothing
      // adverse besides it -> "NOW: 1 superseded since"; the ENDED badge
      // always carries the "ENDED:" prefix and a YYYY-MM-DD title. "rich2"
      // has no attributed symbols at all -> ENDED renders, NOW does not.
      const endedNow = await evalJS(
        cdp,
        `(() => {
          document.querySelector('.index-row[data-session="rich9"]').click();
          const withSymbolsPane = document.querySelector('.detail-pane[data-session="rich9"]');
          const endedBadge = withSymbolsPane ? withSymbolsPane.querySelector(".outcome-badge") : null;
          const nowBadge = withSymbolsPane ? withSymbolsPane.querySelector(".now-badge") : null;
          document.querySelector('.index-row[data-session="rich2"]').click();
          const noSymbolsPane = document.querySelector('.detail-pane[data-session="rich2"]');
          const noSymbolsEnded = noSymbolsPane ? noSymbolsPane.querySelector(".outcome-badge") : null;
          const noSymbolsNow = noSymbolsPane ? noSymbolsPane.querySelector(".now-badge") : null;
          return {
            endedText: endedBadge ? endedBadge.textContent : null,
            endedTitle: endedBadge ? endedBadge.getAttribute("title") : null,
            nowText: nowBadge ? nowBadge.textContent : null,
            noSymbolsEndedText: noSymbolsEnded ? noSymbolsEnded.textContent : null,
            hasNoSymbolsNow: !!noSymbolsNow,
          };
        })()`,
      );
      check(
        "ended-now-badges",
        Boolean(endedNow.endedText) &&
          endedNow.endedText.startsWith("ENDED:") &&
          /^\d{4}-\d{2}-\d{2}$/.test(endedNow.endedTitle || "") &&
          Boolean(endedNow.nowText) &&
          /NOW: 1 superseded since/.test(endedNow.nowText) &&
          Boolean(endedNow.noSymbolsEndedText) &&
          endedNow.noSymbolsEndedText.startsWith("ENDED:") &&
          !endedNow.hasNoSymbolsNow &&
          !/NOW: (ok|fine|current)/.test(endedNow.nowText || ""),
        JSON.stringify(endedNow),
      );

      // ---- collapsed-defaults (Codex adversarial-review finding 4) ---------
      // "rich1" carries all four stages (request, 2 investigated files, a
      // todo, and an outcome) — ASK/DELIBERATION must render closed,
      // STEER/OUTCOME open.
      const collapsedDefaults = await evalJS(
        cdp,
        `(() => {
          document.querySelector('.index-row[data-session="rich1"]').click();
          const pane = document.querySelector('.detail-pane[data-session="rich1"]');
          const ask = pane ? pane.querySelector('[data-stage="S_INPUT"]') : null;
          const delib = pane ? pane.querySelector('[data-stage="S_DELIB"]') : null;
          const steer = pane ? pane.querySelector('[data-stage="S_STEER"]') : null;
          const outcome = pane ? pane.querySelector('[data-stage="S_OUT"]') : null;
          return {
            askOpen: ask ? ask.open : null,
            delibOpen: delib ? delib.open : null,
            steerOpen: steer ? steer.open : null,
            outcomeOpen: outcome ? outcome.open : null,
          };
        })()`,
      );
      check(
        "collapsed-defaults",
        collapsedDefaults.askOpen === false &&
          collapsedDefaults.delibOpen === false &&
          collapsedDefaults.steerOpen === true &&
          collapsedDefaults.outcomeOpen === true,
        JSON.stringify(collapsedDefaults),
      );

      // ---- pane-scroll (Codex adversarial-review finding 2) ----------------
      // Scroll the detail pane down first (simulating a reader mid-scroll),
      // then activate a different, lower row and confirm the pane's scroll
      // resets to the top and the new heading lands in view. The page's own
      // `prefers-reduced-motion` read happens once at load time (before any
      // CDP media emulation could take effect), so this waits out the
      // smooth-scroll animation itself instead — a generous margin over
      // Chromium's default scroll-behavior duration (~300-500ms).
      const paneScrollSetup = await evalJS(
        cdp,
        `(() => {
          const wrap = document.querySelector(".detail-pane-wrap");
          const rows = Array.from(document.querySelectorAll(".index-row"));
          if (!wrap || rows.length < 2) return { skipped: true };
          wrap.scrollTop = wrap.scrollHeight;
          const target = rows[rows.length - 1];
          target.click();
          return { skipped: false };
        })()`,
      );
      let paneScroll = paneScrollSetup;
      if (!paneScrollSetup.skipped) {
        await new Promise((resolve) => setTimeout(resolve, 700));
        paneScroll = await evalJS(
          cdp,
          `(() => {
            const wrap = document.querySelector(".detail-pane-wrap");
            const visiblePane = document.querySelector(".detail-pane:not([hidden])");
            const heading = visiblePane ? visiblePane.querySelector(".sentence") : null;
            const wrapRect = wrap.getBoundingClientRect();
            const headingRect = heading ? heading.getBoundingClientRect() : null;
            const headingInView =
              !!headingRect &&
              headingRect.top >= wrapRect.top - 1 &&
              headingRect.top <= wrapRect.bottom;
            return { after: wrap.scrollTop, headingInView };
          })()`,
        );
      }
      check(
        "pane-scroll",
        paneScroll.skipped || (paneScroll.after === 0 && paneScroll.headingInView),
        JSON.stringify(paneScroll),
      );

      // ==== Journal v3 "Dreams Gateway" Phase 2 =========================
      // Reuses this same page/tab (no second Chrome instance) against a
      // dedicated dream fixture — see `buildDreamFixtureSql`'s doc comment
      // for why it's separate from the mailbox fixture above.
      const dreamFixture = seedDreamFixtureAndRender(workDir);
      const dreamFileUrl = "file://" + dreamFixture.reportPath;
      const dreamNavigated = cdp.once("Page.loadEventFired");
      await cdp.send("Page.navigate", { url: dreamFileUrl });
      await dreamNavigated;
      await new Promise((resolve) => setTimeout(resolve, 150));

      // ---- (a) dreams home is the default view; every card carries a
      // receipt ---------------------------------------------------------
      const dreamHome = await evalJS(
        cdp,
        `(() => {
          const home = document.getElementById("dreams-home-view");
          const detail = document.getElementById("dream-detail-view");
          const log = document.getElementById("session-log-view");
          const cards = Array.from(document.querySelectorAll(".dream-card"));
          return {
            homeVisible: !!home && !home.hidden,
            detailHidden: !!detail && !!detail.hidden,
            logHidden: !!log && !!log.hidden,
            cardCount: cards.length,
            allHaveReceipt: cards.length > 0 && cards.every(c => c.textContent.includes("⌗")),
          };
        })()`,
      );
      check(
        "dreams-home-default-with-receipts",
        dreamHome.homeVisible &&
          dreamHome.detailHidden &&
          dreamHome.logHidden &&
          dreamHome.cardCount === 2 &&
          dreamHome.allHaveReceipt,
        JSON.stringify(dreamHome),
      );

      // ---- (b) project grouping headers present -------------------------
      const dreamGroups = await evalJS(
        cdp,
        `Array.from(document.querySelectorAll(".dream-project-eyebrow")).map(e => e.textContent.trim())`,
      );
      check(
        "dreams-project-grouping",
        dreamGroups.length === 2 &&
          dreamGroups.some((t) => t.includes("dream-alpha")) &&
          dreamGroups.some((t) => t.includes("dream-beta")),
        JSON.stringify(dreamGroups),
      );

      // ---- (c) grade chips present; legacy mailbox unreachable as
      // default ------------------------------------------------------------
      const dreamGrades = await evalJS(
        cdp,
        `Array.from(document.querySelectorAll(".dream-grade")).map(c => c.className)`,
      );
      check(
        "dreams-grade-chips",
        dreamGrades.some((c) => c.includes("item-grade")) &&
          dreamGrades.some((c) => c.includes("session-grade")),
        JSON.stringify(dreamGrades),
      );
      check("dreams-mailbox-not-default", dreamHome.logHidden === true, JSON.stringify(dreamHome));

      // ---- (d) clicking a card shows the detail shell; back returns
      // home --------------------------------------------------------------
      // Clicking an `<a href="#...">` updates `location.hash` synchronously,
      // but the browser dispatches `hashchange` (which the routing script
      // listens on) as a separate task — a same-tick read right after
      // `.click()` would race that dispatch and see stale state, so the
      // click and the assertion are two `evalJS` calls with a wait between.
      const dreamCardId = await evalJS(
        cdp,
        `document.querySelector(".dream-card").dataset.item`,
      );
      await evalJS(cdp, `document.querySelector(".dream-card").click()`);
      await new Promise((resolve) => setTimeout(resolve, 150));
      const dreamClick = await evalJS(
        cdp,
        `(() => {
          const id = "${dreamCardId}";
          const detail = document.getElementById("dream-detail-view");
          const home = document.getElementById("dreams-home-view");
          const article = document.querySelector('.dream-detail-item[data-item="' + id + '"]');
          const back = article ? article.querySelector(".dream-back") : null;
          return {
            id,
            detailVisible: !!detail && !detail.hidden,
            homeHiddenAfterClick: !!home && !!home.hidden,
            articleVisible: !!article && !article.hidden,
            hasBackArrow: !!back,
            backText: back ? back.textContent : null,
          };
        })()`,
      );
      check(
        "dreams-card-click-detail",
        dreamClick.detailVisible &&
          dreamClick.homeHiddenAfterClick &&
          dreamClick.articleVisible &&
          dreamClick.hasBackArrow &&
          !!dreamClick.backText &&
          dreamClick.backText.includes("dreams"),
        JSON.stringify(dreamClick),
      );

      await evalJS(
        cdp,
        `document.querySelector('.dream-detail-item[data-item="${dreamClick.id}"] .dream-back').click()`,
      );
      await new Promise((resolve) => setTimeout(resolve, 150));
      const dreamBack = await evalJS(
        cdp,
        `(() => {
          const home = document.getElementById("dreams-home-view");
          const detail = document.getElementById("dream-detail-view");
          return { homeVisible: !!home && !home.hidden, detailHidden: !!detail && !!detail.hidden };
        })()`,
      );
      check(
        "dreams-back-returns-home",
        dreamBack.homeVisible && dreamBack.detailHidden,
        JSON.stringify(dreamBack),
      );

      // ---- (e) hash deep-link lands on detail ----------------------------
      // Navigating from the SAME document to a URL differing only by
      // fragment is a same-document navigation — Chrome never re-fires
      // `Page.loadEventFired` for it, since no new document loads. Routing
      // through `about:blank` first forces a genuine cross-document
      // navigation into the hash URL, matching what actually happens when a
      // bookmark/deep-link opens the report fresh.
      const blankNavigated = cdp.once("Page.loadEventFired");
      await cdp.send("Page.navigate", { url: "about:blank" });
      await blankNavigated;
      const deepLinkNavigated = cdp.once("Page.loadEventFired");
      await cdp.send("Page.navigate", { url: `${dreamFileUrl}#/item/${dreamClick.id}` });
      await deepLinkNavigated;
      await new Promise((resolve) => setTimeout(resolve, 200));
      const dreamDeepLink = await evalJS(
        cdp,
        `(() => {
          const detail = document.getElementById("dream-detail-view");
          const home = document.getElementById("dreams-home-view");
          const article = document.querySelector('.dream-detail-item[data-item="${dreamClick.id}"]');
          return {
            detailVisible: !!detail && !detail.hidden,
            homeHidden: !!home && !!home.hidden,
            articleVisible: !!article && !article.hidden,
          };
        })()`,
      );
      check(
        "dreams-hash-deep-link",
        dreamDeepLink.detailVisible && dreamDeepLink.homeHidden && dreamDeepLink.articleVisible,
        JSON.stringify(dreamDeepLink),
      );

      // ---- (f) empty-fixture DB renders the honest empty state, zero
      // cards — reuses the pre-seed bootstrap render `seedFixtureAndRender`
      // already produced (a genuinely empty, freshly-migrated DB). --------
      const bootstrapUrl = "file://" + path.join(workDir, "bootstrap.html");
      const bootstrapNavigated = cdp.once("Page.loadEventFired");
      await cdp.send("Page.navigate", { url: bootstrapUrl });
      await bootstrapNavigated;
      await new Promise((resolve) => setTimeout(resolve, 150));
      const dreamEmpty = await evalJS(
        cdp,
        `(() => {
          const empty = document.querySelector(".dreams-empty");
          const cards = document.querySelectorAll(".dream-card");
          return {
            hasEmptyState: !!empty,
            emptyText: empty ? empty.textContent.trim() : null,
            cardCount: cards.length,
          };
        })()`,
      );
      check(
        "dreams-empty-state",
        dreamEmpty.hasEmptyState &&
          dreamEmpty.cardCount === 0 &&
          !!dreamEmpty.emptyText &&
          dreamEmpty.emptyText.includes("No dreams matched open work"),
        JSON.stringify(dreamEmpty),
      );

      // ---- (g) no-JS DOM completeness: raw HTML source (never executed)
      // must carry every item's markup regardless of the `hidden`
      // attribute JS toggles at runtime. ------------------------------------
      const dreamSource = readFileSync(dreamFixture.reportPath, "utf8");
      const dreamNoJs = {
        hasHome: dreamSource.includes('id="dreams-home-view"'),
        hasDetail: dreamSource.includes('id="dream-detail-view"'),
        hasLog: dreamSource.includes('id="session-log-view"'),
        articleCount: (dreamSource.match(/class="dream-detail-item"/g) || []).length,
        evidenceRowCount: (dreamSource.match(/class="dream-evidence-row"/g) || []).length,
      };
      check(
        "dreams-no-js-dom-completeness",
        dreamNoJs.hasHome &&
          dreamNoJs.hasDetail &&
          dreamNoJs.hasLog &&
          dreamNoJs.articleCount === 2 &&
          dreamNoJs.evidenceRowCount >= 2,
        JSON.stringify(dreamNoJs),
      );

      cdp.close();
    } finally {
      chrome.kill("SIGKILL");
      rmSync(userDataDir, { recursive: true, force: true });
    }
  } finally {
    rmSync(workDir, { recursive: true, force: true });
  }

  console.log(`\n${failures === 0 ? "ALL PASS" : `${failures} FAILURE(S)`}`);
  process.exit(failures === 0 ? 0 : 1);
}

main().catch((err) => {
  console.error("FAIL harness", err.stack || err);
  process.exit(1);
});
