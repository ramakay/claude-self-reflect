#!/usr/bin/env node

/**
 * Post-install hook for npm.
 * Downloads the csr-engine binary from GitHub Releases and verifies its
 * checksum. By default nothing else: activation (hook registration, MCP
 * registration, conversation import) happens when the user explicitly runs
 * `csr-engine setup` — postinstall does not touch ~/.claude or index data
 * unless the user opts in with CSR_AUTO_SETUP=1.
 * Detects existing Python CSR installations and guides upgrade.
 *
 * Environment variables:
 *   CSR_SKIP_BINARY_DOWNLOAD=1  — Skip binary download (CI, offline, custom builds)
 *   CSR_AUTO_SETUP=1            — Opt in to running `csr-engine setup` after download
 */

import {
  existsSync,
  readFileSync,
  mkdirSync,
  createWriteStream,
  unlinkSync,
  mkdtempSync,
  rmSync,
  lstatSync,
} from 'fs';
import { dirname, join, resolve } from 'path';
import { homedir, platform, arch, tmpdir } from 'os';
import { execFileSync } from 'child_process';
import { createHash } from 'crypto';
import { get as httpsGet } from 'https';
import { fileURLToPath } from 'url';
import {
  BINARY_NAME,
  activationCommand,
  detectStaleBinaries,
  formatPendingActivation,
  formatStaleWarning,
  installBinary,
  planInstall,
  readJsonConfig,
  registrationStale,
  whichBinary,
} from './lib.js';

const REPO = 'ramakay/claude-self-reflect';
// Absolute: every hint we print has to be runnable from anywhere, and a
// relative CSR_INSTALL_DIR would otherwise produce advice that depends on the
// caller's working directory.
const INSTALL_DIR = resolve(process.env.CSR_INSTALL_DIR || join(homedir(), '.local', 'bin'));
const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const PACKAGE_ROOT = resolve(SCRIPT_DIR, '..');
const MAX_REDIRECTS = 5;

function isSourceCheckout() {
  return existsSync(join(PACKAGE_ROOT, 'csr-engine', 'Cargo.toml')) &&
    existsSync(join(PACKAGE_ROOT, '.git'));
}

// Skip during development
if (isSourceCheckout() && !process.env.CSR_FORCE_POSTINSTALL) {
  process.exit(0);
}

// Skip if explicitly disabled
if (process.env.CSR_SKIP_BINARY_DOWNLOAD === '1') {
  console.log('\n  CSR_SKIP_BINARY_DOWNLOAD=1 — skipping binary download.');
  console.log('  Install manually: curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh\n');
  process.exit(0);
}

// --- Platform detection ---

function detectTarget() {
  const os = platform();
  const cpu = arch();

  let osName;
  switch (os) {
    case 'darwin': osName = 'apple-darwin'; break;
    case 'linux': osName = 'unknown-linux-gnu'; break;
    default:
      console.error(`\n  Unsupported platform: ${os}. Only macOS and Linux are supported.`);
      console.error('  Install manually: curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh\n');
      process.exit(0); // Don't fail npm install
  }

  let archName;
  switch (cpu) {
    case 'arm64': archName = 'aarch64'; break;
    case 'x64': archName = 'x86_64'; break;
    default:
      console.error(`\n  Unsupported architecture: ${cpu}.`);
      process.exit(0);
  }

  // Intel Mac: no prebuilt binaries
  if (archName === 'x86_64' && osName === 'apple-darwin') {
    console.log('\n  Intel Mac (x86_64) — no prebuilt binaries (ONNX limitation).');
    console.log('  Build from source:');
    console.log('    cd csr-engine && cargo build --release');
    console.log('    cp target/release/csr-engine ~/.local/bin/\n');
    process.exit(0);
  }

  return `${archName}-${osName}`;
}

// --- HTTP helpers ---

function httpsUrl(url) {
  const parsed = new URL(url);
  if (parsed.protocol !== 'https:') {
    throw new Error(`Refusing non-HTTPS URL: ${url}`);
  }
  return parsed;
}

function redirectUrl(currentUrl, location) {
  const parsed = new URL(location, currentUrl);
  if (parsed.protocol !== 'https:') {
    throw new Error(`Refusing non-HTTPS redirect: ${parsed.href}`);
  }
  return parsed.href;
}

function downloadText(url, redirects = 0) {
  return new Promise((resolve, reject) => {
    const parsedUrl = httpsUrl(url);
    httpsGet(parsedUrl, { headers: { 'User-Agent': 'claude-self-reflect-npm' } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        if (redirects >= MAX_REDIRECTS) {
          return reject(new Error(`Too many redirects for ${url}`));
        }
        return downloadText(redirectUrl(parsedUrl, res.headers.location), redirects + 1)
          .then(resolve, reject);
      }
      if (res.statusCode !== 200) {
        res.resume();
        return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
      }
      let data = '';
      res.setEncoding('utf8');
      res.on('data', (chunk) => { data += chunk; });
      res.on('end', () => resolve(data));
      res.on('error', reject);
    }).on('error', reject);
  });
}

