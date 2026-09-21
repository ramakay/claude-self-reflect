/**
 * Hermetic tests for scripts/install.sh — `node --test installer/tests/*.test.js`.
 *
 * Two shapes, both offline and both against a throwaway HOME:
 *  - full runs of the script with a stub `curl` first on PATH that serves a
 *    locally built tarball and checksums file, so the real download, checksum,
 *    extract, stage and activation paths execute;
 *  - direct calls into its functions, by sourcing a copy with the trailing
 *    `main` invocation removed.
 *
 * Nothing here touches the network, the real HOME, or a real csr-engine.
 */

import { strict as assert } from 'node:assert';
import { after, before, describe, test } from 'node:test';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  chmodSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = dirname(dirname(dirname(fileURLToPath(import.meta.url))));
const INSTALL_SH = join(REPO_ROOT, 'scripts', 'install.sh');
const SH = '/bin/sh';

const HAVE_SH = (() => {
  try {
    return spawnSync(SH, ['-c', 'exit 7']).status === 7;
  } catch {
    return false;
  }
})();

const scratch = [];
let SOURCEABLE = null;

function tempDir(prefix) {
  const dir = mkdtempSync(join(tmpdir(), `csr-sh-${prefix}-`));
  scratch.push(dir);
  return dir;
}

before(() => {
  if (!HAVE_SH) return;
  // A copy with the trailing `main` call removed, so the functions can be
  // sourced and driven one at a time.
  const lines = readFileSync(INSTALL_SH, 'utf8').split('\n');
  let last = lines.length - 1;
  while (last >= 0 && lines[last].trim() === '') last -= 1;
  assert.equal(lines[last].trim(), 'main', 'install.sh should end by calling main');
  lines.splice(last, 1);
  SOURCEABLE = join(tempDir('lib'), 'install-lib.sh');
  writeFileSync(SOURCEABLE, lines.join('\n'));
});

after(() => {
  for (const dir of scratch) rmSync(dir, { recursive: true, force: true });
});

/** Run a snippet with install.sh's functions in scope. */
function drive(snippet, env) {
  const script = join(tempDir('drive'), 'drive.sh');
  writeFileSync(script, `. ${SOURCEABLE}\n${snippet}\n`);
  return spawnSync(SH, [script], { encoding: 'utf8', timeout: 60000, env });
}

/**
 * Everything a full offline run needs: a tarball holding a stand-in csr-engine,
 * its checksums file, and a `curl` that serves them from disk.
 */
