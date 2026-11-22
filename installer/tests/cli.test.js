/**
 * CLI tests for an install or an upgrade whose postinstall never ran (npm
 * 11.20+ skips the script until the person allows it) —
 * `node --test installer/tests/*.test.js`.
 *
 * Hermetic: each case copies cli.js and lib.js into a throwaway directory next
 * to a stub postinstall.js, so no download happens. HOME and PATH are throwaway,
 * the one hardcoded fallback in lib.js (/usr/local/bin) is repointed in the
 * copy, and a `which` that finds nothing comes first on PATH, so a csr-engine
 * installed on the machine running the tests cannot hide the "binary absent"
 * cases.
 */

import { strict as assert } from 'node:assert';
import { after, describe, test } from 'node:test';
import { spawnSync } from 'node:child_process';
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const INSTALLER_DIR = dirname(dirname(fileURLToPath(import.meta.url)));
const CURL_LINE =
  'curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh';

const scratch = [];

function tempDir(prefix) {
  const dir = mkdtempSync(join(tmpdir(), `csr-test-${prefix}-`));
  scratch.push(dir);
  return dir;
}

after(() => {
  for (const dir of scratch) rmSync(dir, { recursive: true, force: true });
});

const PKG_VERSION = JSON.parse(
  readFileSync(join(INSTALLER_DIR, '..', 'package.json'), 'utf8')
).version;
const OLDER = '0.0.1';
const NEWER = '999.0.0';

/**
 * An engine that answers `--version` as the real one does and echoes any other
 * call. A null version is an engine with no `--version` flag (every release up
 * to 9.5.7): it exits 2.
 */
function fakeEngine(version, ran = 'ENGINE-RAN') {
  const answer = version === null ? 'exit 2' : `echo "csr-engine ${version}"; exit 0`;
  return `#!/bin/sh\nif [ "$1" = "--version" ]; then ${answer}; fi\necho "${ran} $*"\n`;
}

// The engine of this package's own version: what the install step leaves behind.
const FAKE_ENGINE = fakeEngine(PKG_VERSION);

// Writes the fake engine where the real install would, logs one line per call,
// and prints what the real postinstall prints on stdout.
const STUB_OK = `
import { appendFileSync, chmodSync, mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
appendFileSync(process.env.STUB_CALLS, 'install\\n');
mkdirSync(process.env.CSR_INSTALL_DIR, { recursive: true });
const dest = join(process.env.CSR_INSTALL_DIR, 'csr-engine');
writeFileSync(dest, ${JSON.stringify(FAKE_ENGINE)});
chmodSync(dest, 0o755);
console.log('\\n  Installed: ' + dest);
console.log('  To activate, run: csr-engine setup');
`;

// What the real postinstall prints when the download fails.
const STUB_FAIL = `
import { appendFileSync } from 'node:fs';
appendFileSync(process.env.STUB_CALLS, 'install\\n');
console.error('\\n  \\x1b[31mBinary download failed:\\x1b[0m HTTP 404 for https://example.invalid/x');
console.error('  Install manually:');
console.error('    ${CURL_LINE}\\n');
process.exit(1);
`;

/** A copy of the installer files with a stub in place of postinstall.js. */
function sandbox(stubSource) {
  const root = tempDir('cli');
  const dir = join(root, 'installer');
  mkdirSync(dir);
  copyFileSync(join(INSTALLER_DIR, 'cli.js'), join(dir, 'cli.js'));
  const lib = readFileSync(join(INSTALLER_DIR, 'lib.js'), 'utf8');
  const fallback = join(root, 'no-system-bin');
  const rewritten = lib.replaceAll('/usr/local/bin/', `${fallback}/`);
  assert.notEqual(rewritten, lib, 'lib.js no longer has the /usr/local/bin fallback to repoint');
  writeFileSync(join(dir, 'lib.js'), rewritten);
  copyFileSync(join(INSTALLER_DIR, '..', 'package.json'), join(root, 'package.json'));
  writeFileSync(join(dir, 'postinstall.js'), stubSource);
  return { root, cli: join(dir, 'cli.js') };
}

