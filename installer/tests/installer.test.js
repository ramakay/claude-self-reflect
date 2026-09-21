/**
 * Hermetic installer tests — `node --test installer/tests/`.
 *
 * No network, no package manager, and never the real HOME: every case builds a
 * throwaway directory and points the code at it. The one case that runs
 * postinstall.js for real takes the "already installed" branch, so it cannot
 * reach the download path.
 */

import { strict as assert } from 'node:assert';
import { after, describe, test } from 'node:test';
import { spawnSync } from 'node:child_process';
import {
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

import {
  activationCommand,
  detectStaleBinaries,
  formatStaleWarning,
  parseVersion,
  planInstall,
  readHookBinaries,
  readMcpBinary,
} from '../lib.js';

const INSTALLER_DIR = dirname(dirname(fileURLToPath(import.meta.url)));
const POSTINSTALL = join(INSTALLER_DIR, 'postinstall.js');
const PKG_VERSION = JSON.parse(
  readFileSync(join(INSTALLER_DIR, '..', 'package.json'), 'utf8')
).version;

const scratch = [];

function tempDir(prefix) {
  const dir = mkdtempSync(join(tmpdir(), `csr-test-${prefix}-`));
  scratch.push(dir);
  return dir;
}

after(() => {
  for (const dir of scratch) rmSync(dir, { recursive: true, force: true });
});

/** A stand-in csr-engine: prints `csr-engine <version>` for --version, or fails. */
function fakeBinary(dir, name, version) {
  const path = join(dir, name);
  const body =
    version === null
      ? '#!/bin/sh\necho "error: unexpected argument \'--version\' found" >&2\nexit 2\n'
      : `#!/bin/sh\n[ "$1" = "--version" ] || { echo "unexpected: $*" >&2; exit 2; }\necho "csr-engine ${version}"\n`;
  writeFileSync(path, body);
  chmodSync(path, 0o755);
  return path;
}

function writeHomeSettings(home, json) {
  mkdirSync(join(home, '.claude'), { recursive: true });
  writeFileSync(join(home, '.claude', 'settings.json'), json);
}

function hookSettings(binaryPath) {
  return JSON.stringify({
    hooks: {
      SessionStart: [
        {
          matcher: 'startup|resume|compact',
          hooks: [{ type: 'command', command: `${binaryPath} hook session-start` }],
        },
      ],
      Stop: [{ hooks: [{ type: 'command', command: `${binaryPath} hook stop` }] }],
    },
  });
}

describe('version parsing', () => {
  test('reads clap output', () => {
    assert.equal(parseVersion('csr-engine 10.1.0\n'), '10.1.0');
    assert.equal(parseVersion('csr-engine 10.1.0-rc.2'), '10.1.0-rc.2');
  });

  test('refuses anything that is not a version', () => {
    assert.equal(parseVersion(''), null);
    assert.equal(parseVersion(null), null);
    assert.equal(parseVersion("error: unexpected argument '--version' found"), null);
  });
});

describe('install decision (destination only)', () => {
  test('binary at dest that cannot answer --version is reinstalled', () => {
    const dir = tempDir('old');
    const dest = fakeBinary(dir, 'csr-engine', null);
    const plan = planInstall({ destPath: dest, pkgVersion: '10.1.0' });
    assert.deepEqual(plan, { action: 'install', reason: 'unknown', installedVersion: null });
  });

  test('matching version at dest is skipped and never downloads', () => {
    const dir = tempDir('match');
    const dest = fakeBinary(dir, 'csr-engine', '10.1.0');
    let downloads = 0;
    const plan = planInstall({ destPath: dest, pkgVersion: '10.1.0' });
    if (plan.action === 'install') downloads += 1;
    assert.equal(plan.action, 'skip');
    assert.equal(plan.installedVersion, '10.1.0');
    assert.equal(downloads, 0, 'skip must not reach the downloader');
  });

  test('different version at dest is replaced', () => {
    const dir = tempDir('diff');
    const dest = fakeBinary(dir, 'csr-engine', '9.5.7');
    const plan = planInstall({ destPath: dest, pkgVersion: '10.1.0' });
    assert.deepEqual(plan, { action: 'install', reason: 'different', installedVersion: '9.5.7' });
  });

  test('missing dest installs, and a stale PATH copy does not make it skip', () => {
    const dir = tempDir('missing');
    // A current-version copy somewhere else on the machine must not be mistaken
    // for the destination — that is the dead check this replaces.
    fakeBinary(dir, 'csr-engine-elsewhere', '10.1.0');
    const plan = planInstall({ destPath: join(dir, 'csr-engine'), pkgVersion: '10.1.0' });
    assert.deepEqual(plan, { action: 'install', reason: 'missing', installedVersion: null });
  });

  test('only --version is ever run against a pre-existing binary', () => {
    const calls = [];
    const probe = (binaryPath) => {
      calls.push(binaryPath);
      return '10.1.0';
    };
    const dir = tempDir('probe');
    const dest = fakeBinary(dir, 'csr-engine', '10.1.0');
    planInstall({ destPath: dest, pkgVersion: '10.1.0', probe });
    assert.deepEqual(calls, [dest], 'exactly one probe, against the destination');
  });
});

describe('stale copy detection', () => {
  test('a stale PATH copy is named in the warning', () => {
    const home = tempDir('home-path');
    const other = tempDir('other');
    const dest = fakeBinary(tempDir('dest'), 'csr-engine', '10.1.0');
    const shadow = fakeBinary(other, 'csr-engine', '9.5.7');

    const stale = detectStaleBinaries({ destPath: dest, homeDir: home, pathBinary: shadow });
    assert.deepEqual(stale, [{ path: shadow, uses: ['PATH'] }]);

    const warning = formatStaleWarning({ stale, destPath: dest });
    assert.match(warning, new RegExp(shadow.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
    assert.match(warning, /first on PATH/);
  });

  test('a symlink to the destination is not reported', () => {
    const home = tempDir('home-link');
    const destDir = tempDir('dest-link');
    const linkDir = tempDir('link');
    const dest = fakeBinary(destDir, 'csr-engine', '10.1.0');
    const link = join(linkDir, 'csr-engine');
    symlinkSync(dest, link);

    const stale = detectStaleBinaries({ destPath: dest, homeDir: home, pathBinary: link });
    assert.deepEqual(stale, []);
    assert.equal(formatStaleWarning({ stale, destPath: dest }), null);
  });

  test('hooks pointing at another absolute path are reported with the MCP remedy', () => {
    const home = tempDir('home-hooks');
    const dest = fakeBinary(tempDir('dest-hooks'), 'csr-engine', '10.1.0');
    const registered = '/usr/local/bin/csr-engine';
    writeHomeSettings(home, hookSettings(registered));
    writeFileSync(
      join(home, '.claude.json'),
      JSON.stringify({
        mcpServers: { 'claude-self-reflect': { type: 'stdio', command: registered, args: [] } },
      })
    );

    assert.deepEqual(readHookBinaries(home), [registered]);
    assert.equal(readMcpBinary(home), registered);

    const stale = detectStaleBinaries({ destPath: dest, homeDir: home, pathBinary: null });
    assert.deepEqual(stale, [{ path: registered, uses: ['hooks', 'MCP'] }]);

    const warning = formatStaleWarning({ stale, destPath: dest });
    assert.match(warning, /Claude Code hooks, MCP server/);
    // `claude mcp add` refuses to overwrite an existing entry, so setup alone
    // cannot repoint the MCP server.
    assert.match(warning, /claude mcp remove claude-self-reflect -s user/);
    assert.match(warning, new RegExp(`${dest.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')} setup`));
  });

  test('a bare `csr-engine` hook command is left to the PATH check', () => {
    const home = tempDir('home-bare');
    writeHomeSettings(home, hookSettings('csr-engine'));
    assert.deepEqual(readHookBinaries(home), []);
  });

  test('malformed or unreadable settings produce no warning and no crash', () => {
    const home = tempDir('home-broken');
    const dest = fakeBinary(tempDir('dest-broken'), 'csr-engine', '10.1.0');

    writeHomeSettings(home, '{ this is not json');
    writeFileSync(join(home, '.claude.json'), '\u0000not json either');
    assert.deepEqual(readHookBinaries(home), []);
    assert.equal(readMcpBinary(home), null);
    assert.deepEqual(detectStaleBinaries({ destPath: dest, homeDir: home, pathBinary: null }), []);

    // Unreadable (chmod 000) is the same story: fail open, stay quiet.
    chmodSync(join(home, '.claude', 'settings.json'), 0o000);
    assert.deepEqual(readHookBinaries(home), []);
    chmodSync(join(home, '.claude', 'settings.json'), 0o600);

    // And an entirely empty home.
    const empty = tempDir('home-empty');
    assert.deepEqual(detectStaleBinaries({ destPath: dest, homeDir: empty, pathBinary: null }), []);
  });
});

describe('activation hint', () => {
  test('uses the absolute path when bare csr-engine resolves elsewhere', () => {
    const dest = fakeBinary(tempDir('dest-hint'), 'csr-engine', '10.1.0');
    const shadow = fakeBinary(tempDir('shadow-hint'), 'csr-engine', '9.5.7');
    assert.equal(activationCommand({ destPath: dest, pathBinary: shadow }), dest);
    assert.equal(activationCommand({ destPath: dest, pathBinary: null }), dest);
  });

  test('uses the bare command when PATH resolves to the destination', () => {
    const destDir = tempDir('dest-hint2');
    const dest = fakeBinary(destDir, 'csr-engine', '10.1.0');
    const link = join(tempDir('link-hint'), 'csr-engine');
    symlinkSync(dest, link);
    assert.equal(activationCommand({ destPath: dest, pathBinary: dest }), 'csr-engine');
    assert.equal(activationCommand({ destPath: dest, pathBinary: link }), 'csr-engine');
  });
});

describe('postinstall end to end (skip branch, no network)', () => {
  test('already-installed destination skips the download and warns about the shadow', () => {
    const home = tempDir('e2e-home');
    const installDir = tempDir('e2e-install');
    const shadowDir = tempDir('e2e-shadow');

    fakeBinary(installDir, 'csr-engine', PKG_VERSION);
    const shadow = fakeBinary(shadowDir, 'csr-engine', '9.5.7');
    writeHomeSettings(home, hookSettings(shadow));

    const result = spawnSync(process.execPath, [POSTINSTALL], {
      encoding: 'utf8',
      timeout: 60000,
      env: {
        ...process.env,
        HOME: home,
        PATH: `${shadowDir}:${process.env.PATH}`,
        CSR_INSTALL_DIR: installDir,
        CSR_FORCE_POSTINSTALL: '1',
        CSR_AUTO_SETUP: '',
      },
    });

    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /already installed/);
    assert.doesNotMatch(result.stdout, /Downloading/);
    assert.match(result.stdout, new RegExp(shadow.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
    assert.match(result.stdout, /first on PATH, Claude Code hooks/);
    assert.match(
      result.stdout,
      new RegExp(`${join(installDir, 'csr-engine').replace(/[.*+?^${}()|[\]\\]/g, '\\$&')} setup`)
    );
  });
});
