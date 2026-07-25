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
  chmodSync,
  unlinkSync,
  mkdtempSync,
  rmSync,
  copyFileSync,
  lstatSync,
} from 'fs';
import { dirname, join, resolve } from 'path';
import { homedir, platform, arch, tmpdir } from 'os';
import { execFileSync } from 'child_process';
import { createHash } from 'crypto';
import { get as httpsGet } from 'https';
import { fileURLToPath } from 'url';

const REPO = 'ramakay/claude-self-reflect';
const BINARY_NAME = 'csr-engine';
const INSTALL_DIR = process.env.CSR_INSTALL_DIR || join(homedir(), '.local', 'bin');
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
function runOrExplainActivation(binaryPath) {
  if (process.env.CSR_AUTO_SETUP === '1') {
    console.log('  CSR_AUTO_SETUP=1 — running setup...');
    try {
      execFileSync(binaryPath, ['setup'], { stdio: 'inherit', timeout: 60000 });
      console.log('\n  \x1b[32mDone. Restart Claude Code to activate.\x1b[0m\n');
    } catch {
      console.log('\n  Setup failed. Run manually: csr-engine setup\n');
      process.exitCode = 1;
    }
  } else {
    console.log('\n  \x1b[1mTo activate\x1b[0m (registers the MCP server, installs hooks,');
    console.log('  and imports your conversations), run:');
    console.log('\n    \x1b[1;32mcsr-engine setup\x1b[0m\n');
    console.log('  Then restart Claude Code.\n');
  }
}

// --- Existing installation detection ---

function findExistingBinary() {
  const candidates = [
    join(INSTALL_DIR, BINARY_NAME),
    '/usr/local/bin/csr-engine',
  ];
  try {
    const which = execFileSync('which', ['csr-engine'], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim();
    if (which) candidates.unshift(which);
  } catch {}

  for (const p of candidates) {
    if (existsSync(p)) return p;
  }
  return null;
}

function detectPythonCSR() {
  const signals = [];
  try {
    const settingsPath = join(homedir(), '.claude', 'settings.json');
    if (existsSync(settingsPath)) {
      const settings = JSON.parse(readFileSync(settingsPath, 'utf8'));
      const hooks = settings.hooks || {};
      for (const entries of Object.values(hooks)) {
        const hookStr = JSON.stringify(entries);
        if (hookStr.includes('.py') && hookStr.includes('claude-self-reflect')) {
          signals.push('Python hooks in settings.json');
          break;
        }
      }
    }
  } catch {}
  return signals;
}

// --- Main ---

async function main() {
  const target = detectTarget();
  const existingBinary = findExistingBinary();
  const pythonSignals = detectPythonCSR();

  // Get version from package.json (pinned to this release)
  const pkgVersion = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8')).version;
  const tag = `v${pkgVersion}`;

  if (pythonSignals.length > 0) {
    console.log('\n  \x1b[1;33mUpgrading from Python CSR to v8.0 (Rust)\x1b[0m');
    console.log('  v8.0 replaces Docker + Python + Qdrant with a single 44MB binary.');
    console.log('  Your conversation data is preserved.\n');
  }

  if (existingBinary) {
    // Check if existing binary is the right version
    try {
      const version = execFileSync(existingBinary, ['--version'], {
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'ignore'],
      }).trim();
      if (version === pkgVersion || version === tag || version.includes(` ${pkgVersion}`)) {
        console.log(`\n  \x1b[1;32mcsr-engine ${pkgVersion} already installed.\x1b[0m`);
        runOrExplainActivation(existingBinary);
        return;
      }
      console.log(`  Updating ${existingBinary} to ${tag}...`);
    } catch {
      console.log(`  Updating ${existingBinary}...`);
    }
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

    const destPath = join(INSTALL_DIR, BINARY_NAME);
    copyFileSync(binaryPath, destPath);
    chmodSync(destPath, 0o755);

    console.log(`  \x1b[1;32mInstalled:\x1b[0m ${destPath}`);

    runOrExplainActivation(destPath);

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
