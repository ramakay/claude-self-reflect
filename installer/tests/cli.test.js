/**
 * CLI tests for an install whose postinstall never ran (npm 11.20+ skips the
 * script until the person allows it) — `node --test installer/tests/*.test.js`.
 *
 * Hermetic: each case copies cli.js and lib.js into a throwaway directory next
 * to a stub postinstall.js, so no download happens. HOME and PATH are throwaway,
 * and the one hardcoded fallback in lib.js (/usr/local/bin) is repointed in the
 * copy, so a csr-engine installed on the machine running the tests cannot hide
 * the "binary absent" cases.
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

const FAKE_ENGINE = '#!/bin/sh\necho "ENGINE-RAN $*"\n';

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

function runCli(box, args, { withEngine } = {}) {
  const home = tempDir('cli-home');
  const installDir = tempDir('cli-install');
  const bin = tempDir('cli-path');
  const calls = join(tempDir('cli-calls'), 'calls.log');
  if (withEngine) {
    const engine = join(installDir, 'csr-engine');
    writeFileSync(engine, FAKE_ENGINE);
    chmodSync(engine, 0o755);
  }
  const result = spawnSync(process.execPath, [box.cli, ...args], {
    encoding: 'utf8',
    timeout: 30000,
    env: {
      HOME: home,
      PATH: `${bin}:/usr/bin:/bin`,
      CSR_INSTALL_DIR: installDir,
      STUB_CALLS: calls,
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

  test('binary present: the install step is never run', () => {
    const r = runCli(sandbox(STUB_OK), ['status'], { withEngine: true });

    assert.equal(r.status, 0, r.stderr);
    assert.equal(r.installs, 0);
    assert.equal(r.stdout, 'ENGINE-RAN status\n');
    assert.equal(r.stderr, '');
  });
});
