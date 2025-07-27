#!/usr/bin/env node

import { spawn } from 'child_process';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';
import fs from 'fs/promises';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

const commands = {
  setup: 'Run the setup wizard to configure Claude Self-Reflect',
  doctor: 'Check your installation and diagnose issues',
  help: 'Show this help message'
};

async function setup() {
  console.log('🚀 Claude Self-Reflect Setup Wizard\n');
  
  const setupPath = join(__dirname, 'setup-wizard.js');
  // Pass along any command line arguments after 'setup'
  const args = process.argv.slice(3); // Skip node, script, and 'setup'
  const child = spawn('node', [setupPath, ...args], { stdio: 'inherit' });
  
  child.on('exit', (code) => {
    process.exit(code || 0);
  });
}

async function doctor() {
  console.log('🔍 Checking Claude Self-Reflect installation...\n');
  
  const checks = [
    {
      name: 'Python 3.10+',
      check: async () => {
        try {
          const { execSync } = await import('child_process');
          const version = execSync('python3 --version').toString().trim();
          return { passed: true, message: version };
        } catch {
          return { passed: false, message: 'Python 3.10+ not found' };
        }
      }
    },
    {
      name: 'Qdrant',
      check: async () => {
        try {
          const response = await fetch('http://localhost:6333');
          const data = await response.json();
          if (data.title && data.title.includes('qdrant')) {
            return { passed: true, message: `Qdrant ${data.version} is running on port 6333` };
          }
        } catch {}
        return { passed: false, message: 'Qdrant not accessible on localhost:6333' };
      }
    },
    {
      name: 'MCP Server',
      check: async () => {
        const mcpPath = join(__dirname, '..', 'mcp-server', 'pyproject.toml');
        try {
          await fs.access(mcpPath);
          return { passed: true, message: 'MCP server files found' };
        } catch {
          return { passed: false, message: 'MCP server files not found' };
        }
      }
    },
    {
      name: 'Environment Variables',
      check: async () => {
        const envPath = join(__dirname, '..', '.env');
        try {
          const content = await fs.readFile(envPath, 'utf-8');
          const hasVoyageKey = content.includes('VOYAGE_KEY=') && !content.includes('VOYAGE_KEY=your-');
          if (hasVoyageKey) {
            return { passed: true, message: '.env file configured with VOYAGE_KEY' };
          }
          return { passed: false, message: '.env file missing VOYAGE_KEY' };
        } catch {
          return { passed: false, message: '.env file not found' };
        }
      }
    }
  ];
  
  for (const check of checks) {
    const result = await check.check();
    const icon = result.passed ? '✅' : '❌';
    console.log(`${icon} ${check.name}: ${result.message}`);
  }
  
  console.log('\n💡 Run "claude-self-reflect setup" to fix any issues');
}

const ASCII_ART = `
  ____  _____ _____ _     _____ ____ _____ 
 |  _ \\| ____|  ___| |   | ____/ ___|_   _|
 | |_) |  _| | |_  | |   |  _|| |     | |  
 |  _ <| |___|  _| | |___| |__| |___  | |  
 |_| \\_\\_____|_|   |_____|_____\\____| |_|  
                                            
         Memory that learns and forgets
`;

function help() {
  console.log(ASCII_ART);
  console.log('\nClaude Self-Reflect - Perfect memory for Claude\n');
  console.log('Usage: claude-self-reflect <command> [options]\n');
  console.log('Commands:');
  
  for (const [cmd, desc] of Object.entries(commands)) {
    console.log(`  ${cmd.padEnd(10)} ${desc}`);
  }
  
  console.log('\nSetup Options:');
  console.log('  --voyage-key=<key>   Provide Voyage AI API key (recommended)');
  console.log('  --local              Run in local mode without API key');
  console.log('  --debug              Enable debug output for troubleshooting');
  
  console.log('\nExamples:');
  console.log('  claude-self-reflect setup --voyage-key=pa-1234567890');
  console.log('  claude-self-reflect setup --local');
  console.log('  claude-self-reflect setup --debug  # For troubleshooting');
  
  console.log('\nFor more information: https://github.com/ramakay/claude-self-reflect');
}

// Main
const command = process.argv[2] || 'help';

switch (command) {
  case 'setup':
    setup();
    break;
  case 'doctor':
    doctor();
    break;
  case 'help':
  default:
    help();
    break;
}