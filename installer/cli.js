#!/usr/bin/env node

/**
 * Self-Reflect CLI — thin wrapper around the csr-engine binary.
 *
 * All real work is done by csr-engine (Rust). This script provides
 * the `claude-self-reflect` npm command and proxies to the binary.
 */

import { spawn, spawnSync } from 'child_process';
import { readFileSync } from 'fs';
import { join, resolve } from 'path';
import { homedir } from 'os';
import { fileURLToPath } from 'url';
import {
  BINARY_NAME,
  ensureEngineBinary,
  findEngineBinary,
  outdatedEngine,
  whichBinary,
} from './lib.js';

const commands = {
  setup: 'Import conversations, register MCP server, install hooks, install the csr-companion plugin',
  status: 'Show system status (JSON)',
  statusline: 'Configure Claude Code statusline integration',
  help: 'Show this help message'
};

/**
 * Find the csr-engine binary.
 *
 * The package-managed copy comes first: this CLI ships pinned to an npm
 * release, and preferring whatever is first on PATH means
 * `claude-self-reflect setup` silently proxies to the older build the upgrade
 * was meant to replace. PATH and /usr/local/bin stay as fallbacks for installs
 * done through install.sh or by hand.
 */
function findBinary() {
  return findEngineBinary({ installDir: installDir(), pathBinary: whichBinary() });
}

function installDir() {
  return resolve(process.env.CSR_INSTALL_DIR || join(homedir(), '.local', 'bin'));
}

/**
 * The engine this package manages, when it is older than the package: an npm
 * upgrade whose install script npm skipped leaves the engine of the release
 * before it in place. CSR_SKIP_BINARY_DOWNLOAD=1 means the person manages the
 * binary, as it does for postinstall, so nothing is compared.
 */
function outdated(binary) {
  if (process.env.CSR_SKIP_BINARY_DOWNLOAD === '1') return null;
  return outdatedEngine({
    binary,
    destPath: join(installDir(), BINARY_NAME),
    pkgVersion: packageVersion(),
  });
}

/**
 * Run the install step npm should have run (npm 11.20+ skips postinstall until
 * the person allows it). Its progress goes to stderr so a command like
 * `claude-self-reflect status | jq` still gets clean stdout; its stderr is kept
 * for the failure reason.
 */
function installEngine() {
  const postinstallPath = fileURLToPath(new URL('./postinstall.js', import.meta.url));
  const run = spawnSync(process.execPath, [postinstallPath], {
    encoding: 'utf8',
    stdio: ['ignore', 2, 'pipe'],
  });
  if (run.status !== 0) {
    const err = new Error(`install step exited with ${run.status ?? run.signal}`);
    err.stderr = run.stderr;
    throw err;
  }
  if (run.stderr) process.stderr.write(run.stderr);
}

function runEngine(args) {
  const binary = ensureEngineBinary({
    find: findBinary,
    install: installEngine,
    say: (line) => console.error(line),
    outdated,
    // Setup is the upgrade step, so it brings an older engine up to date first.
    // Any other command says what to run and uses the engine that is there.
    update: args[0] === 'setup',
  });
  if (!binary) process.exit(1);

  const child = spawn(binary, args, { stdio: 'inherit' });
  child.on('exit', (code) => process.exit(code || 0));
  child.on('error', (err) => {
    console.error('Failed to run csr-engine:', err.message);
    process.exit(1);
  });
}

/** Version from the package.json this CLI ships in; empty if it cannot be read. */
function packageVersion() {
  try {
    const manifest = new URL('../package.json', import.meta.url);
    return JSON.parse(readFileSync(manifest, 'utf8')).version || '';
  } catch {
    return '';
  }
}

function help() {
  const version = packageVersion();
  console.log(`\nSelf-Reflect${version ? ` v${version}` : ''} - Claude reflects on its own work\n`);
  console.log('Usage: claude-self-reflect <command>\n');
  console.log('Commands:');
  for (const [cmd, desc] of Object.entries(commands)) {
    console.log(`  ${cmd.padEnd(12)} ${desc}`);
  }
  console.log('\nDirect binary usage (faster):');
  console.log('  csr-engine setup              # First-time setup');
  console.log('  csr-engine status             # JSON status');
  console.log('  csr-engine status --compact    # Statusline output');
  console.log('  csr-engine daemon             # Background enrichment');
  console.log('\nMore info: https://github.com/ramakay/claude-self-reflect');
}

const command = process.argv[2] || 'help';

switch (command) {
  case 'setup':
    runEngine(['setup', ...process.argv.slice(3)]);
    break;
  case 'status':
    runEngine(['status', ...process.argv.slice(3)]);
    break;
  case 'statusline': {
    import('./statusline-setup.js').then(module => {
      const StatuslineSetup = module.default;
      const setup = new StatuslineSetup();
      if (process.argv[3] === '--restore') {
        setup.restore();
      } else {
        setup.run();
      }
    }).catch(err => {
      console.error('Statusline setup failed:', err.message);
      process.exit(1);
    });
    break;
  }
  case 'help':
  default:
    help();
    break;
}
