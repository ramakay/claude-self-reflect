/**
 * Hermetic installer tests — `node --test installer/tests/*.test.js`.
 *
 * No network, no package manager, and never the real HOME: every case builds a
 * throwaway directory and points the code at it. The cases that run
 * postinstall.js for real take the "already installed" branch, so they cannot
 * reach the download path.
 */

import { strict as assert } from 'node:assert';
import { after, describe, test } from 'node:test';
import { spawnSync } from 'node:child_process';
import {
  chmodSync,
  closeSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  linkSync,
  openSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
  writeSync,
} from 'node:fs';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

import {
  activationCommand,
  detectStaleBinaries,
  findEngineBinary,
  formatPendingActivation,
  formatStaleWarning,
  installBinary,
  isRunnableBinary,
  parseVersion,
  planInstall,
  probeVersion,
  readHookBinaries,
  readJsonConfig,
  readMcpBinary,
  registrationStale,
  shellQuote,
  whichBinary,
} from '../lib.js';

const INSTALLER_DIR = dirname(dirname(fileURLToPath(import.meta.url)));
const POSTINSTALL = join(INSTALLER_DIR, 'postinstall.js');
const PKG_VERSION = JSON.parse(
  readFileSync(join(INSTALLER_DIR, '..', 'package.json'), 'utf8')
).version;
const IS_ROOT = typeof process.getuid === 'function' && process.getuid() === 0;

const scratch = [];

function tempDir(prefix) {
  const dir = mkdtempSync(join(tmpdir(), `csr-test-${prefix}-`));
  scratch.push(dir);
  return dir;
}

after(() => {
  for (const dir of scratch) {
    try {
      chmodSync(join(dir, '.claude', 'settings.json'), 0o600);
    } catch {}
    rmSync(dir, { recursive: true, force: true });
  }
});

/** Escape a path for use inside a RegExp. */
function re(value) {
  return new RegExp(value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'));
}

/** A stand-in csr-engine: prints `csr-engine <version>` for --version, or fails. */
function fakeBinary(dir, name, version, setupWrites) {
  const path = join(dir, name);
  let body;
  if (version === null) {
    body = '#!/bin/sh\necho "error: unexpected argument \'--version\' found" >&2\nexit 2\n';
  } else {
    body = `#!/bin/sh
case "$1" in
  --version) echo "csr-engine ${version}" ;;
`;
    if (setupWrites) {
      // Stand in for the engine's setup: register hooks and the MCP server at
      // whatever path this fake was told to claim.
      body += `  setup)
    mkdir -p "$HOME/.claude"
    printf '{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"%s hook stop"}]}]}}' '${setupWrites}' > "$HOME/.claude/settings.json"
    printf '{"mcpServers":{"claude-self-reflect":{"type":"stdio","command":"%s"}}}' '${setupWrites}' > "$HOME/.claude.json"
    echo "=== Setup Complete ===" ;;
`;
    }
    body += '  *) echo "unexpected: $*" >&2; exit 2 ;;\nesac\n';
  }
  writeFileSync(path, body);
  chmodSync(path, 0o755);
  return path;
}

function writeHomeSettings(home, json) {
  mkdirSync(join(home, '.claude'), { recursive: true });
  const path = join(home, '.claude', 'settings.json');
  writeFileSync(path, json);
  return path;
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

function mcpConfig(home, command) {
  writeFileSync(
    join(home, '.claude.json'),
    JSON.stringify({
      mcpServers: { 'claude-self-reflect': { type: 'stdio', command, args: [] } },
    })
  );
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
    const dest = fakeBinary(tempDir('old'), 'csr-engine', null);
    const plan = planInstall({ destPath: dest, pkgVersion: '10.1.0' });
    assert.deepEqual(plan, { action: 'install', reason: 'unknown', installedVersion: null });
  });

  test('matching version at dest is skipped', () => {
    const dest = fakeBinary(tempDir('match'), 'csr-engine', '10.1.0');
    const plan = planInstall({ destPath: dest, pkgVersion: '10.1.0' });
    assert.deepEqual(plan, { action: 'skip', reason: 'match', installedVersion: '10.1.0' });
  });

  test('different version at dest is replaced', () => {
    const dest = fakeBinary(tempDir('diff'), 'csr-engine', '9.5.7');
    const plan = planInstall({ destPath: dest, pkgVersion: '10.1.0' });
    assert.deepEqual(plan, { action: 'install', reason: 'different', installedVersion: '9.5.7' });
  });

  test('missing dest installs, and a current copy elsewhere does not make it skip', () => {
    const dir = tempDir('missing');
    fakeBinary(dir, 'csr-engine-elsewhere', '10.1.0');
    const plan = planInstall({ destPath: join(dir, 'csr-engine'), pkgVersion: '10.1.0' });
    assert.deepEqual(plan, { action: 'install', reason: 'missing', installedVersion: null });
  });

  test('the pre-existing binary is invoked exactly once, with exactly --version', () => {
    const dest = fakeBinary(tempDir('probe'), 'csr-engine', '10.1.0');
    const calls = [];
    const exec = (file, args) => {
      calls.push({ file, args });
      return 'csr-engine 10.1.0\n';
    };
    const plan = planInstall({
      destPath: dest,
      pkgVersion: '10.1.0',
      probe: (p) => probeVersion(p, exec),
    });

    assert.equal(plan.action, 'skip');
    assert.equal(calls.length, 1, 'exactly one invocation');
    assert.equal(calls[0].file, dest, 'against the destination, not PATH');
    assert.deepEqual(calls[0].args, ['--version'], 'no status/setup/hook/eval, ever');
  });
});

