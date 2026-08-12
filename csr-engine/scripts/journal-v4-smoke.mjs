#!/usr/bin/env node
// Dreams v4 live-server smoke. Zero npm dependencies; fixture databases and
// browser profiles live only under one temporary directory.

import { createHash } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync } from "node:fs";
import net from "node:net";
import { networkInterfaces, tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const ENGINE_DIR = path.dirname(SCRIPT_DIR);
const BINARY =
  process.env.CSR_SMOKE_BINARY || path.join(ENGINE_DIR, "target", "release", "csr-engine");
const CHROME =
  process.env.CSR_SMOKE_CHROME ||
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

let failures = 0;
function check(name, ok, detail = "") {
  console.log(`${ok ? "PASS" : "FAIL"} ${name}${detail ? ` ${detail}` : ""}`);
  if (!ok) failures += 1;
}

function sql(value) {
  return `'${String(value).replaceAll("'", "''")}'`;
}

function itemId(project, item) {
  return createHash("sha256")
    .update(project)
    .update("\0")
    .update(item.trim().toLowerCase())
    .digest("hex")
    .slice(0, 16);
}

function runBinary(args, env = {}) {
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

function sqlite(dbPath, statement) {
  const result = spawnSync("sqlite3", [dbPath], { input: statement, encoding: "utf8" });
  if (result.status !== 0) {
    throw new Error(`sqlite3 failed: ${result.stderr}\nSQL:\n${statement}`);
  }
  return result.stdout.trim();
}

function bootstrap(dbPath, projectsDir, workDir, name) {
  mkdirSync(projectsDir, { recursive: true });
  runBinary([
    "--db-path",
    dbPath,
    "--projects-dir",
    projectsDir,
    "dream",
    "--report",
    "--no-open",
    "--out",
    path.join(workDir, `${name}-bootstrap.html`),
  ]);
}

function episode(id, project, timestamp, todos, files = []) {
  const content = JSON.stringify({
    schema: "v2",
    session_id: id,
    project,
    timestamp,
    todos,
    files_modified: files,
  });
  return `INSERT INTO reflections (id, content, tags, timestamp)
          VALUES (${sql(`episode-${id}`)}, ${sql(content)}, '["session_episode","schema_v2"]', ${sql(timestamp)});`;
}

function witness(project, file, symbol, stamp, verdict, receipt, timestamp) {
  return `INSERT INTO witness_ledger
            (project, file, symbol, span_start, span_end, stamp, tier, at_oid, source_kind, source_id)
          VALUES (${sql(project)}, ${sql(file)}, ${sql(symbol)}, 1, 3, ${sql(stamp)},
                  'committed', ${sql(`at-${stamp}`)}, 'backfill', ${sql(`at-${stamp}`)});
          INSERT INTO witness_verdicts
            (witness_id, verdict, successor_witness_id, receipt_oid, observed_head_oid, created_at)
          VALUES ((SELECT id FROM witness_ledger WHERE stamp = ${sql(stamp)}), ${sql(verdict)},
                  NULL, ${receipt == null ? "NULL" : sql(receipt)}, 'fixture-head', ${sql(timestamp)});`;
}

function seedBoardFixture(dbPath) {
  const proposal = {
    project: "proposal-project",
    session: "session-proposal",
    item: "execute `proposal_symbol` release plan",
  };
  const observation = {
    project: "observation-project",
    session: "session-observation",
    item: "investigate whether the observed flow still matters",
  };
  const outdated = {
    project: "outdated-project",
    session: "session-outdated",
    item: "retire `outdated_symbol` claim",
  };
  const unexamined = {
    project: "unexamined-project",
    session: "session-unexamined",
    item: "decide the copy tone",
  };
  const settled = {
    project: "settled-project",
    session: "session-settled-open",
    item: "finish `settled_symbol` cleanup",
  };
  const archived = {
    project: "archive-project",
    session: "session-archive",
    item: "update `archive_symbol` note",
  };
  const proposalId = itemId(proposal.project, proposal.item);
  const statements = [
    episode(proposal.session, proposal.project, "2026-08-01T08:00:00Z", [
      { content: proposal.item, status: "pending" },
    ]),
    episode(observation.session, observation.project, "2026-08-01T09:00:00Z", [
      { content: observation.item, status: "pending" },
    ], ["/fixture/observation.rs"]),
    episode(outdated.session, outdated.project, "2026-08-01T10:00:00Z", [
      { content: outdated.item, status: "pending" },
    ]),
    episode(unexamined.session, unexamined.project, "2026-08-01T11:00:00Z", [
      { content: unexamined.item, status: "pending" },
    ]),
    episode(settled.session, settled.project, "2026-07-20T08:00:00Z", [
      { content: settled.item, status: "pending" },
    ]),
    episode("session-settled-done", settled.project, "2026-08-02T08:00:00Z", [
      { content: settled.item, status: "completed" },
    ]),
    episode(archived.session, archived.project, "2026-06-01T08:00:00Z", [
      { content: archived.item, status: "pending" },
    ]),
    witness(proposal.project, "/fixture/proposal.rs", "proposal_symbol", "stamp-proposal", "superseded_by", "11111111aaa", "2026-08-03T03:00:00Z"),
    witness(observation.project, "/fixture/observation.rs", "observed_symbol", "stamp-observation", "anchor_reinstated", "22222222bbb", "2026-08-03T03:00:00Z"),
    witness(outdated.project, "/fixture/outdated.rs", "outdated_symbol", "stamp-outdated", "anchor_obsolete", "33333333ccc", "2026-08-03T03:00:00Z"),
    witness(settled.project, "/fixture/settled.rs", "settled_symbol", "stamp-settled", "superseded_by", "44444444ddd", "2026-07-21T03:00:00Z"),
    witness(archived.project, "/fixture/archive.rs", "archive_symbol", "stamp-archive", "anchor_obsolete", "55555555eee", "2026-06-02T03:00:00Z"),
    `INSERT INTO dream_plans
       (plan_hash, item_id, project, session_id, context, steps_json, files_json,
        acceptance, dropped, model)
     VALUES ('plan-fixture', ${sql(proposalId)}, ${sql(proposal.project)}, ${sql(proposal.session)},
       'Stored fixture context',
       '[{"action":"Update proposal_symbol","citation":"11111111","files":["/fixture/proposal.rs"]}]',
       '["/fixture/proposal.rs"]', 'Run the release gate', 0, 'fixture-model');`,
    `INSERT INTO witness_generations
       (generation_id, project, file, head_oid, extractor_version, status, created_at)
     VALUES ('archive-pass', ${sql(archived.project)}, '/fixture/archive.rs', 'archive-head',
             'fixture-v1', 'complete', '2026-06-03T03:00:00Z');`,
  ];
  for (const item of [proposal, observation, outdated, unexamined, settled, archived]) {
    statements.push(`INSERT INTO chunks
      (id, conversation_id, project_name, timestamp, content, message_count)
      VALUES (${sql(`chunk-${item.session}`)}, ${sql(item.session)}, ${sql(item.project)},
              '2026-08-01T00:00:00Z', 'fixture chunk', 1);`);
  }
  sqlite(dbPath, `BEGIN IMMEDIATE;\n${statements.join("\n")}\nCOMMIT;`);
  return { proposal, observation, outdated, unexamined, settled, archived };
}

function collectOutput(child) {
  child.output = "";
  for (const stream of [child.stdout, child.stderr]) {
    stream.setEncoding("utf8");
    stream.on("data", (chunk) => {
      child.output += chunk;
    });
  }
  return child;
}

async function waitFor(child, pattern, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const match = child.output.match(pattern);
    if (match) return match;
    if (child.exitCode != null) {
      throw new Error(`process exited ${child.exitCode} before ${pattern}:\n${child.output}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`timed out waiting for ${pattern}:\n${child.output}`);
}

async function startJournal(dbPath, projectsDir, port = 0) {
  const child = collectOutput(spawn(BINARY, [
    "--db-path", dbPath,
    "--projects-dir", projectsDir,
    "journal", "serve", "--port", String(port),
  ], {
    env: { ...process.env, CSR_NO_AI_NARRATIVES: "1", RUST_LOG: "info" },
    stdio: ["ignore", "pipe", "pipe"],
  }));
  const match = await waitFor(child, /CSR dream journal serving at http:\/\/127\.0\.0\.1:(\d+)\//);
  const boundPort = Number(match[1]);
  return { child, port: boundPort, base: `http://127.0.0.1:${boundPort}` };
}

async function stop(child, signal = "SIGINT", timeoutMs = 8_000) {
  if (child.exitCode != null) return child.exitCode;
  child.kill(signal);
  const exit = new Promise((resolve) => child.once("exit", (code) => resolve(code)));
  const timeout = new Promise((resolve) => setTimeout(() => resolve("timeout"), timeoutMs));
  const result = await Promise.race([exit, timeout]);
  if (result === "timeout") {
    child.kill("SIGKILL");
    await new Promise((resolve) => child.once("exit", resolve));
  }
  return result;
}

async function fetchText(url, options = {}) {
  const response = await fetch(url, { redirect: "manual", ...options });
  return { response, body: await response.text() };
}

function csrfFrom(body) {
  const match = body.match(/name="csrf" value="([0-9a-f]+)"/);
  if (!match) throw new Error("detail page carried no CSRF token");
  return match[1];
}

function lanAddress() {
  for (const entries of Object.values(networkInterfaces())) {
    for (const entry of entries || []) {
      if (entry.family === "IPv4" && !entry.internal) return entry.address;
    }
  }
  return null;
}

async function connectionFails(host, port) {
  return new Promise((resolve) => {
    const socket = net.createConnection({ host, port });
    const done = (failed) => {
      socket.destroy();
      resolve(failed);
    };
    socket.setTimeout(1500, () => done(true));
    socket.once("error", () => done(true));
    socket.once("connect", () => done(false));
  });
}

async function freePort() {
  const server = net.createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  return port;
}

class CDP {
  constructor(url) {
    this.ws = new WebSocket(url);
    this.id = 0;
    this.pending = new Map();
    this.listeners = new Map();
    this.ready = new Promise((resolve, reject) => {
      this.ws.addEventListener("open", resolve, { once: true });
      this.ws.addEventListener("error", reject, { once: true });
    });
    this.ws.addEventListener("message", (event) => {
      const msg = JSON.parse(event.data);
      if (msg.id) {
        const pending = this.pending.get(msg.id);
        if (!pending) return;
        this.pending.delete(msg.id);
        msg.error ? pending.reject(new Error(msg.error.message)) : pending.resolve(msg.result);
        return;
      }
      for (const listener of this.listeners.get(msg.method) || []) listener(msg.params);
    });
  }
  async send(method, params = {}) {
    await this.ready;
    const id = ++this.id;
    const result = new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
    this.ws.send(JSON.stringify({ id, method, params }));
    return result;
  }
  on(method, listener) {
    const listeners = this.listeners.get(method) || [];
    listeners.push(listener);
    this.listeners.set(method, listeners);
  }
  once(method, timeoutMs = 10_000) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`CDP timeout: ${method}`)), timeoutMs);
      const listener = (params) => {
        clearTimeout(timer);
        const listeners = this.listeners.get(method) || [];
        this.listeners.set(method, listeners.filter((candidate) => candidate !== listener));
        resolve(params);
      };
      this.on(method, listener);
    });
  }
  close() {
    this.ws.close();
  }
}

