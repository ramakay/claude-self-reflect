/**
 * Installer helpers, kept out of postinstall.js so the install decision and the
 * stale-copy detection can be tested without a network, a package manager, or
 * the user's real HOME.
 *
 * Everything here is read-only. Nothing in this file writes, deletes, or
 * elevates: a user may deliberately keep a different csr-engine build first on
 * PATH, so a shadowed install is reported, never corrected.
 */

import { existsSync, readFileSync, realpathSync } from 'fs';
import { execFileSync } from 'child_process';
import { join } from 'path';

export const BINARY_NAME = 'csr-engine';
export const MCP_SERVER_NAME = 'claude-self-reflect';

/** Resolve symlinks. Returns the input unchanged when it cannot be resolved. */
export function realPath(p) {
  try {
    return realpathSync(p);
  } catch {
    return p;
  }
}

/** Pull the version out of clap's `csr-engine 10.1.0` line. */
export function parseVersion(output) {
  const firstLine = String(output || '').trim().split('\n')[0].trim();
  const match = firstLine.match(/(\d+\.\d+\.\d+[0-9A-Za-z.+-]*)$/);
  return match ? match[1] : null;
}

/**
 * Ask a binary what version it is.
 *
 * `--version` is the only thing the installer ever runs against a binary it did
 * not just install: `status`, `setup`, `hook` and `eval` all open the user's
 * live database. Returns null when the binary cannot answer — every release up
 * to 9.5.7 predates the flag and exits 2 — which means "unknown", i.e. reinstall.
 */
export function probeVersion(binaryPath, exec = execFileSync) {
  try {
    return parseVersion(
      exec(binaryPath, ['--version'], {
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'ignore'],
        timeout: 10000,
      })
    );
  } catch {
    return null;
  }
}

/**
 * Decide what to do about INSTALL_DIR/csr-engine — and only about that path.
 * Which csr-engine comes first on PATH is a separate question
 * (detectStaleBinaries); answering it here is how postinstall ended up printing
 * "Updating /usr/local/bin/csr-engine..." immediately before writing elsewhere.
 */
export function planInstall({ destPath, pkgVersion, probe = probeVersion }) {
  if (!existsSync(destPath)) {
    return { action: 'install', reason: 'missing', installedVersion: null };
  }
  const installedVersion = probe(destPath);
  if (installedVersion === null) {
    return { action: 'install', reason: 'unknown', installedVersion: null };
  }
  if (installedVersion === pkgVersion) {
    return { action: 'skip', reason: 'match', installedVersion };
  }
  return { action: 'install', reason: 'different', installedVersion };
}

