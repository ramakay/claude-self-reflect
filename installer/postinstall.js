#!/usr/bin/env node

import { fileURLToPath } from 'url';
import { dirname, join } from 'path';
import StatuslineSetup from './statusline-setup.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

// Only show message if not in development
if (!process.cwd().includes('claude-self-reflect')) {
  console.log('\n🎉 Claude Self-Reflect installed!\n');
  console.log('Run "claude-self-reflect setup" to configure your installation.\n');

  // Attempt to setup statusline integration automatically
  console.log('\n📊 Setting up Claude Code statusline integration...');
  const statuslineSetup = new StatuslineSetup();
  statuslineSetup.run().then(success => {
    if (success) {
      console.log('✅ Statusline integration configured automatically!');
    } else {
      console.log('⚠️ Statusline integration requires manual setup. Run "claude-self-reflect setup" for help.');
    }
  }).catch(error => {
    console.log('⚠️ Statusline setup skipped:', error.message);
  });
}