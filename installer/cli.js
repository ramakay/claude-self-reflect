#!/usr/bin/env node

/**
 * Claude Self-Reflect CLI — thin wrapper around the csr-engine binary.
 *
 * All real work is done by csr-engine (Rust). This script provides
 * the `claude-self-reflect` npm command and proxies to the binary.
 */

import { spawn, execSync } from 'child_process';
import { existsSync } from 'fs';
import { join } from 'path';
import { homedir } from 'os';

const commands = {
  setup: 'Import conversations, register MCP server, install hooks',
  status: 'Show system status (JSON)',
  statusline: 'Configure Claude Code statusline integration',
  help: 'Show this help message'
};

/** Find the csr-engine binary. Checks common install locations. */
function findBinary() {
  const candidates = [
    join(homedir(), '.local', 'bin', 'csr-engine'),
    '/usr/local/bin/csr-engine',
  ];

  // Also check PATH
  try {
    const which = execSync('which csr-engine 2>/dev/null', { encoding: 'utf8' }).trim();
    if (which) candidates.unshift(which);
  } catch {}

  for (const p of candidates) {
    if (existsSync(p)) return p;
  }
  return null;
}

function runEngine(args) {
  const binary = findBinary();
  if (!binary) {
    console.error('csr-engine binary not found.');
    console.error('Install it: curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh');
    process.exit(1);
  }

  const child = spawn(binary, args, { stdio: 'inherit' });
  child.on('exit', (code) => process.exit(code || 0));
  child.on('error', (err) => {
    console.error('Failed to run csr-engine:', err.message);
    process.exit(1);
  });
}

function help() {
  console.log('\nClaude Self-Reflect v8.0 - Perfect memory for Claude\n');
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
