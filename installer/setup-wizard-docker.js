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
    
    // Check for existing bind mount data that needs migration
    const bindMountPath = join(projectRoot, 'data', 'qdrant');
    try {
      await fs.access(bindMountPath);
      const files = await fs.readdir(bindMountPath);
      if (files.length > 0) {
        console.log('\n⚠️  Found existing Qdrant data in ./data/qdrant');
        console.log('📦 This will be automatically migrated to Docker volume on first start.');
        
        // Create a migration marker
        await fs.writeFile(join(projectRoot, '.needs-migration'), 'true');
      }
    } catch {
      // No existing data, nothing to migrate
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
    
    // Check if we need to migrate data
    try {
      await fs.access(join(projectRoot, '.needs-migration'));
      console.log('\n🔄 Migrating data from bind mount to Docker volume...');
      
      // Stop Qdrant to perform migration
      safeExec('docker', ['compose', 'stop', 'qdrant'], {
        cwd: projectRoot,
        stdio: 'pipe'
      });
      
      // Copy data from bind mount to Docker volume
      safeExec('docker', ['run', '--rm', 
        '-v', `${projectRoot}/data/qdrant:/source:ro`,
        '-v', 'claude-self-reflect_qdrant_data:/target',
        'alpine', 'sh', '-c', 'cp -R /source/* /target/'
      ], {
        cwd: projectRoot,
        stdio: 'inherit'
      });
      
      console.log('✅ Data migration completed!');
      
      // Remove migration marker
      await fs.unlink(join(projectRoot, '.needs-migration'));
      
      // Restart Qdrant
      safeExec('docker', ['compose', '--profile', 'mcp', 'up', '-d', 'qdrant'], {
        cwd: projectRoot,
        stdio: 'pipe'
      });
      
      await new Promise(resolve => setTimeout(resolve, 3000));
    } catch {
      // No migration needed
    }
    
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
# Run the MCP server in the Docker container with stdin attached
# Using python -u for unbuffered output
# Using the main module which properly supports local embeddings
docker exec -i claude-reflection-mcp python -u -m src
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
  
  console.log('\n🎯 Your Claude Self-Reflect System:');
  console.log('   • 🌐 Qdrant Dashboard: http://localhost:6333/dashboard/');
  console.log('   • 📊 Status: All services running');
  console.log('   • 🔍 Search: Semantic search with memory decay enabled');
  console.log('   • 🚀 Import: Watcher checking every 60 seconds');
  
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

async function checkExistingInstallation() {
  try {
    // Check if services are already running
    const psResult = safeExec('docker', ['compose', '-f', 'docker-compose.yaml', 'ps', '--format', 'json'], {
      cwd: projectRoot,
      encoding: 'utf8'
    });
    
    if (psResult && psResult.includes('claude-reflection-')) {
      const services = psResult.split('\n').filter(line => line.trim());
      const runningServices = services.filter(line => line.includes('"State":"running"')).length;
      
      if (runningServices >= 2) {  // At least Qdrant and MCP should be running
        console.log('✅ Claude Self-Reflect is already installed and running!\n');
        console.log('🎯 Your System Status:');
        console.log('   • 🌐 Qdrant Dashboard: http://localhost:6333/dashboard/');
        console.log('   • 📊 Services: ' + runningServices + ' containers running');
        console.log('   • 🔍 Mode: ' + (localMode ? 'Local embeddings (privacy mode)' : 'Cloud embeddings (Voyage AI)'));
        console.log('   • ⚡ Memory decay: Enabled (90-day half-life)');
        
        console.log('\n📋 Quick Commands:');
        console.log('   • View status: docker compose ps');
        console.log('   • View logs: docker compose logs -f');
        console.log('   • Restart: docker compose restart');
        console.log('   • Stop: docker compose down');
        
        console.log('\n💡 To re-run setup, first stop services with: docker compose down');
        return true;
      }
    }
  } catch (err) {
    // Services not running, continue with setup
  }
  return false;
}

async function main() {
  console.log('🚀 Claude Self-Reflect Setup (Docker Edition)\n');
  
  // Check if already installed
  const alreadyInstalled = await checkExistingInstallation();
  if (alreadyInstalled) {
    if (rl) rl.close();
    process.exit(0);
  }
  
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