async function waitJson(url, timeoutMs = 15_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url);
      if (response.ok) return response.json();
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`timed out waiting for ${url}`);
}

async function browserProof(workDir, base, detailPath) {
  if (typeof WebSocket === "undefined") throw new Error("Node >= 22 global WebSocket required");
  if (!existsSync(CHROME)) throw new Error(`Chrome not found at ${CHROME}`);
  const debugPort = await freePort();
  const profile = path.join(workDir, "chrome-profile");
  const chrome = collectOutput(spawn(CHROME, [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    `--remote-debugging-port=${debugPort}`, `--user-data-dir=${profile}`, "about:blank",
  ], { stdio: ["ignore", "pipe", "pipe"] }));
  try {
    await waitJson(`http://127.0.0.1:${debugPort}/json/version`);
    const targetResponse = await fetch(
      `http://127.0.0.1:${debugPort}/json/new?${encodeURIComponent("about:blank")}`,
      { method: "PUT" },
    );
    const target = await targetResponse.json();
    const cdp = new CDP(target.webSocketDebuggerUrl);
    const requests = [];
    cdp.on("Network.requestWillBeSent", ({ request }) => requests.push(request.url));
    await cdp.send("Page.enable");
    await cdp.send("Network.enable");
    await cdp.send("Emulation.setScriptExecutionDisabled", { value: true });

    for (const [name, url, required] of [
      ["board", `${base}/`, ["#proposals", "#observations", "#outdated-claims", "#unexamined", "#settled", "#archive"]],
      ["detail", `${base}${detailPath}`, ["main", "form", "#copy"]],
    ]) {
      const loaded = cdp.once("Page.loadEventFired");
      await cdp.send("Page.navigate", { url });
      await loaded;
      const expression = `(() => ({
        ready: document.readyState,
        missing: ${JSON.stringify(required)}.filter(s => !document.querySelector(s)),
        scripts: document.scripts.length,
        text: document.body.innerText
      }))()`;
      const evaluated = await cdp.send("Runtime.evaluate", { expression, returnByValue: true });
      const value = evaluated.result.value;
      check(`${name} DOM is complete with JavaScript disabled`, value.ready === "complete" && value.missing.length === 0 && value.scripts === 0, JSON.stringify(value));
    }
    const external = requests.filter((url) => {
      try {
        const parsed = new URL(url);
        return parsed.origin !== base && parsed.protocol !== "data:";
      } catch {
        return true;
      }
    });
    check("served pages make no external network requests", external.length === 0, external.join(", "));
    cdp.close();
  } finally {
    await stop(chrome, "SIGTERM", 3_000);
  }
}