describe('writing the binary', () => {
  test('a symlinked destination is replaced, not written through', () => {
    const installDir = tempDir('link-dest');
    const outsideDir = tempDir('outside');
    const outside = join(outsideDir, 'other-install');
    writeFileSync(outside, 'ORIGINAL OTHER INSTALL');
    const before = readFileSync(outside);

    const dest = join(installDir, 'csr-engine');
    symlinkSync(outside, dest);

    const source = join(tempDir('src'), 'csr-engine');
    writeFileSync(source, 'NEW BINARY');

    installBinary(source, dest);

    assert.deepEqual(readFileSync(outside), before, 'the file outside INSTALL_DIR is untouched');
    assert.equal(readFileSync(outside, 'utf8'), 'ORIGINAL OTHER INSTALL');
    assert.equal(lstatSync(dest).isSymbolicLink(), false, 'destination is a regular file now');
    assert.equal(lstatSync(dest).isFile(), true);
    assert.equal(readFileSync(dest, 'utf8'), 'NEW BINARY');
    assert.equal(statSync(dest).mode & 0o777, 0o755);
    assert.deepEqual(readdirSync(installDir), ['csr-engine'], 'no staging file left behind');
  });

  test('a hard-linked destination is replaced, not written through', () => {
    const installDir = tempDir('hard-dest');
    const outside = join(tempDir('outside2'), 'other-install');
    writeFileSync(outside, 'ORIGINAL OTHER INSTALL');

    const dest = join(installDir, 'csr-engine');
    linkSync(outside, dest);

    const source = join(tempDir('src2'), 'csr-engine');
    writeFileSync(source, 'NEW BINARY');

    installBinary(source, dest);

    assert.equal(readFileSync(outside, 'utf8'), 'ORIGINAL OTHER INSTALL');
    assert.equal(readFileSync(dest, 'utf8'), 'NEW BINARY');
    assert.equal(statSync(outside).nlink, 1, 'the hard link was broken, not followed');
  });

  test('a failed write leaves no staging file and no damaged destination', () => {
    const installDir = tempDir('fail-dest');
    const dest = join(installDir, 'csr-engine');
    writeFileSync(dest, 'EXISTING BINARY');
    chmodSync(dest, 0o755);

    assert.throws(() => installBinary(join(installDir, 'does-not-exist'), dest));
    assert.equal(readFileSync(dest, 'utf8'), 'EXISTING BINARY', 'untouched on failure');
    assert.deepEqual(readdirSync(installDir), ['csr-engine'], 'staging file cleaned up');
  });
});

