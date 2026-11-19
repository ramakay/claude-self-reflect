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
 * Nothing here touches the network, the real HOME, a real csr-engine, or the
 * real launchctl (a stub in fakebin answers, so no test restarts the daemon).
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
  assert.equal(lines[last].trim(), 'main "$@"', 'install.sh should end by calling main "$@"');
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
function buildRelease({ setupWrites, setupRecordsEnv } = {}) {
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
  if (setupRecordsEnv) {
    // Records whether CSR_DREAMS_CONSENT reached setup; `-` distinguishes
    // unset from empty.
    body +=
      '  setup)\n' +
      '    printf "consent=%s\\n" "${CSR_DREAMS_CONSENT-<unset>}" > "$HOME/setup-env.txt"\n' +
      // The installer's own variables, which setup would copy into the launch agent.
      '    env | grep -E "^CSR_(VERSION|TARBALL|RELEASE_BASE_URL|CHECKSUMS|TTY)=" > "$HOME/setup-installer-env.txt"\n' +
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

  // Every URL the stub is asked for goes to this log, so a test can prove
  // which requests were (and were not) made.
  const curlLog = join(root, 'curl.calls');
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
printf '%s\\n' "$url" >> '${curlLog}'
case "$url" in
  *api.github.com*) printf '{"tag_name": "v10.1.0"}\\n'; exit 0 ;;
esac
src="${dirs.fixtures}/\${url##*/}"
[ -f "$src" ] || exit 22
if [ -n "$out" ]; then cp "$src" "$out"; else cat "$src"; fi
`
  );
  chmodSync(join(dirs.fakebin, 'curl'), 0o755);

  // install.sh ends with `launchctl kickstart -k` on the user's daemon. The
  // real one would restart the live csr-engine daemon of whoever runs these
  // tests, so a stub first on PATH answers instead: `print` says the label is
  // loaded (so the kickstart path runs), and every call is written to a log.
  const launchctlLog = join(root, 'launchctl.calls');
  writeFileSync(
    join(dirs.fakebin, 'launchctl'),
    `#!/bin/sh\nprintf '%s\\n' "$*" >> '${launchctlLog}'\nexit 0\n`
  );
  chmodSync(join(dirs.fakebin, 'launchctl'), 0o755);

  // A pre-existing older copy, to put first on PATH when a test wants one.
  writeFileSync(
    join(dirs.oldbin, 'csr-engine'),
    '#!/bin/sh\necho "error: unexpected argument \'--version\' found" >&2\nexit 2\n'
  );
  chmodSync(join(dirs.oldbin, 'csr-engine'), 0o755);

  return { root, dirs, tarball, launchctlLog, curlLog };
}

function curlCalls(curlLog) {
  try {
    return readFileSync(curlLog, 'utf8').split('\n').filter(Boolean);
  } catch {
    return [];
  }
}

function launchctlCalls(launchctlLog) {
  try {
    return readFileSync(launchctlLog, 'utf8').split('\n').filter(Boolean);
  } catch {
    return [];
  }
}