/** `engine` is the script to put where the package keeps its engine; none by default. */
function runCli(box, args, { engine, engineOnPath, env = {} } = {}) {
  const home = tempDir('cli-home');
  const installDir = tempDir('cli-install');
  const bin = tempDir('cli-path');
  const calls = join(tempDir('cli-calls'), 'calls.log');
  // PATH keeps /usr/bin and /bin for `sh`. The CLI asks `which` for an engine
  // on PATH, so a `which` that finds nothing comes first: a csr-engine in
  // /usr/bin or /bin cannot stand in for the absent one.
  const which = join(bin, 'which');
  writeFileSync(which, '#!/bin/sh\nexit 1\n');
  chmodSync(which, 0o755);
  if (engineOnPath) {
    const stray = join(bin, 'csr-engine');
    writeFileSync(stray, '#!/bin/sh\necho "STRAY-ENGINE-RAN $*"\n');
    chmodSync(stray, 0o755);
  }
  if (engine) {
    const dest = join(installDir, 'csr-engine');
    writeFileSync(dest, engine);
    chmodSync(dest, 0o755);
  }
  const result = spawnSync(process.execPath, [box.cli, ...args], {
    encoding: 'utf8',
    timeout: 30000,
    env: {
      HOME: home,
      PATH: `${bin}:/usr/bin:/bin`,
      CSR_INSTALL_DIR: installDir,
      STUB_CALLS: calls,
      ...env,
    },
  });
  const installs = existsSync(calls)
    ? readFileSync(calls, 'utf8').split('\n').filter(Boolean).length
    : 0;
  return { ...result, installs, installDir };
}

describe('claude-self-reflect when postinstall never ran', () => {
  test('binary absent, install succeeds: installs once, then runs the command', () => {
    const r = runCli(sandbox(STUB_OK), ['status', '--compact']);

    assert.equal(r.status, 0, r.stderr);
    assert.equal(r.installs, 1);
    assert.match(r.stderr, /the engine is not installed yet/);
    // stdout belongs to the command alone, so `status | jq` keeps working.
    assert.equal(r.stdout, 'ENGINE-RAN status --compact\n');
  });

  test('binary absent, install fails: one message with the manual command, exit 1, no stack', () => {
    const r = runCli(sandbox(STUB_FAIL), ['setup']);

    assert.equal(r.status, 1);
    assert.equal(r.installs, 1);
    assert.equal(r.stdout, '');
    assert.equal(r.stderr.split(CURL_LINE).length - 1, 1, 'the manual command appears exactly once');
    assert.match(r.stderr, /could not install the engine \(HTTP 404 for https:\/\/example\.invalid\/x\)/);
    assert.match(r.stderr, /Run this command, then run your command again/);
    assert.doesNotMatch(r.stderr, /\n\s+at |node:internal|Error:/);
  });

  test('binary absent, install step does nothing: stops after one attempt', () => {
    const quiet = `
      import { appendFileSync } from 'node:fs';
      appendFileSync(process.env.STUB_CALLS, 'install\\n');
    `;
    const r = runCli(sandbox(quiet), ['status']);

    assert.equal(r.status, 1);
    assert.equal(r.installs, 1);
    assert.equal(r.stderr.split(CURL_LINE).length - 1, 1);
  });

  test('a csr-engine elsewhere on PATH does not stand in for the absent engine here', () => {
    // Guards the sandbox, not the CLI: with the machine's `which`, the stray
    // engine is found, the install step is skipped, and the absent cases above
    // test nothing.
    const r = runCli(sandbox(STUB_OK), ['status'], { engineOnPath: true });

    assert.equal(r.status, 0, r.stderr);
    assert.equal(r.installs, 1);
    assert.equal(r.stdout, 'ENGINE-RAN status\n');
  });

  test('binary present: the install step is never run', () => {
    const r = runCli(sandbox(STUB_OK), ['status'], { engine: FAKE_ENGINE });

    assert.equal(r.status, 0, r.stderr);
    assert.equal(r.installs, 0);
    assert.equal(r.stdout, 'ENGINE-RAN status\n');
    assert.equal(r.stderr, '');
  });
});