function buildRelease({ setupWrites } = {}) {
  const root = tempDir('release');
  const dirs = {};
  for (const name of ['fixtures', 'fakebin', 'pkg', 'home', 'installdir', 'outside', 'oldbin']) {
    dirs[name] = join(root, name);
    mkdirSync(dirs[name]);
  }
  mkdirSync(join(dirs.home, '.claude'));

  let body = '#!/bin/sh\ncase "$1" in\n  --version) echo "csr-engine 10.1.0" ;;\n';
  if (setupWrites) {
    body +=
      '  setup)\n' +
      '    mkdir -p "$HOME/.claude"\n' +
      `    printf '{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"%s hook stop"}]}]}}' '${setupWrites}' > "$HOME/.claude/settings.json"\n` +
      `    printf '{"mcpServers":{"claude-self-reflect":{"type":"stdio","command":"%s"}}}' '${setupWrites}' > "$HOME/.claude.json"\n` +
      '    echo "=== Setup Complete ===" ;;\n';
  }
  body += '  *) echo "unexpected: $*" >&2; exit 2 ;;\nesac\n';
  writeFileSync(join(dirs.pkg, 'csr-engine'), body);
  chmodSync(join(dirs.pkg, 'csr-engine'), 0o755);

  const machine = spawnSync('uname', ['-m'], { encoding: 'utf8' }).stdout.trim();
  const system = spawnSync('uname', ['-s'], { encoding: 'utf8' }).stdout.trim();
  const arch = ['arm64', 'aarch64'].includes(machine) ? 'aarch64' : 'x86_64';
  const os = system === 'Darwin' ? 'apple-darwin' : 'unknown-linux-gnu';
  const tarball = `csr-engine-${arch}-${os}.tar.gz`;

  const tarred = spawnSync('tar', ['-czf', join(dirs.fixtures, tarball), '-C', dirs.pkg, 'csr-engine']);
  assert.equal(tarred.status, 0, 'tar should build the fixture archive');
  const sum = createHash('sha256').update(readFileSync(join(dirs.fixtures, tarball))).digest('hex');
  writeFileSync(join(dirs.fixtures, 'checksums.txt'), `${sum}  ${tarball}\n`);

  writeFileSync(
    join(dirs.fakebin, 'curl'),
    `#!/bin/sh
url=""; out=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    -*) shift ;;
    *)  url="$1"; shift ;;
  esac
done
case "$url" in
  *api.github.com*) printf '{"tag_name": "v10.1.0"}\\n'; exit 0 ;;
esac
src="${dirs.fixtures}/\${url##*/}"
[ -f "$src" ] || exit 22
if [ -n "$out" ]; then cp "$src" "$out"; else cat "$src"; fi
`
  );
  chmodSync(join(dirs.fakebin, 'curl'), 0o755);

  // A pre-existing older copy, to put first on PATH when a test wants one.
  writeFileSync(
    join(dirs.oldbin, 'csr-engine'),
    '#!/bin/sh\necho "error: unexpected argument \'--version\' found" >&2\nexit 2\n'
  );
  chmodSync(join(dirs.oldbin, 'csr-engine'), 0o755);

  return { root, dirs, tarball };
}

function runInstall({ dirs, installDir, extraEnv = {}, pathPrefix = [] }) {
  const env = {
    ...process.env,
    HOME: dirs.home,
    CSR_INSTALL_DIR: installDir || dirs.installdir,
    PATH: [...pathPrefix, dirs.fakebin, process.env.PATH].join(':'),
    ...extraEnv,
  };
  delete env.CSR_AUTO_SETUP;
  delete env.CSR_SKIP_SETUP;
  for (const [k, v] of Object.entries(extraEnv)) env[k] = v;
  return spawnSync(SH, [INSTALL_SH], { encoding: 'utf8', timeout: 120000, env });
}

function stageLeftovers(dir) {
  return readdirSync(dir).filter((n) => n.startsWith('.csr-engine.'));
}