function downloadFile(url, dest, redirects = 0) {
  return new Promise((resolve, reject) => {
    const parsedUrl = httpsUrl(url);
    httpsGet(parsedUrl, { headers: { 'User-Agent': 'claude-self-reflect-npm' } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        res.resume();
        if (redirects >= MAX_REDIRECTS) {
          return reject(new Error(`Too many redirects for ${url}`));
        }
        return downloadFile(redirectUrl(parsedUrl, res.headers.location), dest, redirects + 1)
          .then(resolve, reject);
      }
      if (res.statusCode !== 200) {
        res.resume();
        return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
      }
      const file = createWriteStream(dest, { flags: 'wx', mode: 0o600 });
      let settled = false;
      const fail = (e) => {
        if (settled) return;
        settled = true;
        file.destroy();
        try { unlinkSync(dest); } catch {}
        reject(e);
      };

      res.pipe(file);
      res.on('error', fail);
      file.on('error', fail);
      file.on('finish', () => {
        file.close((err) => {
          if (err) return fail(err);
          if (!settled) {
            settled = true;
            resolve();
          }
        });
      });
    }).on('error', reject);
  });
}

function findExpectedChecksum(checksumData, filename) {
  for (const rawLine of checksumData.split('\n')) {
    const line = rawLine.trim();
    if (!line) continue;
    const match = line.match(/^([a-fA-F0-9]{64})\s+\*?(.+)$/);
    if (match && match[2].trim() === filename) {
      return match[1].toLowerCase();
    }
  }
  throw new Error(`No checksum entry found for ${filename}`);
}

// --- Activation ---

// Activation is a separate consent event: setup writes hooks into
// ~/.claude/settings.json, registers the MCP server, and imports
// conversation transcripts. Never do that from a package manager
// lifecycle script unless the user explicitly opted in.
function runOrExplainActivation(destPath, pathBinary) {
  // Bare `csr-engine` is only safe advice when PATH really resolves to the
  // binary we installed; otherwise it runs whatever shadows it.
  const command = activationCommand({ destPath, pathBinary });

  if (process.env.CSR_AUTO_SETUP !== '1') {
    console.log('\n  \x1b[1mTo activate\x1b[0m (registers the MCP server, installs hooks,');
    console.log('  and imports your conversations), run:');
    console.log(`\n    \x1b[1;32m${command} setup\x1b[0m\n`);
    console.log('  Then restart Claude Code.\n');
    return;
  }

  // CSR_AUTO_SETUP=1 is the user's explicit opt-in to run setup from the
  // package manager, and destPath is the binary this package owns reporting
  // exactly this package's version — whether we downloaded it a second ago or
  // found it already there. Running it is the consented action, not a probe.
  console.log('  CSR_AUTO_SETUP=1 — running setup...');
  try {
    execFileSync(destPath, ['setup'], { stdio: 'inherit', timeout: 60000 });
  } catch {
    console.log(`\n  Setup failed. Run manually: ${command} setup\n`);
    process.exitCode = 1;
    return;
  }

  // Setup exiting 0 is not evidence that the registrations moved. Re-read them:
  // a hook or MCP entry still naming another binary means Claude Code will keep
  // launching that one, and "Done" would be a lie.
  const pending = registrationStale(
    detectStaleBinaries({ destPath, homeDir: homedir(), pathBinary: null })
  );
  if (pending.length > 0) {
    console.log(formatPendingActivation({ stale: pending, destPath }));
    process.exitCode = 1;
    return;
  }

  console.log('\n  \x1b[32mDone. Restart Claude Code to activate.\x1b[0m\n');
}

// --- Shadowed installation detection ---

// An npm upgrade writes INSTALL_DIR/csr-engine, but hooks and the MCP server
// are registered with the absolute path of whichever binary ran setup. If that
// was a different copy, the upgrade is invisible to Claude Code. Report it;
// never touch it — keeping another build first on PATH is a legitimate choice.
function warnAboutStaleCopies(destPath, pathBinary) {
  const warning = formatStaleWarning({
    stale: detectStaleBinaries({ destPath, homeDir: homedir(), pathBinary }),
    destPath,
  });
  if (warning) console.log(warning);
}

