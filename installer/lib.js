/**
 * Installer helpers, kept out of postinstall.js so the install decision, the
 * binary write and the stale-copy detection can be tested without a network, a
 * package manager, or the user's real HOME.
 *
 * Only installBinary writes, and only inside the directory it is given.
 * Everything else is read-only: a user may deliberately keep a different
 * csr-engine build first on PATH, so a shadowed install is reported, never
 * corrected.
 */

import {
  accessSync,
  closeSync,
  constants,
  existsSync,
  fchmodSync,
  fstatSync,
  openSync,
  readSync,
  realpathSync,
  renameSync,
  statSync,
  unlinkSync,
  writeSync,
} from 'fs';
import { execFileSync } from 'child_process';
import { randomBytes } from 'crypto';
import { basename, dirname, join } from 'path';

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

// A path made only of these needs no quoting, which keeps the common hint
// readable. Anything else — spaces, quotes, $, ;, backticks — is quoted.
const SHELL_SAFE = /^[A-Za-z0-9_./-]+$/;

/**
 * Make a path safe to paste into a shell. Printed commands are meant to be
 * run: an install directory like `/Users/alice/CSR Tools/bin` would otherwise
 * produce a hint that executes `/Users/alice/CSR`, and a directory name with
 * shell metacharacters would run whatever they say.
 */
