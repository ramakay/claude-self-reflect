#!/usr/bin/env node

import { execSync, spawn, spawnSync } from 'child_process';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';
import fs from 'fs/promises';
import fsSync from 'fs';
import readline from 'readline';
import path from 'path';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const projectRoot = join(__dirname, '..');

// Parse command line arguments
const args = process.argv.slice(2);
let voyageKey = null;
let debugMode = false;

for (const arg of args) {
  if (arg.startsWith('--voyage-key=')) {
    voyageKey = arg.split('=')[1];
  } else if (arg === '--debug') {
    debugMode = true;
  }
}

// Default to local mode unless Voyage key is provided
let localMode = !voyageKey;

// Helper to safely execute commands
function safeExec(command, args = [], options = {}) {
  const result = spawnSync(command, args, {
    ...options,
    shell: false
  });
  
  if (result.error) {
    throw result.error;
  }
  
  if (result.status !== 0) {
    const error = new Error(`Command failed: ${command} ${args.join(' ')}`);
    error.stdout = result.stdout;
    error.stderr = result.stderr;
    error.status = result.status;
    throw error;
  }
  
  return result.stdout?.toString() || '';
}

const isInteractive = process.stdin.isTTY && process.stdout.isTTY;

const rl = isInteractive ? readline.createInterface({
  input: process.stdin,
  output: process.stdout
}) : null;

const question = (query) => {
  if (!isInteractive) {
    console.log(`Non-interactive mode detected. ${query} [Defaulting to 'n']`);
    return Promise.resolve('n');
  }
  return new Promise((resolve) => rl.question(query, resolve));
};

async function checkDocker() {
  console.log('\n🐳 Checking Docker...');
  try {
    safeExec('docker', ['info'], { stdio: 'ignore' });
    console.log('✅ Docker is installed and running');
    
    // Check docker compose
    try {
      safeExec('docker', ['compose', 'version'], { stdio: 'ignore' });
      console.log('✅ Docker Compose v2 is available');
      return true;
    } catch {
      console.log('❌ Docker Compose v2 not found');
      console.log('   Please update Docker Desktop to the latest version');
      return false;
    }
  } catch {
    console.log('❌ Docker is not running or not installed');
    console.log('\n📋 Please install Docker:');
    console.log('   • macOS/Windows: https://docker.com/products/docker-desktop');
    console.log('   • Linux: https://docs.docker.com/engine/install/');
    console.log('\n   After installation, make sure Docker is running and try again.');
    return false;
  }
}

async function configureEnvironment() {
  console.log('\n🔐 Configuring environment...');
  
  const envPath = join(projectRoot, '.env');
  let envContent = '';
  let hasValidApiKey = false;
  
  try {
    envContent = await fs.readFile(envPath, 'utf-8');
  } catch {
    // .env doesn't exist, create it
  }
  
  // Check if we have a command line API key
  if (voyageKey) {
    if (voyageKey.startsWith('pa-')) {
      console.log('✅ Using API key from command line');
      envContent = envContent.replace(/VOYAGE_KEY=.*/g, '');
      envContent += `\nVOYAGE_KEY=${voyageKey}\n`;
      hasValidApiKey = true;
    } else {
      console.log('❌ Invalid API key format. Voyage keys start with "pa-"');
      process.exit(1);
    }
  } else if (localMode) {
    console.log('🏠 Running in local mode - no API key required');
    hasValidApiKey = false;
  } else {
    // Check if we already have a valid API key
    const existingKeyMatch = envContent.match(/VOYAGE_KEY=([^\s]+)/);
    if (existingKeyMatch && existingKeyMatch[1] && !existingKeyMatch[1].includes('your-')) {
      console.log('✅ Found existing Voyage API key in .env file');
      hasValidApiKey = true;
    } else if (isInteractive) {
      console.log('\n🔑 Voyage AI API Key Setup (Optional)');
      console.log('━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━');
      console.log('For better search accuracy, you can use Voyage AI embeddings.');
      console.log('Skip this to use local embeddings (recommended for privacy).\n');
      
      const inputKey = await question('Paste your Voyage AI key (or press Enter to skip): ');
      
      if (inputKey && inputKey.trim() && inputKey.trim().startsWith('pa-')) {
        envContent = envContent.replace(/VOYAGE_KEY=.*/g, '');
        envContent += `\nVOYAGE_KEY=${inputKey.trim()}\n`;
        hasValidApiKey = true;
        console.log('✅ API key saved');
      } else if (inputKey && inputKey.trim()) {
        console.log('⚠️  Invalid key format. Skipping...');
      }
    }
  }
  
  // Set default values
  if (!envContent.includes('QDRANT_URL=')) {
    envContent += 'QDRANT_URL=http://localhost:6333\n';
  }
  if (!envContent.includes('ENABLE_MEMORY_DECAY=')) {
    envContent += 'ENABLE_MEMORY_DECAY=false\n';
  }
  if (!envContent.includes('PREFER_LOCAL_EMBEDDINGS=')) {
    envContent += `PREFER_LOCAL_EMBEDDINGS=${localMode ? 'true' : 'false'}\n`;
  }
  
  await fs.writeFile(envPath, envContent.trim() + '\n');
  console.log('✅ Environment configured');
  
  return { hasValidApiKey };
}