function detectPythonCSR() {
  const signals = [];
  // Bounded read: this runs before the install decision, so a huge or
  // non-regular settings.json must not be able to hang or OOM the install.
  const settings = readJsonConfig(join(homedir(), '.claude', 'settings.json'));
  const hooks = settings && typeof settings === 'object' ? settings.hooks : null;
  if (!hooks || typeof hooks !== 'object') return signals;

  for (const entries of Object.values(hooks)) {
    const hookStr = JSON.stringify(entries);
    if (hookStr.includes('.py') && hookStr.includes('claude-self-reflect')) {
      signals.push('Python hooks in settings.json');
      break;
    }
  }
  return signals;
}

// --- Main ---

async function main() {
  const target = detectTarget();
  const pythonSignals = detectPythonCSR();
  const destPath = join(INSTALL_DIR, BINARY_NAME);
  const pathBinary = whichBinary();

  // Get version from package.json (pinned to this release)
  const pkgVersion = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8')).version;
  const tag = `v${pkgVersion}`;

  if (pythonSignals.length > 0) {
    console.log('\n  \x1b[1;33mUpgrading from Python CSR to v8.0 (Rust)\x1b[0m');
    console.log('  v8.0 replaces Docker + Python + Qdrant with a single 44MB binary.');
    console.log('  Your conversation data is preserved.\n');
  }

  // The skip decision is about the destination and nothing else: this package
  // owns INSTALL_DIR/csr-engine and no other path. `--version` is the only
  // thing we run against a binary we did not just install.
  const plan = planInstall({ destPath, pkgVersion });

  if (plan.action === 'skip') {
    console.log(`\n  \x1b[1;32mcsr-engine ${pkgVersion} already installed:\x1b[0m ${destPath}`);
    warnAboutStaleCopies(destPath, pathBinary);
    runOrExplainActivation(destPath, pathBinary);
    return;
  }

  if (plan.reason === 'different') {
    console.log(`  Replacing ${destPath} (${plan.installedVersion}) with ${pkgVersion}...`);
  } else if (plan.reason === 'unknown') {
    // Pre-10.1 binaries have no --version flag, so "unknown" is the normal
    // answer on the first upgrade past this release.
    console.log(`  Replacing ${destPath} (version unknown) with ${pkgVersion}...`);
  }

  // Download binary
  const tarball = `csr-engine-${target}.tar.gz`;
  const url = `https://github.com/${REPO}/releases/download/${tag}/${tarball}`;
  const checksumUrl = `https://github.com/${REPO}/releases/download/${tag}/checksums.txt`;

  console.log(`  Downloading csr-engine ${tag} for ${target}...`);

  mkdirSync(INSTALL_DIR, { recursive: true });
  const tmpDir = mkdtempSync(join(tmpdir(), 'csr-install-'));

  try {
    const tarPath = join(tmpDir, tarball);
    await downloadFile(url, tarPath);

    // Verify checksum from the release's published checksums.txt.
    const checksumData = await downloadText(checksumUrl);
    const expected = findExpectedChecksum(checksumData, tarball);
    const actual = createHash('sha256').update(readFileSync(tarPath)).digest('hex');
    if (expected !== actual) {
      throw new Error(`Checksum mismatch for ${tarball}. Expected ${expected}, got ${actual}`);
    }
    console.log('  \x1b[32mChecksum verified.\x1b[0m');

    // Extract
    execFileSync('tar', ['-xzf', tarPath, '-C', tmpDir], { stdio: 'pipe' });

    // Find and install binary
    const binaryPath = join(tmpDir, BINARY_NAME);
    if (!existsSync(binaryPath)) {
      throw new Error('Binary not found in archive');
    }
    if (!lstatSync(binaryPath).isFile()) {
      throw new Error('Archive entry csr-engine is not a regular file');
    }

    // Staged rename, never a write through the destination — see installBinary.
    installBinary(binaryPath, destPath);

    console.log(`  \x1b[1;32mInstalled:\x1b[0m ${destPath}`);

    warnAboutStaleCopies(destPath, pathBinary);
    runOrExplainActivation(destPath, pathBinary);

    if (pythonSignals.length > 0) {
      console.log('  Old Python stack can be cleaned up:');
      console.log('    docker stop qdrant 2>/dev/null');
      console.log('    rm -rf ~/projects/claude-self-reflect/venv\n');
    }
  } catch (e) {
    console.error(`\n  \x1b[31mBinary download failed:\x1b[0m ${e.message}`);
    console.error('  Install manually:');
    console.error('    curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh\n');
    process.exit(1);
  } finally {
    // Cleanup temp dir
    try { rmSync(tmpDir, { recursive: true, force: true }); } catch {}
  }
}

main().catch((e) => {
  console.error(`  Postinstall error: ${e.message}`);
  console.error('  Install manually: curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh');
  process.exit(1);
});