describe('bounded config reads', () => {
  test('reads a normal config', () => {
    const home = tempDir('bounded-ok');
    writeHomeSettings(home, JSON.stringify({ hooks: { Stop: [] } }));
    assert.deepEqual(readJsonConfig(join(home, '.claude', 'settings.json')), {
      hooks: { Stop: [] },
    });
  });

  test('refuses a file over the size cap even when it is valid JSON', () => {
    const home = tempDir('bounded-big');
    const path = join(home, 'big.json');
    // Valid JSON padded with whitespace past 32 MiB: a naive reader parses it,
    // the bounded reader must not.
    const fd = openSync(path, 'w');
    writeSync(fd, '{"hooks":{}}');
    writeSync(fd, Buffer.alloc(33 * 1024 * 1024, 0x20));
    closeSync(fd);

    assert.ok(statSync(path).size > 32 * 1024 * 1024);
    assert.equal(JSON.parse(readFileSync(path, 'utf8')).hooks !== undefined, true, 'still valid');
    assert.equal(readJsonConfig(path), null);
  });

  test('refuses a FIFO instead of blocking on it', (t) => {
    const home = tempDir('bounded-fifo');
    const path = join(home, 'fifo.json');
    const made = spawnSync('mkfifo', [path]);
    if (made.status !== 0) {
      t.skip('mkfifo unavailable');
      return;
    }
    assert.equal(readJsonConfig(path), null);
  });

  test('refuses a directory', () => {
    const home = tempDir('bounded-dir');
    mkdirSync(join(home, 'adir'));
    assert.equal(readJsonConfig(join(home, 'adir')), null);
  });
});