describe('install.sh: staging the binary', { skip: !HAVE_SH && 'no /bin/sh' }, () => {
  test('a symlinked destination is replaced, and its target is untouched', () => {
    const { dirs } = buildRelease();
    const outside = join(dirs.outside, 'other-install');
    writeFileSync(outside, 'ORIGINAL OTHER INSTALL');
    symlinkSync(outside, join(dirs.installdir, 'csr-engine'));

    const result = runInstall({ dirs, extraEnv: { CSR_SKIP_SETUP: '1' } });

    assert.equal(result.status, 0, result.stderr);
    assert.equal(readFileSync(outside, 'utf8'), 'ORIGINAL OTHER INSTALL');
    const dest = join(dirs.installdir, 'csr-engine');
    assert.equal(lstatSync(dest).isSymbolicLink(), false, 'destination is a regular file now');
    assert.match(readFileSync(dest, 'utf8'), /csr-engine 10\.1\.0/);
    assert.deepEqual(stageLeftovers(dirs.installdir), [], 'no staging file left behind');
  });

  test('a directory destination is refused and leaves no staging file', () => {
    const { dirs } = buildRelease();
    const dest = join(dirs.installdir, 'csr-engine');
    mkdirSync(dest);

    const result = runInstall({ dirs, extraEnv: { CSR_SKIP_SETUP: '1' } });

    assert.equal(result.status, 1, 'a directory destination must not report success');
    assert.match(result.stderr, /is a directory/);
    assert.equal(statSync(dest).isDirectory(), true);
    assert.deepEqual(readdirSync(dest), [], 'nothing was moved inside it');
    assert.deepEqual(stageLeftovers(dirs.installdir), []);
  });

  test('an install directory containing a space produces quoted, pasteable hints', () => {
    const { root, dirs } = buildRelease();
    const spaced = join(root, 'CSR Tools', 'bin');
    mkdirSync(spaced, { recursive: true });

    const result = runInstall({ dirs, installDir: spaced, extraEnv: { CSR_SKIP_SETUP: '1' } });

    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, new RegExp(`'${spaced}/csr-engine' setup`.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
    assert.match(result.stdout, /export PATH='.*CSR Tools\/bin':"\$PATH"/);
  });
});

describe('install.sh: activation', { skip: !HAVE_SH && 'no /bin/sh' }, () => {
  function seedStale(dirs, staleBinary) {
    writeFileSync(
      join(dirs.home, '.claude', 'settings.json'),
      JSON.stringify({
        hooks: { Stop: [{ hooks: [{ type: 'command', command: `${staleBinary} hook stop` }] }] },
      })
    );
    writeFileSync(
      join(dirs.home, '.claude.json'),
      JSON.stringify({ mcpServers: { 'claude-self-reflect': { command: staleBinary } } })
    );
  }

  test('setup that leaves a stale registration reports "Not active yet" and exits 1', () => {
    const stale = '/opt/old/csr-engine';
    const { dirs } = buildRelease({ setupWrites: stale });
    seedStale(dirs, stale);

    const result = runInstall({ dirs, extraEnv: { CSR_AUTO_SETUP: '1' } });

    assert.equal(result.status, 1);
    assert.match(result.stdout, /Not active yet/);
    assert.match(result.stdout, /\/opt\/old\/csr-engine/);
    assert.doesNotMatch(result.stdout, /Done\. Restart Claude Code/);
  });

  test('setup that really repoints everything reports "Done" and exits 0', () => {
    const { dirs } = buildRelease({ setupWrites: '__DEST__' });
    // Rebuild the stand-in now that the destination path is known.
    const dest = join(dirs.installdir, 'csr-engine');
    const rebuilt = buildRelease({ setupWrites: dest });
    seedStale(rebuilt.dirs, '/opt/old/csr-engine');

    const result = runInstall({
      dirs: rebuilt.dirs,
      installDir: dirs.installdir,
      extraEnv: { CSR_AUTO_SETUP: '1' },
    });

    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /Done\. Restart Claude Code/);
    assert.doesNotMatch(result.stdout, /Not active yet/);
  });
});

