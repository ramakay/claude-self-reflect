#!/usr/bin/env node

import { spawn, execSync } from 'child_process';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';
import fs from 'fs/promises';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

const commands = {
  setup: 'Run the setup wizard to configure Claude Self-Reflect',
  update: 'Check for and install missing features or updates',
  status: 'Get indexing status as JSON (overall + per-project breakdown)',
  statusline: 'Configure Claude Code statusline integration',
  doctor: 'Check your installation and diagnose issues',
  version: 'Show version and check for updates',
  uninstall: 'Uninstall Claude Self-Reflect',
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

async function status() {
  // Call the Python status script directly
  const mcpServerPath = join(__dirname, '..', 'mcp-server');
  
  // Check for venv or .venv
  let venvPython = join(mcpServerPath, 'venv', 'bin', 'python');
  try {
    await fs.access(venvPython);
  } catch {
    venvPython = join(mcpServerPath, '.venv', 'bin', 'python');
  }
  
  const statusScript = join(mcpServerPath, 'src', 'status.py');
  
  try {
    const child = spawn(venvPython, [statusScript], {
      cwd: mcpServerPath,
      stdio: ['inherit', 'pipe', 'pipe']
    });
    
    let stdout = '';
    let stderr = '';
    
    child.stdout.on('data', (data) => {
      stdout += data.toString();
    });
    
    child.stderr.on('data', (data) => {
      stderr += data.toString();
    });
    
    child.on('exit', (code) => {
      if (code === 0) {
        // Output the JSON directly for other tools to parse
        process.stdout.write(stdout);
        process.exit(0);
      } else {
        console.error('Error getting status:', stderr || 'Unknown error');
        process.exit(1);
      }
    });
    
    // Handle timeout
    setTimeout(() => {
      child.kill('SIGTERM');
      console.error('Status check timed out');
      process.exit(1);
    }, 10000); // 10 second timeout
    
  } catch (error) {
    console.error('Failed to execute status command:', error.message);
    process.exit(1);
  }
}

async function doctor() {
  console.log('🔍 Running comprehensive diagnostics...\n');
  
  // Use the new Python doctor script for comprehensive checks
  const doctorScript = join(__dirname, '..', 'scripts', 'doctor.py');
  
  try {
    // Check if Python is available
    try {
      execSync('python3 --version', { stdio: 'ignore' });
    } catch {
      console.log('❌ Python 3 is required but not found');
      console.log('   Please install Python 3.10 or later');
      process.exit(1);
    }
    
    // Run the doctor script
    const child = spawn('python3', [doctorScript], { 
      stdio: 'inherit',
      cwd: join(__dirname, '..')
    });
    
    child.on('exit', (code) => {
      if (code === 0) {
        console.log('\n✅ All checks passed!');
      } else if (code === 2) {
        console.log('\n⚠️  Some issues found - see recommendations above');
      } else {
        console.log('\n❌ Critical issues found - please address them first');
      }
      process.exit(code || 0);
    });
  } catch (error) {
    // Fallback to basic checks if doctor script fails
    console.log('⚠️  Could not run comprehensive diagnostics, using basic checks...\n');
    
    const checks = [
      {
        name: 'Docker',
        check: async () => {
          try {
            execSync('docker info', { stdio: 'ignore' });
            return { passed: true, message: 'Docker is running' };
          } catch {
            return { passed: false, message: 'Docker not running or not installed' };
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
              return { passed: true, message: `Qdrant ${data.version} is running` };
            }
          } catch {}
          return { passed: false, message: 'Qdrant not accessible on localhost:6333' };
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
  console.log('  claude-self-reflect update          # Fix missing features');
  console.log('  claude-self-reflect status          # Get indexing status');
  console.log('  claude-self-reflect doctor          # Diagnose issues');

  console.log('\nFor more information:');
  console.log('  Documentation: https://github.com/ramakay/claude-self-reflect');
  console.log('  Issues: https://github.com/ramakay/claude-self-reflect/issues');
}

async function statusline() {
  const StatuslineSetup = (await import('./statusline-setup.js')).default;
  const setup = new StatuslineSetup();

  if (process.argv[3] === '--restore') {
    console.log('🔄 Restoring original statusline...');
    setup.restore();
  } else {
    await setup.run();
  }
}

// Helper function to run bash scripts with validation
async function runBashScript(scriptName, errorMessage) {
  const scriptPath = join(__dirname, '..', 'scripts', 'setup', scriptName);

  // Check bash is available
  try {
    execSync('bash --version', { stdio: 'ignore' });
  } catch {
    console.error('❌ Bash is required but not found');
    console.error('   Please ensure bash is installed');
    process.exit(1);
  }

  // Check script file exists
  try {
    await fs.access(scriptPath);
  } catch {
    console.error(`❌ Script not found: ${scriptPath}`);
    console.error('   Please reinstall claude-self-reflect');
    process.exit(1);
  }

  // Run script
  try {
    const child = spawn('bash', [scriptPath], { stdio: 'inherit' });
    child.on('exit', (code) => {
      process.exit(code || 0);
    });
  } catch (error) {
    console.error(`${errorMessage} ${error.message}`);
    process.exit(1);
  }
}

async function update() {
  console.log('🔄 Checking for missing features and updates...\n');

  try {
    const UpdateManager = (await import('./update-manager.js')).default;
    const manager = new UpdateManager();
    await manager.run();
  } catch (error) {
    // Check if update-manager.js itself is missing (not a nested dependency)
    // Node uses file:// URLs in error messages, not relative paths
    const isUpdateManagerMissing =
      (error.code === 'ERR_MODULE_NOT_FOUND' &&
       (error.message.includes('update-manager.js') || error.message.includes('update-manager'))) ||
      (error.message && error.message.includes('Cannot find module') && error.message.includes('update-manager'));

    if (isUpdateManagerMissing) {
      console.error('❌ Update manager not found');
      console.error('   Please reinstall: npm install -g claude-self-reflect');
    } else {
      console.error('❌ Update check failed:', error.message);
      console.error('   Run with DEBUG=1 for more details');
      if (process.env.DEBUG) {
        console.error('\nStack trace:', error.stack);
      }
    }
    process.exit(1);
  }
}

async function version() {
  await runBashScript('version.sh', 'Failed to check version:');
}

async function uninstall() {
  await runBashScript('uninstall.sh', 'Failed to run uninstall:');
}

// Main
const command = process.argv[2] || 'help';

switch (command) {
  case 'setup':
    setup();
    break;
  case 'update':
    update();
    break;
  case 'status':
    status();
    break;
  case 'statusline':
    statusline();
    break;
  case 'doctor':
    doctor();
    break;
  case 'version':
    version();
    break;
  case 'uninstall':
    uninstall();
    break;
  case 'help':
  default:
    help();
    break;
}