async function main() {
  if (!existsSync(BINARY)) throw new Error(`release binary missing at ${BINARY}`);
  const workDir = mkdtempSync(path.join(tmpdir(), "csr-journal-v4-smoke-"));
  const children = [];
  try {
    const dbPath = path.join(workDir, "board.db");
    const projectsDir = path.join(workDir, "projects");
    bootstrap(dbPath, projectsDir, workDir, "board");
    const fixture = seedBoardFixture(dbPath);

    const live = await startJournal(dbPath, projectsDir, 0);
    children.push(live.child);
    check("listener reports literal loopback binding", live.child.output.includes(`http://127.0.0.1:${live.port}/`) && live.child.output.includes("Loopback only (127.0.0.1)"), live.child.output.trim());
    const lan = lanAddress();
    check("non-loopback connection fails", Boolean(lan) && await connectionFails(lan, live.port), lan || "no non-loopback IPv4 interface found");

    const board = await fetchText(`${live.base}/`);
    check("cluster board returns HTML", board.response.status === 200 && board.body.startsWith("<!doctype html>"));
    for (const label of ["Proposals", "Observations", "Outdated claims", "Unexamined", "Settled", "Archive"]) {
      check(`board renders ${label}`, board.body.includes(label));
    }
    for (const slug of ["proposals", "observations", "outdated-claims"]) {
      const lane = board.body.match(new RegExp(`<section class="column" id="${slug}"[\\s\\S]*?<\\/section>`))?.[0] || "";
      check(`${slug} lane has a real cluster card`, lane.includes("class=\"card "), lane.slice(0, 160));
    }
    const cards = [...board.body.matchAll(/<li class="card [\s\S]*?<\/li>/g)].map((match) => match[0]);
    check("index cards are three-line cards", cards.length >= 3 && cards.every((card) => card.includes("card-caps") && card.includes("card-conclusion") && card.includes("card-meta")), `${cards.length} cards`);
    check("every active card has only the micro copy icon action", cards.length >= 3 && cards.every((card) => /class="card-copy"[^>]*>⧉<\/a>/.test(card) && !/>copy<\/a>/i.test(card)));

    const outdatedId = itemId(fixture.outdated.project, fixture.outdated.item);
    const detailPath = `/dream/${outdatedId}`;
    check("target card is present before resolve", board.body.includes(`href="${detailPath}"`));
    const detail = await fetchText(`${live.base}${detailPath}`);
    check("real detail route resolves", detail.response.status === 200 && detail.body.includes(fixture.outdated.item));
    const unknown = await fetchText(`${live.base}/dream/does-not-exist`);
    check("unknown dream id is 404", unknown.response.status === 404 && unknown.body.includes("No dream with that id"));

    const csrf = csrfFrom(detail.body);
    const formHeaders = {
      "content-type": "application/x-www-form-urlencoded",
      "sec-fetch-site": "same-origin",
      origin: live.base,
    };
    const crossOrigin = await fetchText(`${live.base}${detailPath}/resolve`, {
      method: "POST",
      headers: { ...formHeaders, "sec-fetch-site": "cross-site", origin: "https://example.invalid" },
      body: `csrf=${csrf}`,
    });
    check("cross-origin POST is rejected", crossOrigin.response.status === 403);
    const missingCsrf = await fetchText(`${live.base}${detailPath}/resolve`, {
      method: "POST", headers: formHeaders, body: "csrf=",
    });
    check("missing CSRF is rejected", missingCsrf.response.status === 403);
    const oversized = await fetchText(`${live.base}${detailPath}/resolve`, {
      method: "POST", headers: formHeaders, body: `csrf=${csrf}&pad=${"x".repeat(2048)}`,
    });
    check("oversized body is rejected", oversized.response.status === 413);

    await browserProof(workDir, live.base, detailPath);

    const resolved = await fetchText(`${live.base}${detailPath}/resolve`, {
      method: "POST", headers: formHeaders, body: `csrf=${csrf}`,
    });
    check("resolve mutation succeeds and is attributed", resolved.response.status === 200 && resolved.body.includes("Verdict recorded") && resolved.body.includes("journal_ui"));
    const afterResolve = await fetchText(`${live.base}/`);
    check("resolved card leaves the board", afterResolve.response.status === 200 && !afterResolve.body.includes(`href="${detailPath}"`));

    sqlite(dbPath, "DROP TABLE reflections;");
    const degraded = await fetchText(`${live.base}/`);
    check("read failures render a degraded notice", degraded.response.status === 500 && degraded.body.includes("The feed could not be read") && degraded.body.includes("Nothing is rendered from a partial read"));

    const releasedPort = live.port;
    const stopped = await stop(live.child);
    check("journal exits gracefully on SIGINT", stopped !== "timeout", live.child.output.trim());
    const rebound = net.createServer();
    await new Promise((resolve, reject) => {
      rebound.once("error", reject);
      rebound.listen(releasedPort, "127.0.0.1", resolve);
    });
    check("graceful shutdown releases the port", rebound.address().port === releasedPort);
    await new Promise((resolve) => rebound.close(resolve));

    const emptyDb = path.join(workDir, "empty.db");
    const emptyProjects = path.join(workDir, "empty-projects");
    bootstrap(emptyDb, emptyProjects, workDir, "empty");
    const empty = await startJournal(emptyDb, emptyProjects, 0);
    children.push(empty.child);
    const emptyPage = await fetchText(`${empty.base}/`);
    check("empty fixture renders the honest empty state", emptyPage.response.status === 200 && emptyPage.body.includes("Nothing on record yet.") && emptyPage.body.includes("empty feed, not a failure") && !emptyPage.body.toLowerCase().includes("all clear"));
    await stop(empty.child);

    const occupied = net.createServer();
    await new Promise((resolve, reject) => {
      occupied.once("error", reject);
      occupied.listen(0, "127.0.0.1", resolve);
    });
    const busyPort = occupied.address().port;
    const daemonDb = path.join(workDir, "daemon.db");
    const daemonProjects = path.join(workDir, "daemon-projects");
    bootstrap(daemonDb, daemonProjects, workDir, "daemon");
    const daemon = collectOutput(spawn(BINARY, [
      "--db-path", daemonDb, "--projects-dir", daemonProjects, "daemon", "--no-ai",
    ], {
      env: {
        ...process.env,
        CSR_JOURNAL_PORT: String(busyPort),
        CSR_NO_AI_NARRATIVES: "1",
        CSR_NO_DREAMING: "1",
        RUST_LOG: "csr_engine=info",
      },
      stdio: ["ignore", "pipe", "pipe"],
    }));
    children.push(daemon);
    const fallback = await waitFor(daemon, /journal server listening at http:\/\/127\.0\.0\.1:(\d+)\//, 30_000);
    const fallbackPort = Number(fallback[1]);
    const health = await fetchText(`http://127.0.0.1:${fallbackPort}/healthz`);
    check("busy journal port does not kill the daemon", daemon.exitCode == null && fallbackPort !== busyPort && health.response.status === 200, daemon.output.trim());
    await stop(daemon, "SIGTERM", 3_000);
    await new Promise((resolve) => occupied.close(resolve));

    check("all live-server fixture paths stayed outside the user database", dbPath.startsWith(workDir) && emptyDb.startsWith(workDir) && daemonDb.startsWith(workDir), workDir);
  } finally {
    for (const child of children) {
      if (child.exitCode == null) child.kill("SIGKILL");
    }
    rmSync(workDir, { recursive: true, force: true });
  }

  console.log(`${failures === 0 ? "PASS" : "FAIL"} journal v4 live-server smoke: ${failures === 0 ? "all checks passed" : `${failures} check(s) failed`}`);
  process.exitCode = failures === 0 ? 0 : 1;
}

main().catch((error) => {
  console.error(`FAIL journal v4 live-server smoke harness ${error.stack || error}`);
  process.exitCode = 1;
});