async function startDockerServices() {
  console.log('\n🚀 Starting Docker services...');
  
  try {
    // First, ensure any old containers are stopped
    console.log('🧹 Cleaning up old containers...');
    try {
      safeExec('docker', ['compose', 'down'], { 
        cwd: projectRoot, 
        stdio: 'pipe' 
      });
    } catch {
      // Ignore errors if no containers exist
    }
    
    // Start Qdrant and MCP server
    console.log('📦 Starting Qdrant database and MCP server...');
    safeExec('docker', ['compose', '--profile', 'mcp', 'up', '-d'], {
      cwd: projectRoot,
      stdio: 'inherit'
    });
    
    // Wait for services to be ready
    console.log('⏳ Waiting for services to start...');
    await new Promise(resolve => setTimeout(resolve, 5000));
    
    // Check if services are running
    const psOutput = safeExec('docker', ['compose', 'ps', '--format', 'table'], {
      cwd: projectRoot,
      encoding: 'utf8'
    });
    
    console.log('\n📊 Service Status:');
    console.log(psOutput);
    
    return true;
  } catch (error) {
    console.log('❌ Failed to start Docker services:', error.message);
    return false;
  }
}

async function configureClaude() {
  console.log('\n🤖 Configuring Claude Desktop...');
  
  const mcpScript = join(projectRoot, 'mcp-server', 'run-mcp-docker.sh');
  
  // Create a script that runs the MCP server in Docker
  const scriptContent = `#!/bin/bash
docker exec -i claude-reflection-mcp python -m src.server_v2
`;
  
  await fs.writeFile(mcpScript, scriptContent, { mode: 0o755 });
  
  // Check if Claude CLI is available
  try {
    safeExec('which', ['claude'], { stdio: 'ignore' });
    
    console.log('🔧 Adding MCP to Claude Desktop...');
    try {
      const mcpArgs = ['mcp', 'add', 'claude-self-reflect', mcpScript];
      safeExec('claude', mcpArgs, { stdio: 'inherit' });
      console.log('✅ MCP added successfully!');
      console.log('\n⚠️  Please restart Claude Desktop for changes to take effect.');
    } catch {
      console.log('⚠️  Could not add MCP automatically');
      showManualConfig(mcpScript);
    }
  } catch {
    console.log('⚠️  Claude CLI not found');
    showManualConfig(mcpScript);
  }
}

function showManualConfig(mcpScript) {
  console.log('\nAdd this to your Claude Desktop config manually:');
  console.log('```json');
  console.log(JSON.stringify({
    "claude-self-reflect": {
      "command": mcpScript
    }
  }, null, 2));
  console.log('```');
}

async function importConversations() {
  console.log('\n📚 Importing conversations...');
  
  const answer = await question('Would you like to import your existing Claude conversations? (y/n): ');
  
  if (answer.toLowerCase() === 'y') {
    console.log('🔄 Starting import process...');
    console.log('   This may take a few minutes depending on your conversation history');
    
    try {
      safeExec('docker', ['compose', 'run', '--rm', 'importer'], {
        cwd: projectRoot,
        stdio: 'inherit'
      });
      console.log('\n✅ Import completed!');
    } catch {
      console.log('\n⚠️  Import had some issues, but you can continue');
    }
  } else {
    console.log('📝 Skipping import. You can import later with:');
    console.log('   docker compose run --rm importer');
  }
}

async function showFinalInstructions() {
  console.log('\n✅ Setup complete!');
  console.log('\n📋 Quick Reference Commands:');
  console.log('   • Check status: docker compose ps');
  console.log('   • View logs: docker compose logs -f');
  console.log('   • Import conversations: docker compose run --rm importer');
  console.log('   • Start watcher: docker compose --profile watch up -d');
  console.log('   • Stop all: docker compose down');
  
  console.log('\n🎯 Next Steps:');
  console.log('1. Restart Claude Desktop');
  console.log('2. Look for "claude-self-reflect" in the MCP tools');
  console.log('3. Try: "Search my past conversations about Python"');
  
  console.log('\n📚 Documentation: https://github.com/ramakay/claude-self-reflect');
}

async function main() {
  console.log('🚀 Claude Self-Reflect Setup (Docker Edition)\n');
  console.log('This simplified setup runs everything in Docker.');
  console.log('No Python installation required!\n');
  
  // Check Docker
  const dockerOk = await checkDocker();
  if (!dockerOk) {
    if (rl) rl.close();
    process.exit(1);
  }
  
  // Configure environment
  await configureEnvironment();
  
  // Start services
  const servicesOk = await startDockerServices();
  if (!servicesOk) {
    console.log('\n❌ Failed to start services');
    console.log('   Check the Docker logs for details');
    if (rl) rl.close();
    process.exit(1);
  }
  
  // Configure Claude
  await configureClaude();
  
  // Import conversations
  await importConversations();
  
  // Show final instructions
  await showFinalInstructions();
  
  if (rl) rl.close();
}

main().catch(error => {
  console.error('❌ Setup failed:', error);
  if (rl) rl.close();
  process.exit(1);
});