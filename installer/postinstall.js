#!/usr/bin/env node

/**
 * Post-install hook for npm. Just prints instructions.
 * The csr-engine binary must be installed separately.
 */

// Skip during development
if (process.cwd().includes('claude-self-reflect')) {
  process.exit(0);
}

console.log('\n  Claude Self-Reflect installed!\n');
console.log('  Next steps:');
console.log('    1. Install the binary:');
console.log('       curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh');
console.log('    2. Run setup:');
console.log('       csr-engine setup');
console.log('    3. Restart Claude Code\n');