describe('stale copy detection', () => {
  test('a stale PATH copy, found through a real PATH, is named in the warning', () => {
    const home = tempDir('home-path');
    const shadowDir = tempDir('other');
    const dest = fakeBinary(tempDir('dest'), 'csr-engine', '10.1.0');
    const shadow = fakeBinary(shadowDir, 'csr-engine', '9.5.7');

    const originalPath = process.env.PATH;
    let pathBinary;
    try {
      process.env.PATH = `${shadowDir}:${originalPath}`;
      pathBinary = whichBinary();
    } finally {
      process.env.PATH = originalPath;
    }
    assert.equal(pathBinary, shadow, 'whichBinary resolved the fixture on PATH');

    const stale = detectStaleBinaries({ destPath: dest, homeDir: home, pathBinary });
    assert.deepEqual(stale, [{ path: shadow, uses: ['PATH'] }]);

    const warning = formatStaleWarning({ stale, destPath: dest });
    assert.match(warning, re(shadow));
    assert.match(warning, /first on PATH/);
  });

  test('a symlink to the destination is not reported', () => {
    const home = tempDir('home-link');
    const dest = fakeBinary(tempDir('dest-link'), 'csr-engine', '10.1.0');
    const link = join(tempDir('link'), 'csr-engine');
    symlinkSync(dest, link);

    const stale = detectStaleBinaries({ destPath: dest, homeDir: home, pathBinary: link });
    assert.deepEqual(stale, []);
    assert.equal(formatStaleWarning({ stale, destPath: dest }), null);
  });

  test('hooks and MCP at another absolute path are reported with the setup remedy', () => {
    const home = tempDir('home-hooks');
    const dest = fakeBinary(tempDir('dest-hooks'), 'csr-engine', '10.1.0');
    const registered = '/usr/local/bin/csr-engine';
    writeHomeSettings(home, hookSettings(registered));
    mcpConfig(home, registered);

    assert.deepEqual(readHookBinaries(home), [registered]);
    assert.equal(readMcpBinary(home), registered);

    const stale = detectStaleBinaries({ destPath: dest, homeDir: home, pathBinary: null });
    assert.deepEqual(stale, [{ path: registered, uses: ['hooks', 'MCP'] }]);
    assert.deepEqual(registrationStale(stale), stale);

    const warning = formatStaleWarning({ stale, destPath: dest });
    assert.match(warning, /Claude Code hooks, MCP server/);
    assert.match(warning, re(`${dest} setup`));
    // setup itself now removes and re-adds the user-scope registration, so the
    // manual remove must not appear in the ordinary remedy.
    assert.doesNotMatch(warning, /claude mcp remove/);
  });

  test('a project-scoped MCP entry is not mistaken for the user-scope one', () => {
    const home = tempDir('home-proj');
    const dest = fakeBinary(tempDir('dest-proj'), 'csr-engine', '10.1.0');
    writeFileSync(
      join(home, '.claude.json'),
      JSON.stringify({
        mcpServers: { 'claude-self-reflect': { command: dest } },
        projects: {
          '/Users/me/work': { mcpServers: { 'claude-self-reflect': { command: '/opt/old/csr-engine' } } },
        },
      })
    );
    assert.equal(readMcpBinary(home), dest);
    assert.deepEqual(detectStaleBinaries({ destPath: dest, homeDir: home, pathBinary: null }), []);
  });

  test('paths with spaces survive, quoted or not', () => {
    const home = tempDir('home-space');
    const spaced = '/Users/alice/CSR Tools/bin/csr-engine';
    writeHomeSettings(
      home,
      JSON.stringify({
        hooks: {
          Stop: [{ hooks: [{ type: 'command', command: `${spaced} hook stop` }] }],
          PreCompact: [{ hooks: [{ type: 'command', command: `'${spaced}' hook precompact` }] }],
        },
      })
    );
    mcpConfig(home, spaced);

    assert.deepEqual(readHookBinaries(home), [spaced], 'unquoted and quoted both resolve');
    assert.equal(readMcpBinary(home), spaced, 'the whole command is the path');
  });

  test('a bare `csr-engine` hook command is left to the PATH check', () => {
    const home = tempDir('home-bare');
    writeHomeSettings(home, hookSettings('csr-engine'));
    assert.deepEqual(readHookBinaries(home), []);
  });

  test('malformed settings produce no warning and no crash', () => {
    const home = tempDir('home-broken');
    const dest = fakeBinary(tempDir('dest-broken'), 'csr-engine', '10.1.0');
    writeHomeSettings(home, '{ this is not json');
    writeFileSync(join(home, '.claude.json'), '\u0000not json either');

    assert.deepEqual(readHookBinaries(home), []);
    assert.equal(readMcpBinary(home), null);
    assert.deepEqual(detectStaleBinaries({ destPath: dest, homeDir: home, pathBinary: null }), []);
  });

  test('valid but unreadable settings produce no warning and no crash', (t) => {
    if (IS_ROOT) {
      t.skip('root can read a 0000 file');
      return;
    }
    const home = tempDir('home-unreadable');
    const dest = fakeBinary(tempDir('dest-unreadable'), 'csr-engine', '10.1.0');
    // Valid JSON naming a stale binary — only the permissions hide it, so a
    // regression in the fail-open path would surface as a warning here.
    const path = writeHomeSettings(home, hookSettings('/opt/old/csr-engine'));
    assert.deepEqual(readHookBinaries(home), ['/opt/old/csr-engine'], 'readable: found');

    chmodSync(path, 0o000);
    assert.deepEqual(readHookBinaries(home), [], 'unreadable: fails open');
    assert.deepEqual(detectStaleBinaries({ destPath: dest, homeDir: home, pathBinary: null }), []);
    chmodSync(path, 0o600);
  });

  test('an empty home yields nothing', () => {
    const dest = fakeBinary(tempDir('dest-empty'), 'csr-engine', '10.1.0');
    const empty = tempDir('home-empty');
    assert.deepEqual(detectStaleBinaries({ destPath: dest, homeDir: empty, pathBinary: null }), []);
  });
});

