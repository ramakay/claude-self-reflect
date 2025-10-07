#!/usr/bin/env node

import { fileURLToPath } from 'url';
import { dirname, join } from 'path';
import StatuslineSetup from './statusline-setup.js';
import FastEmbedFallback from './fastembed-fallback.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

// Only show message if not in development
if (!process.cwd().includes('claude-self-reflect')) {
  console.log('\n🎉 Claude Self-Reflect installed!\n');
  console.log('🔍 Checking installation...\n');

  // Import and run update manager for comprehensive setup
  import('./update-manager.js').then(module => {
    const UpdateManager = module.default;
    const manager = new UpdateManager();
    manager.run().then(() => {
      console.log('\n✅ Installation complete!');
      console.log('\n📋 Next steps:');
      console.log('   1. Run: claude-self-reflect setup');
      console.log('   2. Configure your embedding preferences');
      console.log('   3. Start using Claude with perfect memory!\n');
    }).catch(error => {
      console.log('\n⚠️  Setup encountered issues:', error.message);
      console.log('   Run "claude-self-reflect update" to fix any problems\n');
    });
  }).catch(error => {
    console.log('⚠️  Could not run automatic setup');
    console.log('   Run "claude-self-reflect setup" to configure manually\n');
  });
}