describe('install.sh: config readers', { skip: !HAVE_SH && 'no /bin/sh' }, () => {
  const DECOY_HOOK = '/opt/old/csr-engine';
  const QUOTED_PATH = "/tmp/o'brien/CSR Tools/csr-engine";
  const QUOTED_COMMAND = "'/tmp/o'\\''brien/CSR Tools/csr-engine' hook precompact";

  function decoyHome() {
    const home = tempDir('decoy-home');
    mkdirSync(join(home, '.claude'));
    writeFileSync(
      join(home, '.claude', 'settings.json'),
      JSON.stringify({
        hooks: {
          Stop: [{ hooks: [{ type: 'command', command: `${DECOY_HOOK} hook stop` }] }],
          PreCompact: [{ hooks: [{ type: 'command', command: QUOTED_COMMAND }] }],
          SessionStart: [{ hooks: [{ type: 'command', command: 'csr-engine hook session-start' }] }],
          Other: [{ hooks: [{ type: 'command', command: '/usr/bin/env node /some/other.js' }] }],
        },
      })
    );
    writeFileSync(
      join(home, '.claude.json'),
      JSON.stringify({
        mcpServers: {
          'claude-self-reflect': { type: 'stdio', command: '/opt/mcp/csr-engine' },
          other: { command: '/x/y' },
        },
        // A project key and a history entry that both end in /csr-engine: neither
        // is a registration, and a grep-based scan reported them as one.
        projects: {
          '/Users/me/projects/foo/csr-engine': {
            mcpServers: { 'claude-self-reflect': { command: '/decoy/csr-engine' } },
          },
        },
        history: [{ display: 'cd /Users/me/projects/foo/csr-engine' }],
      })
    );
    return home;
  }

  const SNIPPET =
    'JSON_READER="$FORCE_READER"\n' +
    'echo "HOOKS:"\n' +
    'registered_executables "$HOME/.claude/settings.json" hooks\n' +
    'echo "MCP:"\n' +
    'registered_executables "$HOME/.claude.json" mcp\n';

  for (const reader of ['python3', 'node']) {
    test(`${reader} decodes quoted paths and ignores project keys and history`, (t) => {
      if (spawnSync(reader, ['--version']).status !== 0) {
        t.skip(`${reader} unavailable`);
        return;
      }
      const home = decoyHome();
      const result = drive(SNIPPET, {
        ...process.env,
        HOME: home,
        CSR_INSTALL_DIR: join(home, 'bin'),
        FORCE_READER: reader,
      });

      assert.equal(result.status, 0, result.stderr);
      const [, hooks, mcp] = result.stdout.split(/^(?:HOOKS|MCP):$/m);
      const hookLines = hooks.split('\n').filter(Boolean);
      const mcpLines = mcp.split('\n').filter(Boolean);

      assert.deepEqual(hookLines.sort(), [DECOY_HOOK, QUOTED_PATH].sort());
      assert.deepEqual(mcpLines, ['/opt/mcp/csr-engine']);
      assert.ok(!result.stdout.includes('/decoy/csr-engine'), 'project scope is not ours');
      assert.ok(!result.stdout.includes('/Users/me/projects/foo'), 'a project key is not a binary');
    });
  }

  test('abstains entirely when neither python3 nor node is available', () => {
    const home = decoyHome();
    // A PATH with no interpreters at all. resolve_path degrades to a string
    // compare, which is exactly the documented fallback.
    const bareBin = tempDir('bare-bin');
    const result = drive('pick_json_reader\necho "reader=[$JSON_READER]"\n' + SNIPPET, {
      HOME: home,
      CSR_INSTALL_DIR: join(home, 'bin'),
      FORCE_READER: '',
      PATH: bareBin,
    });

    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /reader=\[\]/);
    assert.equal(result.stdout.includes('/opt/old/csr-engine'), false, 'no guesses without a parser');
    assert.equal(result.stdout.includes('/opt/mcp/csr-engine'), false);
  });

  test('a malformed or missing config yields nothing and exits 0', () => {
    const home = tempDir('broken-home');
    mkdirSync(join(home, '.claude'));
    writeFileSync(join(home, '.claude', 'settings.json'), '{ not json "command": "/opt/old/csr-engine"');

    const result = drive(SNIPPET, {
      ...process.env,
      HOME: home,
      CSR_INSTALL_DIR: join(home, 'bin'),
      FORCE_READER: '',
    });

    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout.includes('/opt/old'), false);
  });
});

describe('install.sh: install directory', { skip: !HAVE_SH && 'no /bin/sh' }, () => {
  test('refuses to guess a destination when HOME is unset', () => {
    const env = { PATH: process.env.PATH };
    const result = spawnSync(SH, ['-c', `. ${SOURCEABLE}; echo "reached=$INSTALL_DIR"`], {
      encoding: 'utf8',
      env,
    });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /HOME is not set/);
    assert.equal(result.stdout.includes('reached='), false);
  });

  test('makes a relative CSR_INSTALL_DIR absolute', () => {
    const base = tempDir('relative');
    const result = spawnSync(SH, ['-c', `. ${SOURCEABLE}; printf '%s\\n' "$INSTALL_DIR"`], {
      encoding: 'utf8',
      cwd: base,
      env: { ...process.env, HOME: base, CSR_INSTALL_DIR: 'rel/bin' },
    });
    assert.equal(result.status, 0, result.stderr);
    const resolved = result.stdout.trim();
    // $PWD may itself be a resolved form of the temp dir (/private/var on
    // macOS), so assert the property that matters rather than the exact prefix.
    assert.match(resolved, /^\//, 'must be absolute');
    assert.ok(resolved.endsWith('/rel/bin'), resolved);
  });
});