/** The first csr-engine on PATH, or null. */
export function whichBinary(exec = execFileSync) {
  try {
    const found = exec('which', [BINARY_NAME], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim();
    return found || null;
  } catch {
    return null;
  }
}

/** Read JSON, fail open: unreadable or malformed yields null, never a throw. */
function readJson(path) {
  try {
    return JSON.parse(readFileSync(path, 'utf8'));
  } catch {
    return null;
  }
}

/**
 * The binary out of a hook command string such as
 * `/usr/local/bin/csr-engine hook stop`. Hook commands are written unquoted and
 * run through a shell, so the first whitespace-delimited token is what the shell
 * executes. A bare `csr-engine ...` resolves through PATH and is covered by the
 * PATH check instead.
 */
function hookBinary(command) {
  if (typeof command !== 'string') return null;
  const first = command.trim().split(/\s+/)[0];
  return first.includes('/') && first.endsWith(`/${BINARY_NAME}`) ? first : null;
}

/** csr-engine paths registered as Claude Code hooks in ~/.claude/settings.json. */
export function readHookBinaries(homeDir) {
  const settings = readJson(join(homeDir, '.claude', 'settings.json'));
  const hooks = settings && typeof settings === 'object' ? settings.hooks : null;
  if (!hooks || typeof hooks !== 'object') return [];

  const found = [];
  for (const entries of Object.values(hooks)) {
    if (!Array.isArray(entries)) continue;
    for (const entry of entries) {
      const inner = entry && Array.isArray(entry.hooks) ? entry.hooks : [];
      for (const hook of inner) {
        const binary = hookBinary(hook && hook.command);
        if (binary) found.push(binary);
      }
    }
  }
  return [...new Set(found)];
}

/** The command Claude Code runs as our MCP server (user scope, ~/.claude.json). */
export function readMcpBinary(homeDir) {
  const config = readJson(join(homeDir, '.claude.json'));
  const servers = config && typeof config === 'object' ? config.mcpServers : null;
  const server = servers && typeof servers === 'object' ? servers[MCP_SERVER_NAME] : null;
  const command = server && typeof server.command === 'string' ? server.command : null;
  return command && command.includes('/') ? command : null;
}

const USE_LABELS = {
  PATH: 'first on PATH',
  hooks: 'Claude Code hooks',
  MCP: 'MCP server',
};

/**
 * Copies of csr-engine that will be run instead of the one we just installed.
 * Compared by realpath, so a symlink into INSTALL_DIR is not reported.
 */
export function detectStaleBinaries({ destPath, homeDir, pathBinary }) {
  const destReal = realPath(destPath);
  const byRealPath = new Map();

  const note = (candidate, use) => {
    if (!candidate) return;
    const real = realPath(candidate);
    if (real === destReal) return;
    const seen = byRealPath.get(real);
    if (seen) {
      if (!seen.uses.includes(use)) seen.uses.push(use);
    } else {
      byRealPath.set(real, { path: candidate, uses: [use] });
    }
  };

  note(pathBinary, 'PATH');
  for (const binary of readHookBinaries(homeDir)) note(binary, 'hooks');
  note(readMcpBinary(homeDir), 'MCP');

  return [...byRealPath.values()];
}

/**
 * The warning text, or null when nothing shadows the destination.
 *
 * The remedies are the ones the engine actually performs:
 *  - hooks: `<dest> setup` runs `hook install --apply`, whose merge drops every
 *    CSR hook entry (matched on the command containing "csr-engine", not on its
 *    path) before re-adding them at the new absolute path. So setup repoints
 *    hooks on its own.
 *  - MCP: setup registers with `claude mcp add`, which refuses to overwrite an
 *    existing server ("MCP server claude-self-reflect already exists in user
 *    config") and exits 1. The old command therefore survives a plain re-run —
 *    it has to be removed first.
 */
export function formatStaleWarning({ stale, destPath }) {
  if (!stale || stale.length === 0) return null;

  const uses = new Set(stale.flatMap((s) => s.uses));
  const lines = [
    '',
    '  \x1b[1;33mWARNING: a different csr-engine is still the one in use.\x1b[0m',
    '',
    `  Just installed:  ${destPath}`,
  ];
  for (const { path, uses: u } of stale) {
    lines.push(`  Still used:      ${path}  (${u.map((x) => USE_LABELS[x]).join(', ')})`);
  }
  lines.push('');
  lines.push('  Nothing was changed outside the install directory — the other copy');
  lines.push('  was left in place.');

  if (uses.has('hooks') || uses.has('MCP')) {
    lines.push('');
    lines.push('  To point Claude Code at the binary just installed, run:');
    lines.push('');
    if (uses.has('MCP')) {
      // `claude mcp add` will not overwrite an existing entry, so setup alone
      // cannot repoint the MCP server.
      lines.push(`    \x1b[1mclaude mcp remove ${MCP_SERVER_NAME} -s user\x1b[0m`);
    }
    lines.push(`    \x1b[1m${destPath} setup\x1b[0m`);
    lines.push('');
    lines.push('  Then restart Claude Code.');
  }

  if (uses.has('PATH')) {
    const shadow = stale.find((s) => s.uses.includes('PATH'));
    lines.push('');
    lines.push(`  \`${BINARY_NAME}\` on your PATH still resolves to ${shadow.path}.`);
    lines.push(`  Run ${destPath} by absolute path, or put its directory earlier`);
    lines.push('  on PATH — that choice is yours, so this is a warning only.');
  }

  lines.push('');
  return lines.join('\n');
}

/**
 * What to tell the user to run: the bare command only when that is really the
 * binary we installed, otherwise the absolute path.
 */
export function activationCommand({ destPath, pathBinary }) {
  if (pathBinary && realPath(pathBinary) === realPath(destPath)) return BINARY_NAME;
  return destPath;
}