export function shellQuote(value) {
  const str = String(value);
  if (SHELL_SAFE.test(str)) return str;
  return `'${str.replace(/'/g, "'\\''")}'`;
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

/**
 * Put `sourcePath` at `destPath` without ever writing through the destination.
 *
 * Copying straight onto the destination follows a symlink or hard link sitting
 * there, so an upgrade could overwrite a binary elsewhere on the system while
 * the installer claims nothing outside the install directory changed — and an
 * interrupted copy would leave the existing executable truncated. So: stage,
 * then rename. Rename is atomic and cannot hit ETXTBSY on Linux when the old
 * binary is still running.
 *
 * The stage name is random and created with `wx`, i.e. `O_CREAT|O_EXCL`. A
 * predictable name (a pid) could be pre-planted as a symlink by anyone who can
 * write to the install directory, and the copy would then follow it right back
 * outside. Exclusive creation fails on an existing name of any kind, and every
 * byte is written through that one descriptor — never reopened by name.
 */
export function installBinary(sourcePath, destPath) {
  const stagePath = join(
    dirname(destPath),
    `.${basename(destPath)}.${randomBytes(8).toString('hex')}.tmp`
  );

  let stageFd = null;
  let sourceFd = null;
  try {
    stageFd = openSync(stagePath, 'wx', 0o755);
    sourceFd = openSync(sourcePath, constants.O_RDONLY);

    const buffer = Buffer.allocUnsafe(1024 * 1024);
    for (;;) {
      const read = readSync(sourceFd, buffer, 0, buffer.length, null);
      if (read <= 0) break;
      let written = 0;
      while (written < read) {
        written += writeSync(stageFd, buffer, written, read - written);
      }
    }

    // `wx` honours the umask, so set the mode explicitly on the descriptor.
    fchmodSync(stageFd, 0o755);
    closeSync(stageFd);
    stageFd = null;

    renameSync(stagePath, destPath);
  } catch (e) {
    if (stageFd !== null) {
      try {
        closeSync(stageFd);
      } catch {}
    }
    try {
      unlinkSync(stagePath);
    } catch {}
    throw e;
  } finally {
    if (sourceFd !== null) {
      try {
        closeSync(sourceFd);
      } catch {}
    }
  }
}

/** A candidate is usable only if it is a regular file we may execute. */
export function isRunnableBinary(p) {
  try {
    if (!statSync(p).isFile()) return false;
    accessSync(p, constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

/**
 * Pick the csr-engine to run: the package-managed copy first, then PATH, then
 * /usr/local/bin. Existence is not enough — a half-written or non-executable
 * file at the destination must not hide a working fallback.
 */
export function findEngineBinary({ installDir, pathBinary }) {
  const candidates = [join(installDir, BINARY_NAME)];
  if (pathBinary) candidates.push(pathBinary);
  candidates.push(`/usr/local/bin/${BINARY_NAME}`);

  for (const p of candidates) {
    if (isRunnableBinary(p)) return p;
  }
  return null;
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

// Claude Code's configs are small. A multi-gigabyte valid JSON document would
// exhaust the Node heap before any try/catch could help, and a config path
// resolving to a FIFO would block the install forever.
const MAX_CONFIG_BYTES = 32 * 1024 * 1024;

/**
 * Read and parse a JSON config, fail open.
 *
 * Opens non-blocking so a FIFO cannot hang the installer, refuses anything that
 * is not a regular file, and refuses anything over 32 MiB. Any read or parse
 * failure yields null — no warning, no crash.
 */
export function readJsonConfig(path) {
  let fd = null;
  try {
    fd = openSync(path, constants.O_RDONLY | constants.O_NONBLOCK);
    const stat = fstatSync(fd);
    if (!stat.isFile() || stat.size > MAX_CONFIG_BYTES) return null;

    const buffer = Buffer.allocUnsafe(stat.size);
    let read = 0;
    while (read < stat.size) {
      const n = readSync(fd, buffer, read, stat.size - read, read);
      if (n <= 0) break;
      read += n;
    }
    return JSON.parse(buffer.subarray(0, read).toString('utf8'));
  } catch {
    return null;
  } finally {
    if (fd !== null) {
      try {
        closeSync(fd);
      } catch {}
    }
  }
}

function stripQuotes(value) {
  const first = value[0];
  const last = value[value.length - 1];
  if (value.length >= 2 && first === last && (first === "'" || first === '"')) {
    return value.slice(1, -1);
  }
  return value;
}

/**
 * Decode a leading POSIX single-quoted word, which is what setup now writes.
 * Inside single quotes everything is literal, and a literal apostrophe appears
 * as `'\''` — close, escaped quote, reopen. Returns null when unterminated.
 */
function decodeSingleQuoted(command) {
  let out = '';
  let i = 1;
  while (i < command.length) {
    if (command[i] !== "'") {
      out += command[i];
      i += 1;
    } else if (command.startsWith("'\\''", i)) {
      out += "'";
      i += 4;
    } else {
      return out;
    }
  }
  return null;
}

/**
 * The executable out of a hook command string such as
 * `/usr/local/bin/csr-engine hook stop`.
 *
 * Setup writes `<binary> hook <name>` with the binary shell-quoted when it
 * needs it, so a leading `'` means the whole word is quoted and may contain
 * `'\''`. Otherwise everything before the first ` hook ` is the executable —
 * splitting on whitespace instead would lose `/Users/alice/CSR Tools/csr-engine`.
 *
 * Decoding stays lenient on purpose: settings.json files written by earlier
 * releases carry the bare unquoted form, and detection has to recognise those
 * too. A bare `csr-engine ...` resolves through PATH and is left to the PATH
 * check.
 */
function hookBinary(command) {
  if (typeof command !== 'string') return null;
  const trimmed = command.trim();

  let executable;
  if (trimmed.startsWith("'")) {
    executable = decodeSingleQuoted(trimmed);
    if (executable === null) return null;
  } else {
    const marker = trimmed.indexOf(' hook ');
    const raw = marker === -1 ? trimmed.split(/\s+/)[0] : trimmed.slice(0, marker);
    executable = stripQuotes(raw.trim());
  }

  return executable.includes('/') && basename(executable) === BINARY_NAME ? executable : null;
}

/** csr-engine paths registered as Claude Code hooks in ~/.claude/settings.json. */
export function readHookBinaries(homeDir) {
  const settings = readJsonConfig(join(homeDir, '.claude', 'settings.json'));
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

/**
 * The command Claude Code runs as our MCP server — user scope only. Project
 * scope lives under `projects[<dir>].mcpServers` and is not ours to report.
 * The whole string is the command; it is not a shell line.
 */
export function readMcpBinary(homeDir) {
  const config = readJsonConfig(join(homeDir, '.claude.json'));
  const servers = config && typeof config === 'object' ? config.mcpServers : null;
  const server = servers && typeof servers === 'object' ? servers[MCP_SERVER_NAME] : null;
  const command = server && typeof server.command === 'string' ? server.command.trim() : null;
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

/** Entries that setup is expected to repoint, i.e. everything except PATH. */
export function registrationStale(stale) {
  return stale.filter((s) => s.uses.includes('hooks') || s.uses.includes('MCP'));
}

/**
 * The warning text, or null when nothing shadows the destination.
 *
 * The remedy is `<dest> setup`, verified against the engine: the hook merge
 * evicts CSR entries by command content rather than by path, and
 * register_mcp_server removes an existing user-scope registration before
 * re-adding it, so one setup run repoints both.
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
    lines.push(`    \x1b[1m${shellQuote(destPath)} setup\x1b[0m`);
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
 * What to print instead of "Done" when setup ran but Claude Code is still
 * pointed at another binary. Setup claiming success is not evidence that the
 * registrations moved, so the installers re-read them and say so.
 */
export function formatPendingActivation({ stale, destPath }) {
  const lines = [
    '',
    '  \x1b[1;33mNot active yet.\x1b[0m Setup ran, but Claude Code still points at',
    '  another csr-engine:',
    '',
  ];
  for (const { path, uses } of stale) {
    lines.push(`    ${path}  (${uses.map((x) => USE_LABELS[x]).join(', ')})`);
  }
  lines.push('');
  lines.push('  Register it by hand, then restart Claude Code:');
  lines.push('');
  lines.push(`    claude mcp remove ${MCP_SERVER_NAME} -s user`);
  lines.push(`    ${shellQuote(destPath)} setup`);
  lines.push('');
  return lines.join('\n');
}

/**
 * What to tell the user to run, ready to paste into a shell: the bare command
 * only when that is really the binary we installed, otherwise the quoted
 * absolute path.
 */
export function activationCommand({ destPath, pathBinary }) {
  if (pathBinary && realPath(pathBinary) === realPath(destPath)) return BINARY_NAME;
  return shellQuote(destPath);
}