function runInstall({ dirs, installDir, extraEnv = {}, pathPrefix = [], args = [] }) {
  const env = {
    ...process.env,
    HOME: dirs.home,
    CSR_INSTALL_DIR: installDir || dirs.installdir,
    PATH: [...pathPrefix, dirs.fakebin, process.env.PATH].join(':'),
    ...extraEnv,
  };
  // Nothing from the developer's own shell may steer a run.
  for (const name of [
    'CSR_AUTO_SETUP',
    'CSR_SKIP_SETUP',
    'CSR_VERSION',
    'CSR_TARBALL',
    'CSR_RELEASE_BASE_URL',
    'CSR_CHECKSUMS',
    'CSR_DREAMS_CONSENT',
    'CSR_TTY',
  ]) {
    delete env[name];
  }
  for (const [k, v] of Object.entries(extraEnv)) env[k] = v;
  return spawnSync(SH, [INSTALL_SH, ...args], { encoding: 'utf8', timeout: 120000, env });
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

  test('a destination symlinked to an outside directory is refused', () => {
    const { dirs } = buildRelease();
    // `mv file symlink-to-directory` follows the link and drops the file inside
    // that directory, which is how the binary would land outside INSTALL_DIR.
    const outsideDir = join(dirs.outside, 'somewhere-else');
    mkdirSync(outsideDir);
    symlinkSync(outsideDir, join(dirs.installdir, 'csr-engine'));

    const result = runInstall({ dirs, extraEnv: { CSR_SKIP_SETUP: '1' } });

    assert.equal(result.status, 1);
    assert.match(result.stderr, /is a directory \(or a link to one\)/);
    assert.deepEqual(readdirSync(outsideDir), [], 'nothing was written outside INSTALL_DIR');
    assert.equal(
      lstatSync(join(dirs.installdir, 'csr-engine')).isSymbolicLink(),
      true,
      'the link is left alone, not replaced'
    );
    assert.deepEqual(stageLeftovers(dirs.installdir), [], 'no staging directory left');
  });

  test('a failed staging write exits 1, spares the old binary and cleans up', () => {
    const { dirs } = buildRelease();
    const dest = join(dirs.installdir, 'csr-engine');
    writeFileSync(dest, 'EXISTING BINARY');
    chmodSync(dest, 0o755);

    // A `cat` that fails mid-install, so `set -e` aborts after the staging
    // directory exists and the EXIT trap is the only thing that removes it.
    const sabotage = tempDir('sabotage');
    writeFileSync(join(sabotage, 'cat'), '#!/bin/sh\necho "cat refused" >&2\nexit 1\n');
    chmodSync(join(sabotage, 'cat'), 0o755);

    const result = runInstall({
      dirs,
      extraEnv: { CSR_SKIP_SETUP: '1' },
      pathPrefix: [sabotage],
    });

    assert.equal(result.status, 1, `a failed staging write must exit 1: ${result.stderr}`);
    assert.equal(readFileSync(dest, 'utf8'), 'EXISTING BINARY', 'old binary untouched');
    assert.deepEqual(stageLeftovers(dirs.installdir), [], 'EXIT trap removed the staging dir');
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
    // The daemon restart went to the stub, never to the real launchctl.
    const calls = launchctlCalls(rebuilt.launchctlLog);
    assert.ok(
      calls.some((c) => /^kickstart -k gui\/\d+\/com\.ramakay\.csr-engine\.daemon$/.test(c)),
      `kickstart was requested through the stub: ${JSON.stringify(calls)}`
    );
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
          // The shell runs /opt/suffixed-csr-enginejunk here, so decoding the
          // quoted part as ours would invent a registration nobody has.
          Suffixed: [
            { hooks: [{ type: 'command', command: "'/opt/suffixed/csr-engine'junk hook stop" }] },
          ],
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
      assert.ok(!result.stdout.includes('/opt/suffixed'), 'a quoted word plus junk is not ours');
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

  test('the PATH hint names no rc file when HOME is unset', () => {
    // CSR_INSTALL_DIR gets past the HOME guard; the hint must then not point
    // at "/.zshrc", which is what an empty $HOME used to produce.
    const result = drive('check_path', {
      PATH: '/usr/bin:/bin',
      CSR_INSTALL_DIR: '/opt/csr/bin',
      SHELL: '/bin/zsh',
    });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /Add this to your shell configuration:/);
    assert.match(result.stdout, /export PATH=\/opt\/csr\/bin:"\$PATH"/);
    assert.equal(result.stdout.includes('/.zshrc'), false, result.stdout);
  });

  test('the PATH hint names the rc file when HOME is set', () => {
    const home = tempDir('path-hint');
    const result = drive('check_path', {
      PATH: '/usr/bin:/bin',
      HOME: home,
      CSR_INSTALL_DIR: '/opt/csr/bin',
      SHELL: '/bin/zsh',
    });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, new RegExp(`Add this to ${home.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&')}/\\.zshrc:`));
    assert.match(result.stdout, /export PATH=\/opt\/csr\/bin:"\$PATH"/);
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

describe('install.sh: pinned version', { skip: !HAVE_SH && 'no /bin/sh' }, () => {
  function plainEnv() {
    const home = tempDir('pin-home');
    return { ...process.env, HOME: home, CSR_INSTALL_DIR: join(home, 'bin') };
  }

  for (const [given, tag] of [
    ['10.1.0', 'v10.1.0'],
    ['v10.1.0', 'v10.1.0'],
    ['V10.1.0', 'v10.1.0'],
    ['10.2.0-rc.1', 'v10.2.0-rc.1'],
  ]) {
    test(`normalises ${given} to ${tag}`, () => {
      const result = drive(`normalize_version '${given}'\nprintf '%s\\n' "$NORMALIZED_VERSION"`, plainEnv());
      assert.equal(result.status, 0, result.stderr);
      assert.equal(result.stdout.trim(), tag);
    });
  }

  for (const bad of ['latest', '../x', 'v10.1.0/../../y', '10.1.0 --foo', 'v', '$(id)']) {
    test(`refuses the version ${JSON.stringify(bad)}`, () => {
      const result = drive(`normalize_version '${bad}'\necho reached`, plainEnv());
      assert.equal(result.status, 1);
      assert.match(result.stderr, /Invalid version/);
      assert.equal(result.stdout.includes('reached'), false);
    });
  }

  test('parses --version <tag> and --version=<tag>', () => {
    const spaced = drive('parse_args --version 10.1.0\nprintf "[%s]\\n" "$PIN_VERSION"', plainEnv());
    assert.equal(spaced.status, 0, spaced.stderr);
    assert.equal(spaced.stdout.trim(), '[10.1.0]');

    const equals = drive('parse_args --version=v10.1.0\nprintf "[%s]\\n" "$PIN_VERSION"', plainEnv());
    assert.equal(equals.status, 0, equals.stderr);
    assert.equal(equals.stdout.trim(), '[v10.1.0]');

    const none = drive('parse_args\nprintf "[%s]\\n" "$PIN_VERSION"', plainEnv());
    assert.equal(none.stdout.trim(), '[]');
  });

  test('a missing --version value and an unknown argument both fail', () => {
    for (const args of ['--version', '--version=', '--frobnicate']) {
      const result = drive(`parse_args ${args}\necho reached`, plainEnv());
      assert.equal(result.status, 1, args);
      assert.equal(result.stdout.includes('reached'), false, args);
    }
  });

  test('without a pin it asks the GitHub API for the latest release', () => {
    const { dirs, tarball, curlLog } = buildRelease();
    const result = runInstall({ dirs, extraEnv: { CSR_SKIP_SETUP: '1' } });

    assert.equal(result.status, 0, result.stderr);
    const calls = curlCalls(curlLog);
    assert.ok(calls.some((c) => c.includes('api.github.com/repos/ramakay/claude-self-reflect/releases/latest')), JSON.stringify(calls));
    assert.ok(calls.includes(`https://github.com/ramakay/claude-self-reflect/releases/download/v10.1.0/${tarball}`), JSON.stringify(calls));
  });

  test('CSR_VERSION skips the GitHub API and downloads that tag', () => {
    const { dirs, tarball, curlLog } = buildRelease();
    const result = runInstall({ dirs, extraEnv: { CSR_SKIP_SETUP: '1', CSR_VERSION: '10.0.9' } });

    assert.equal(result.status, 0, result.stderr);
    const calls = curlCalls(curlLog);
    assert.equal(calls.some((c) => c.includes('api.github.com')), false, JSON.stringify(calls));
    assert.ok(calls.includes(`https://github.com/ramakay/claude-self-reflect/releases/download/v10.0.9/${tarball}`), JSON.stringify(calls));
    assert.ok(calls.includes('https://github.com/ramakay/claude-self-reflect/releases/download/v10.0.9/checksums.txt'));
    assert.match(result.stdout, /Installing.* csr-engine v10\.0\.9 \(.*\) from github\.com\/ramakay\/claude-self-reflect releases/);
  });

  test('--version works through sh -s style arguments, and beats CSR_VERSION', () => {
    const { dirs, tarball, curlLog } = buildRelease();
    const result = runInstall({
      dirs,
      args: ['--version', 'v10.0.8'],
      extraEnv: { CSR_SKIP_SETUP: '1', CSR_VERSION: '10.0.1' },
    });

    assert.equal(result.status, 0, result.stderr);
    const calls = curlCalls(curlLog);
    assert.equal(calls.some((c) => c.includes('api.github.com')), false);
    assert.ok(calls.includes(`https://github.com/ramakay/claude-self-reflect/releases/download/v10.0.8/${tarball}`), JSON.stringify(calls));
    assert.equal(calls.some((c) => c.includes('v10.0.1')), false);
  });

  test('an invalid CSR_VERSION stops before any network call or install', () => {
    const { dirs, curlLog } = buildRelease();
    const result = runInstall({ dirs, extraEnv: { CSR_SKIP_SETUP: '1', CSR_VERSION: '../evil' } });

    assert.equal(result.status, 1);
    assert.match(result.stderr, /Invalid version/);
    assert.deepEqual(curlCalls(curlLog), []);
    assert.deepEqual(readdirSync(dirs.installdir), []);
  });
});

describe('install.sh: local tarball and release base', { skip: !HAVE_SH && 'no /bin/sh' }, () => {
  const SKIP = { CSR_SKIP_SETUP: '1' };

  /** A copy of the fixtures, so a test can break them without touching the shared ones. */
  function copyFixtures(rel, { checksums } = {}) {
    const dir = tempDir('local-src');
    writeFileSync(join(dir, rel.tarball), readFileSync(join(rel.dirs.fixtures, rel.tarball)));
    if (checksums !== null) {
      writeFileSync(
        join(dir, 'checksums.txt'),
        checksums ?? readFileSync(join(rel.dirs.fixtures, 'checksums.txt'), 'utf8')
      );
    }
    return dir;
  }

  test('installs from CSR_TARBALL with checksums.txt beside it, with no network call', () => {
    const rel = buildRelease();
    const src = copyFixtures(rel);
    const tarball = join(src, rel.tarball);

    const result = runInstall({ dirs: rel.dirs, extraEnv: { ...SKIP, CSR_TARBALL: tarball } });

    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(curlCalls(rel.curlLog), [], 'curl was never run');
    assert.match(result.stdout, /Checksum.* verified/);
    assert.match(result.stdout, new RegExp(`Installing.* csr-engine \\(unpinned\\) \\(.*\\) from local tarball ${src.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`));
    const dest = join(rel.dirs.installdir, 'csr-engine');
    assert.match(readFileSync(dest, 'utf8'), /csr-engine 10\.1\.0/);
    assert.equal(statSync(dest).mode & 0o111 ? true : false, true, 'installed binary is executable');
    assert.deepEqual(stageLeftovers(rel.dirs.installdir), []);
  });

  test('CSR_TARBALL with a pinned CSR_VERSION shows that version and still makes no call', () => {
    const rel = buildRelease();
    const src = copyFixtures(rel);

    const result = runInstall({
      dirs: rel.dirs,
      extraEnv: { ...SKIP, CSR_TARBALL: join(src, rel.tarball), CSR_VERSION: '10.1.0' },
    });

    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /Installing.* csr-engine v10\.1\.0 /);
    assert.deepEqual(curlCalls(rel.curlLog), []);
  });

  test('a wrong checksum is refused and nothing is installed', () => {
    const rel = buildRelease();
    const src = copyFixtures(rel, { checksums: `${'0'.repeat(64)}  ${rel.tarball}\n` });

    const result = runInstall({ dirs: rel.dirs, extraEnv: { ...SKIP, CSR_TARBALL: join(src, rel.tarball) } });

    assert.equal(result.status, 1);
    assert.match(result.stderr, /Checksum mismatch/);
    assert.deepEqual(readdirSync(rel.dirs.installdir), [], 'no binary installed');
  });

  test('a missing checksums file is refused with a message naming CSR_CHECKSUMS', () => {
    const rel = buildRelease();
    const src = copyFixtures(rel, { checksums: null });

    const result = runInstall({ dirs: rel.dirs, extraEnv: { ...SKIP, CSR_TARBALL: join(src, rel.tarball) } });

    assert.equal(result.status, 1);
    assert.match(result.stderr, /No checksums file found/);
    assert.match(result.stderr, /CSR_CHECKSUMS/);
    assert.match(result.stderr, /unverified/);
    assert.deepEqual(readdirSync(rel.dirs.installdir), [], 'no binary installed');
  });

  test('a checksums file with no entry for the tarball is refused', () => {
    const rel = buildRelease();
    const src = copyFixtures(rel, { checksums: `${'a'.repeat(64)}  some-other-file.tar.gz\n` });

    const result = runInstall({ dirs: rel.dirs, extraEnv: { ...SKIP, CSR_TARBALL: join(src, rel.tarball) } });

    assert.equal(result.status, 1);
    assert.match(result.stderr, /Checksum entry not found/);
    assert.deepEqual(readdirSync(rel.dirs.installdir), []);
  });

  test('CSR_CHECKSUMS names a checksums file kept elsewhere', () => {
    const rel = buildRelease();
    const src = copyFixtures(rel, { checksums: null });
    const elsewhere = join(tempDir('sums'), 'SHA256SUMS');
    writeFileSync(elsewhere, readFileSync(join(rel.dirs.fixtures, 'checksums.txt')));

    const result = runInstall({
      dirs: rel.dirs,
      extraEnv: { ...SKIP, CSR_TARBALL: join(src, rel.tarball), CSR_CHECKSUMS: elsewhere },
    });

    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /Checksum.* verified/);
  });

  test('CSR_CHECKSUMS that does not exist is refused', () => {
    const rel = buildRelease();
    const src = copyFixtures(rel);

    const result = runInstall({
      dirs: rel.dirs,
      extraEnv: { ...SKIP, CSR_TARBALL: join(src, rel.tarball), CSR_CHECKSUMS: join(src, 'nope.txt') },
    });

    assert.equal(result.status, 1);
    assert.match(result.stderr, /No checksums file found/);
    assert.deepEqual(readdirSync(rel.dirs.installdir), []);
  });

  test('a CSR_TARBALL that does not exist is refused', () => {
    const rel = buildRelease();
    const result = runInstall({
      dirs: rel.dirs,
      extraEnv: { ...SKIP, CSR_TARBALL: join(rel.root, 'missing.tar.gz') },
    });

    assert.equal(result.status, 1);
    assert.match(result.stderr, /Could not get/);
    assert.deepEqual(readdirSync(rel.dirs.installdir), []);
  });

  for (const [label, wrap] of [
    ['a file:// URL', (dir) => `file://${dir}`],
    ['a plain absolute directory', (dir) => dir],
    ['a directory with a trailing slash', (dir) => `${dir}/`],
  ]) {
    test(`CSR_RELEASE_BASE_URL as ${label} installs with no network call`, () => {
      const rel = buildRelease();
      const src = copyFixtures(rel);

      const result = runInstall({ dirs: rel.dirs, extraEnv: { ...SKIP, CSR_RELEASE_BASE_URL: wrap(src) } });

      assert.equal(result.status, 0, result.stderr);
      assert.deepEqual(curlCalls(rel.curlLog), [], 'no GitHub API call, no download');
      assert.match(result.stdout, /Checksum.* verified/);
      assert.match(result.stdout, /Installing.* csr-engine \(unpinned\)/);
      assert.match(readFileSync(join(rel.dirs.installdir, 'csr-engine'), 'utf8'), /csr-engine 10\.1\.0/);
    });
  }

  test('CSR_RELEASE_BASE_URL with a wrong checksum is refused', () => {
    const rel = buildRelease();
    const src = copyFixtures(rel, { checksums: `${'1'.repeat(64)}  ${rel.tarball}\n` });

    const result = runInstall({ dirs: rel.dirs, extraEnv: { ...SKIP, CSR_RELEASE_BASE_URL: src } });

    assert.equal(result.status, 1);
    assert.match(result.stderr, /Checksum mismatch/);
    assert.deepEqual(readdirSync(rel.dirs.installdir), []);
  });

  test('CSR_RELEASE_BASE_URL with no checksums file is refused', () => {
    const rel = buildRelease();
    const src = copyFixtures(rel, { checksums: null });

    const result = runInstall({ dirs: rel.dirs, extraEnv: { ...SKIP, CSR_RELEASE_BASE_URL: `file://${src}` } });

    assert.equal(result.status, 1);
    assert.match(result.stderr, /No checksums file found/);
    assert.deepEqual(readdirSync(rel.dirs.installdir), []);
  });

  test('an https CSR_RELEASE_BASE_URL is used as given, without a version directory or the API', () => {
    const rel = buildRelease();

    const result = runInstall({
      dirs: rel.dirs,
      extraEnv: { ...SKIP, CSR_RELEASE_BASE_URL: 'https://mirror.example/csr/' },
    });

    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(curlCalls(rel.curlLog), [
      `https://mirror.example/csr/${rel.tarball}`,
      'https://mirror.example/csr/checksums.txt',
    ]);
  });

  test('a plain http base and a relative path are refused', () => {
    for (const base of ['http://mirror.example/csr', 'relative/dir']) {
      const rel = buildRelease();
      const result = runInstall({ dirs: rel.dirs, extraEnv: { ...SKIP, CSR_RELEASE_BASE_URL: base } });

      assert.equal(result.status, 1, base);
      assert.match(result.stderr, /Unsupported location/, base);
      assert.deepEqual(readdirSync(rel.dirs.installdir), [], base);
    }
  });

  test('CSR_TARBALL together with CSR_RELEASE_BASE_URL is refused', () => {
    const rel = buildRelease();
    const src = copyFixtures(rel);

    const result = runInstall({
      dirs: rel.dirs,
      extraEnv: { ...SKIP, CSR_TARBALL: join(src, rel.tarball), CSR_RELEASE_BASE_URL: src },
    });

    assert.equal(result.status, 1);
    assert.match(result.stderr, /both set/);
    assert.deepEqual(readdirSync(rel.dirs.installdir), []);
  });
});

describe('install.sh: consent pass-through to setup', { skip: !HAVE_SH && 'no /bin/sh' }, () => {
  /** A regular file stands in for the terminal: the answer waiting in it is what `read` gets. */
  function answerFile(answer) {
    const file = join(tempDir('tty'), 'tty');
    writeFileSync(file, answer);
    return file;
  }

  function recorded(dirs) {
    try {
      return readFileSync(join(dirs.home, 'setup-env.txt'), 'utf8').trim();
    } catch {
      return null;
    }
  }

  for (const [label, answer] of [
    ['y', 'y\n'],
    ['an empty answer (the yes default)', '\n'],
  ]) {
    test(`answering ${label} at the prompt sets CSR_DREAMS_CONSENT=yes for setup`, () => {
      const { dirs } = buildRelease({ setupRecordsEnv: true });

      const tty = answerFile(answer);
      const result = runInstall({ dirs, extraEnv: { CSR_TTY: tty } });

      assert.equal(result.status, 0, result.stderr + result.stdout);
      // The prompt went to the stand-in terminal, so the prompt branch ran.
      assert.match(readFileSync(tty, 'utf8'), /Run setup now\? \[Y\/n\]/);
      assert.equal(recorded(dirs), 'consent=yes');
      // CSR_TTY was set for the installer; setup (and so the launch agent) never sees it.
      assert.equal(readFileSync(join(dirs.home, 'setup-installer-env.txt'), 'utf8'), '');
    });
  }

  test('answering n runs no setup at all', () => {
    const { dirs } = buildRelease({ setupRecordsEnv: true });

    const result = runInstall({ dirs, extraEnv: { CSR_TTY: answerFile('n\n') } });

    assert.equal(result.status, 0, result.stderr);
    assert.equal(recorded(dirs), null, 'setup must not have run');
    assert.match(result.stdout, /Not yet active/);
  });

  test('a terminal that gives no answer (EOF) runs no setup', () => {
    const { dirs } = buildRelease({ setupRecordsEnv: true });

    // /dev/null accepts the prompt and reads as end-of-file at once.
    const result = runInstall({ dirs, extraEnv: { CSR_TTY: '/dev/null' } });

    assert.equal(result.status, 0, result.stderr);
    assert.equal(recorded(dirs), null, 'EOF is not consent');
    assert.match(result.stdout, /Not yet active/);
  });

  test('CSR_AUTO_SETUP=1 runs setup without setting CSR_DREAMS_CONSENT', () => {
    const { dirs } = buildRelease({ setupRecordsEnv: true });

    const result = runInstall({ dirs, extraEnv: { CSR_AUTO_SETUP: '1', CSR_TTY: answerFile('y\n') } });

    assert.equal(result.status, 0, result.stderr);
    assert.doesNotMatch(result.stdout, /Run setup now/, 'no prompt under CSR_AUTO_SETUP');
    assert.equal(recorded(dirs), 'consent=<unset>');
  });

  test('the prompt text says that yes also turns on dreams', () => {
    const { dirs } = buildRelease({ setupRecordsEnv: true });
    const tty = answerFile('n\n');

    const result = runInstall({ dirs, extraEnv: { CSR_TTY: tty } });

    assert.equal(result.status, 0, result.stderr);
    const prompt = readFileSync(tty, 'utf8');
    assert.match(prompt, /Answering yes also turns on dreams/);
    assert.match(prompt, /Run setup now\? \[Y\/n\]/);
  });
});