describe('claude-self-reflect after an upgrade whose postinstall never ran', () => {
  const OLD_ENGINE = fakeEngine(OLDER, 'OLD-ENGINE-RAN');

  test('setup, engine older than the package, install succeeds: updates once, then runs the new engine', () => {
    const r = runCli(sandbox(STUB_OK), ['setup'], { engine: OLD_ENGINE });

    assert.equal(r.status, 0, r.stderr);
    assert.equal(r.installs, 1);
    assert.ok(
      r.stderr.includes(`the engine here is ${OLDER} and this package is ${PKG_VERSION}`),
      r.stderr
    );
    assert.doesNotMatch(r.stderr, /could not update/);
    // The command runs once, with the new engine, and stdout is its alone.
    assert.equal(r.stdout, 'ENGINE-RAN setup\n');
  });

  test('setup, engine older, install fails: one message with the manual command, then the engine that is here', () => {
    const r = runCli(sandbox(STUB_FAIL), ['setup', '--no-companion'], { engine: OLD_ENGINE });

    assert.equal(r.status, 0, r.stderr);
    assert.equal(r.installs, 1);
    assert.equal(r.stdout, 'OLD-ENGINE-RAN setup --no-companion\n');
    assert.equal(r.stderr.split(CURL_LINE).length - 1, 1, 'the manual command appears exactly once');
    assert.match(r.stderr, /could not update the engine \(HTTP 404 for https:\/\/example\.invalid\/x\)/);
    assert.match(r.stderr, /This command runs with the engine that is here/);
    assert.doesNotMatch(r.stderr, /\n\s+at |node:internal|Error:/);
  });

  test('setup, engine older, install step does nothing: one attempt, then the engine that is here', () => {
    const quiet = `
      import { appendFileSync } from 'node:fs';
      appendFileSync(process.env.STUB_CALLS, 'install\\n');
    `;
    const r = runCli(sandbox(quiet), ['setup'], { engine: OLD_ENGINE });

    assert.equal(r.status, 0, r.stderr);
    assert.equal(r.installs, 1);
    assert.equal(r.stdout, 'OLD-ENGINE-RAN setup\n');
    assert.equal(r.stderr.split(CURL_LINE).length - 1, 1);
  });

  test('setup: an engine with no --version flag counts as older', () => {
    const r = runCli(sandbox(STUB_OK), ['setup'], { engine: fakeEngine(null, 'OLD-ENGINE-RAN') });

    assert.equal(r.status, 0, r.stderr);
    assert.equal(r.installs, 1);
    assert.match(r.stderr, /the engine here is an older build/);
    assert.equal(r.stdout, 'ENGINE-RAN setup\n');
  });

  test('status, engine older: one line names the command to run, no install step, the engine that is here', () => {
    // A command that only reads must not wait on a download or restart a daemon.
    const r = runCli(sandbox(STUB_OK), ['status', '--compact'], { engine: OLD_ENGINE });

    assert.equal(r.status, 0, r.stderr);
    assert.equal(r.installs, 0);
    assert.equal(r.stdout, 'OLD-ENGINE-RAN status --compact\n');
    assert.equal(
      r.stderr,
      `Self-Reflect: the engine here is ${OLDER} and this package is ${PKG_VERSION}. ` +
        'To update the engine, run: claude-self-reflect setup\n'
    );
  });

  test('setup, engine newer than the package: left alone, no install step, no message', () => {
    const r = runCli(sandbox(STUB_OK), ['setup'], { engine: fakeEngine(NEWER, 'NEW-ENGINE-RAN') });

    assert.equal(r.status, 0, r.stderr);
    assert.equal(r.installs, 0);
    assert.equal(r.stdout, 'NEW-ENGINE-RAN setup\n');
    assert.equal(r.stderr, '');
  });

  test('CSR_SKIP_BINARY_DOWNLOAD=1: an older engine is left alone, by setup and by status', () => {
    for (const command of ['setup', 'status']) {
      const r = runCli(sandbox(STUB_OK), [command], {
        engine: OLD_ENGINE,
        env: { CSR_SKIP_BINARY_DOWNLOAD: '1' },
      });

      assert.equal(r.status, 0, r.stderr);
      assert.equal(r.installs, 0);
      assert.equal(r.stdout, `OLD-ENGINE-RAN ${command}\n`);
      assert.equal(r.stderr, '');
    }
  });
});