describe('shell-safe hints', () => {
  test('plain paths stay bare, awkward ones get quoted', () => {
    assert.equal(shellQuote('/Users/me/.local/bin/csr-engine'), '/Users/me/.local/bin/csr-engine');
    assert.equal(shellQuote('/tmp/CSR Tools/bin/csr-engine'), "'/tmp/CSR Tools/bin/csr-engine'");
    assert.equal(shellQuote("/tmp/o'brien/csr-engine"), "'/tmp/o'\\''brien/csr-engine'");
    assert.equal(shellQuote('/tmp/a;rm -rf ~/csr-engine'), "'/tmp/a;rm -rf ~/csr-engine'");
  });

  test('the activation hint quotes a destination with a space', () => {
    const dir = tempDir('CSR Tools');
    const dest = fakeBinary(dir, 'csr-engine', '10.1.0');
    const hint = activationCommand({ destPath: dest, pathBinary: null });
    assert.equal(hint, `'${dest}'`);
    assert.match(hint, /^'/);
  });

  test('the activation hint uses the absolute path when bare csr-engine resolves elsewhere', () => {
    const dest = fakeBinary(tempDir('dest-hint'), 'csr-engine', '10.1.0');
    const shadow = fakeBinary(tempDir('shadow-hint'), 'csr-engine', '9.5.7');
    assert.equal(activationCommand({ destPath: dest, pathBinary: shadow }), dest);
    assert.equal(activationCommand({ destPath: dest, pathBinary: null }), dest);
  });

  test('the activation hint stays bare when PATH resolves to the destination', () => {
    const dest = fakeBinary(tempDir('dest-hint2'), 'csr-engine', '10.1.0');
    const link = join(tempDir('link-hint'), 'csr-engine');
    symlinkSync(dest, link);
    assert.equal(activationCommand({ destPath: dest, pathBinary: dest }), 'csr-engine');
    assert.equal(activationCommand({ destPath: dest, pathBinary: link }), 'csr-engine');
  });

  test('the pending-activation notice quotes the destination too', () => {
    const dest = join(tempDir('CSR Pending'), 'csr-engine');
    const text = formatPendingActivation({
      stale: [{ path: '/opt/old/csr-engine', uses: ['MCP'] }],
      destPath: dest,
    });
    assert.match(text, /Not active yet/);
    assert.match(text, re(`'${dest}' setup`));
  });
});

describe('choosing a binary to run', () => {
  test('a non-executable destination does not hide a working PATH fallback', () => {
    const installDir = tempDir('cli-dest');
    const pathDir = tempDir('cli-path');
    const halfWritten = join(installDir, 'csr-engine');
    writeFileSync(halfWritten, 'truncated download');
    chmodSync(halfWritten, 0o644);
    const onPath = fakeBinary(pathDir, 'csr-engine', '9.5.7');

    assert.equal(isRunnableBinary(halfWritten), false);
    assert.equal(findEngineBinary({ installDir, pathBinary: onPath }), onPath);
  });

  test('a directory at the destination is skipped', () => {
    const installDir = tempDir('cli-dir');
    mkdirSync(join(installDir, 'csr-engine'));
    const onPath = fakeBinary(tempDir('cli-path2'), 'csr-engine', '9.5.7');
    assert.equal(isRunnableBinary(join(installDir, 'csr-engine')), false);
    assert.equal(findEngineBinary({ installDir, pathBinary: onPath }), onPath);
  });

  test('an executable destination wins over PATH', () => {
    const installDir = tempDir('cli-good');
    const dest = fakeBinary(installDir, 'csr-engine', '10.1.0');
    const onPath = fakeBinary(tempDir('cli-path3'), 'csr-engine', '9.5.7');
    assert.equal(findEngineBinary({ installDir, pathBinary: onPath }), dest);
  });

  test('a non-executable PATH entry is skipped too', () => {
    const installDir = tempDir('cli-none');
    const notExec = join(tempDir('cli-none-path'), 'csr-engine');
    writeFileSync(notExec, 'not a binary');
    chmodSync(notExec, 0o644);

    const found = findEngineBinary({ installDir, pathBinary: notExec });
    assert.notEqual(found, notExec);
    // Only the /usr/local/bin fallback is left, and whether that exists is a
    // property of the machine, not of this code.
    assert.ok(found === null || found === '/usr/local/bin/csr-engine', String(found));
  });
});

function runPostinstall({ home, installDir, pathPrefix, autoSetup }) {
  const env = {
    ...process.env,
    HOME: home,
    PATH: pathPrefix ? `${pathPrefix}:${process.env.PATH}` : process.env.PATH,
    CSR_INSTALL_DIR: installDir,
    CSR_FORCE_POSTINSTALL: '1',
  };
  delete env.CSR_SKIP_BINARY_DOWNLOAD;
  if (autoSetup) env.CSR_AUTO_SETUP = '1';
  else delete env.CSR_AUTO_SETUP;

  return spawnSync(process.execPath, [POSTINSTALL], { encoding: 'utf8', timeout: 60000, env });
}

describe('postinstall end to end (skip branch, no network)', () => {
  test('already-installed destination skips the download and warns about the shadow', () => {
    const home = tempDir('e2e-home');
    const installDir = tempDir('e2e-install');
    const shadowDir = tempDir('e2e-shadow');

    fakeBinary(installDir, 'csr-engine', PKG_VERSION);
    const shadow = fakeBinary(shadowDir, 'csr-engine', '9.5.7');
    writeHomeSettings(home, hookSettings(shadow));

    const result = runPostinstall({ home, installDir, pathPrefix: shadowDir });

    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /already installed/);
    assert.doesNotMatch(result.stdout, /Downloading/);
    assert.match(result.stdout, re(shadow));
    assert.match(result.stdout, /first on PATH, Claude Code hooks/);
    assert.match(result.stdout, re(`${join(installDir, 'csr-engine')} setup`));
  });

  test('CSR_AUTO_SETUP=1 reports Done when setup really repointed everything', () => {
    const home = tempDir('e2e-auto-ok');
    const installDir = tempDir('e2e-auto-ok-install');
    const dest = join(installDir, 'csr-engine');
    fakeBinary(installDir, 'csr-engine', PKG_VERSION, dest);
    writeHomeSettings(home, hookSettings('/opt/old/csr-engine'));
    mcpConfig(home, '/opt/old/csr-engine');

    const result = runPostinstall({ home, installDir, autoSetup: true });

    assert.equal(result.status, 0, result.stderr);
    assert.doesNotMatch(result.stdout, /Downloading/);
    assert.match(result.stdout, /Done\. Restart Claude Code/);
    assert.doesNotMatch(result.stdout, /Not active yet/);
    assert.equal(readMcpBinary(home), dest, 'the fake setup did repoint it');
  });

  test('CSR_AUTO_SETUP=1 refuses to say Done while a registration is still stale', () => {
    const home = tempDir('e2e-auto-stale');
    const installDir = tempDir('e2e-auto-stale-install');
    fakeBinary(installDir, 'csr-engine', PKG_VERSION, '/opt/old/csr-engine');
    writeHomeSettings(home, hookSettings('/opt/old/csr-engine'));
    mcpConfig(home, '/opt/old/csr-engine');

    const result = runPostinstall({ home, installDir, autoSetup: true });

    assert.equal(result.status, 1, 'a stale registration is a failed activation');
    assert.match(result.stdout, /Not active yet/);
    assert.match(result.stdout, re('/opt/old/csr-engine'));
    assert.doesNotMatch(result.stdout, /Done\. Restart Claude Code/);
  });
});
