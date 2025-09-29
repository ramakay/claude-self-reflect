#!/usr/bin/env node

import { execSync, spawn, spawnSync } from 'child_process';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';
import fs from 'fs/promises';
import fsSync from 'fs';
import readline from 'readline';
import path from 'path';
import os from 'os';

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
  
  // Setup config directory in user's home directory for global npm installs
  const userConfigDir = join(os.homedir(), '.claude-self-reflect', 'config');
  
  try {
    await fs.mkdir(userConfigDir, { recursive: true });
    console.log(`📁 Using config directory: ${userConfigDir}`);
    
    // Migrate existing config from project directory if it exists
    const oldConfigDir = join(projectRoot, 'config');
    try {
      await fs.access(oldConfigDir);
      const files = await fs.readdir(oldConfigDir);
      if (files.length > 0) {
        console.log('🔄 Migrating existing config data...');
        for (const file of files) {
          const sourcePath = join(oldConfigDir, file);
          const targetPath = join(userConfigDir, file);
          try {
            await fs.copyFile(sourcePath, targetPath);
          } catch (err) {
            // Ignore copy errors, file might already exist
          }
        }
        console.log('✅ Config migration completed');
      }
    } catch {
      // No old config directory, nothing to migrate
    }

    // Copy qdrant-config.yaml from npm package to user config directory
    // This is critical for global npm installs where Docker cannot mount from /opt/homebrew
    const sourceQdrantConfig = join(projectRoot, 'config', 'qdrant-config.yaml');
    const targetQdrantConfig = join(userConfigDir, 'qdrant-config.yaml');
    try {
      await fs.copyFile(sourceQdrantConfig, targetQdrantConfig);
      console.log('✅ Qdrant config copied to user directory');
    } catch (err) {
      if (err.code !== 'ENOENT') {
        console.log('⚠️  Could not copy qdrant-config.yaml:', err.message);
        console.log('   Docker may have issues starting Qdrant service');
      }
    }
  } catch (error) {
    console.log(`❌ Could not create config directory: ${error.message}`);
    console.log('   This may cause Docker mount issues. Please check permissions.');
    throw error;
  }
  
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
  if (!envContent.includes('CONFIG_PATH=')) {
    envContent += `CONFIG_PATH=${userConfigDir}\n`;
  }
  // CRITICAL: Set CLAUDE_LOGS_PATH with expanded home directory
  // Docker doesn't expand ~ in volume mounts
  if (!envContent.includes('CLAUDE_LOGS_PATH=')) {
    const claudeLogsPath = path.join(os.homedir(), '.claude', 'projects');
    envContent += `CLAUDE_LOGS_PATH=${claudeLogsPath}\n`;
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
  console.log('\n🤖 Configuring Claude Code...');
  
  // Use the clean wrapper if running locally, Docker script for Docker mode
  const isDockerMode = process.env.USE_DOCKER_MCP === 'true';
  const mcpScript = isDockerMode 
    ? join(projectRoot, 'mcp-server', 'run-mcp-docker.sh')
    : join(projectRoot, 'mcp-server', 'run-mcp.sh');
  
  if (isDockerMode) {
    // Create a script that runs the MCP server in Docker
    const scriptContent = `#!/bin/bash
# Run the MCP server in the Docker container with stdin attached
# Using python -u for unbuffered output
# Using the main module which properly supports local embeddings
docker exec -i claude-reflection-mcp python -u -m src
`;
    
    await fs.writeFile(mcpScript, scriptContent, { mode: 0o755 });
  }
  
  // Check if Claude CLI is available
  try {
    safeExec('which', ['claude'], { stdio: 'ignore' });
    
    console.log('🔧 Adding MCP to Claude Code...');
    try {
      const mcpArgs = ['mcp', 'add', 'claude-self-reflect', mcpScript];
      safeExec('claude', mcpArgs, { stdio: 'inherit' });
      console.log('✅ MCP added successfully!');
      console.log('\n⚠️  Please restart Claude Code for changes to take effect.');
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
  console.log('\nAdd this to your Claude Code config manually:');
  console.log('```json');
  console.log(JSON.stringify({
    "claude-self-reflect": {
      "command": mcpScript
    }
  }, null, 2));
  console.log('```');
}

async function importConversations() {
  console.log('\n📚 Checking conversation baseline...');
  
  // First check if Claude projects directory exists and has JSONL files
  const claudeProjectsDir = path.join(os.homedir(), '.claude', 'projects');
  let totalJsonlFiles = 0;
  let projectCount = 0;
  
  try {
    if (fsSync.existsSync(claudeProjectsDir)) {
      const projects = fsSync.readdirSync(claudeProjectsDir);
      for (const project of projects) {
        const projectPath = path.join(claudeProjectsDir, project);
        if (fsSync.statSync(projectPath).isDirectory()) {
          const jsonlFiles = fsSync.readdirSync(projectPath).filter(f => f.endsWith('.jsonl'));
          if (jsonlFiles.length > 0) {
            projectCount++;
            totalJsonlFiles += jsonlFiles.length;
          }
        }
      }
    }
    
    if (totalJsonlFiles === 0) {
      console.log('\n⚠️  No Claude conversation files found!');
      console.log(`   Checked: ${claudeProjectsDir}`);
      console.log('\n   This could mean:');
      console.log('   • You haven\'t used Claude Code yet');
      console.log('   • Claude stores conversations in a different location');
      console.log('   • Permissions issue accessing the directory');
      console.log('\n   The watcher will monitor for new conversations.');
      return;
    }
    
    console.log(`✅ Found ${totalJsonlFiles} conversation files across ${projectCount} projects`);
  } catch (e) {
    console.log(`⚠️  Could not access Claude projects directory: ${e.message}`);
  }
  
  // Check if baseline exists by looking for imported files state
  const configDir = path.join(os.homedir(), '.claude-self-reflect', 'config');
  const stateFile = path.join(configDir, 'imported-files.json');
  let hasBaseline = false;
  let needsMetadataMigration = false;
  
  try {
    if (fsSync.existsSync(stateFile)) {
      const state = JSON.parse(fsSync.readFileSync(stateFile, 'utf8'));
      hasBaseline = state.imported_files && Object.keys(state.imported_files).length > 0;
      
      // Check if any imported files are in old format (string timestamp vs object)
      if (hasBaseline) {
        for (const [file, data] of Object.entries(state.imported_files)) {
          if (typeof data === 'string') {
            needsMetadataMigration = true;
            break;
          }
        }
      }
    }
  } catch (e) {
    // State file doesn't exist or is invalid
  }
  
  if (!hasBaseline) {
    console.log('\n⚠️  No baseline detected. Initial import STRONGLY recommended.');
    console.log('   Without this, historical conversations won\'t be searchable.');
    console.log('   The watcher only handles NEW conversations going forward.');
  } else if (needsMetadataMigration) {
    console.log('\n🔄 Detected old import format. Metadata enhancement available!');
    console.log('   Re-importing will add file analysis, tool usage, and concept tracking.');
    console.log('   This enables advanced search features like search_by_file and search_by_concept.');
  }
  
  const answer = await question('\nImport existing Claude conversations? (y/n) [recommended: y]: ');
  
  if (answer.toLowerCase() === 'y') {
    console.log('🔄 Starting baseline import with metadata extraction...');
    console.log('   This ensures ALL your conversations are searchable');
    console.log('   Enhanced with tool usage tracking and file analysis');
    console.log('   This may take a few minutes depending on your conversation history');
    
    try {
      safeExec('docker', ['compose', 'run', '--rm', 'importer'], {
        cwd: projectRoot,
        stdio: 'inherit'
      });
      console.log('\n✅ Baseline import completed with metadata!');
      console.log('   Historical conversations are now searchable');
      console.log('   Tool usage and file analysis metadata extracted');
    } catch {
      console.log('\n⚠️  Import had some issues, but you can continue');
    }
  } else {
    console.log('\n❌ WARNING: Skipping baseline import means:');
    console.log('   • Historical conversations will NOT be searchable');
    console.log('   • Only NEW conversations from now on will be indexed');
    console.log('   • You may see "BASELINE_NEEDED" warnings in logs');
    console.log('\n📝 You can run baseline import later with:');
    console.log('   docker compose run --rm importer');
  }
}

async function enrichMetadata() {
  console.log('\n🔍 Metadata Enrichment (NEW in v2.5.19!)...');
  console.log('━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━');
  console.log('This feature enhances your conversations with searchable metadata:');
  console.log('   • Concepts: High-level topics (docker, security, testing, etc.)');
  console.log('   • Files: Track which files were analyzed or edited');
  console.log('   • Tools: Record which Claude tools were used');
  console.log('\nEnables powerful searches like:');
  console.log('   • search_by_concept("docker")');
  console.log('   • search_by_file("server.py")');
  
  const enrichChoice = await question('\nEnrich past conversations with metadata? (recommended) (y/n): ');
  
  if (enrichChoice.toLowerCase() === 'y') {
    console.log('\n⏳ Starting metadata enrichment (safe mode)...');
    console.log('   • Processing last 30 days of conversations');
    console.log('   • Using conservative rate limiting');
    console.log('   • This may take 5-10 minutes\n');
    
    try {
      // Run the safe delta update script
      safeExec('docker', [
        'compose', 'run', '--rm',
        '-e', 'DAYS_TO_UPDATE=30',
        '-e', 'BATCH_SIZE=2',
        '-e', 'RATE_LIMIT_DELAY=0.5',
        '-e', 'MAX_CONCURRENT_UPDATES=2',
        'importer',
        'python', '/app/scripts/delta-metadata-update-safe.py'
      ], {
        cwd: projectRoot,
        stdio: 'inherit'
      });
      
      console.log('\n✅ Metadata enrichment completed successfully!');
      console.log('   Your conversations now have searchable concepts and file tracking.');
    } catch (error) {
      console.log('\n⚠️  Metadata enrichment had some issues but continuing setup');
      console.log('   You can retry later with:');
      console.log('   docker compose run --rm importer python /app/scripts/delta-metadata-update-safe.py');
    }
  } else {
    console.log('\n📝 Skipping metadata enrichment.');
    console.log('   You can run it later with:');
    console.log('   docker compose run --rm importer python /app/scripts/delta-metadata-update-safe.py');
  }
}

async function startWatcher() {
  console.log('\n🔄 Starting the streaming watcher...');
  console.log('   • HOT files (<5 min): 2-second processing');
  console.log('   • WARM files (<24 hrs): Normal priority');
  console.log('   • COLD files (>24 hrs): Batch processing');
  
  try {
    safeExec('docker', ['compose', '--profile', 'watch', 'up', '-d', 'safe-watcher'], {
      cwd: projectRoot,
      stdio: 'inherit'
    });
    console.log('✅ Watcher started successfully!');
    return true;
  } catch (error) {
    console.log('⚠️  Could not start watcher automatically');
    console.log('   You can start it manually with: docker compose --profile watch up -d');
    return false;
  }
}

async function showFinalInstructions() {
  console.log('\n✅ Setup complete!');
  
  console.log('\n🎯 Your Claude Self-Reflect System:');
  console.log('   • 🌐 Qdrant Dashboard: http://localhost:6333/dashboard/');
  console.log('   • 📊 Status: All services running');
  console.log('   • 🔍 Search: Semantic search with memory decay enabled');
  console.log('   • 🚀 Watcher: HOT/WARM/COLD prioritization active');
  
  console.log('\n📋 Quick Reference Commands:');
  console.log('   • Check status: docker compose ps');
  console.log('   • View logs: docker compose logs -f');
  console.log('   • Import conversations: docker compose run --rm importer');
  console.log('   • Enrich metadata: docker compose run --rm importer python /app/scripts/delta-metadata-update-safe.py');
  console.log('   • Start watcher: docker compose --profile watch up -d');
  console.log('   • Stop all: docker compose down');
  
  console.log('\n🎯 Next Steps:');
  console.log('1. Restart Claude Code');
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
        
        // Offer metadata enrichment for v2.5.19
        console.log('\n🆕 NEW in v2.5.19: Metadata Enrichment!');
        console.log('   Enhance your conversations with searchable concepts and file tracking.');
        
        const upgradeChoice = await question('\nWould you like to enrich your conversations with metadata? (y/n): ');
        
        if (upgradeChoice.toLowerCase() === 'y') {
          await enrichMetadata();
          console.log('\n✅ Upgrade complete! Your conversations now have enhanced search capabilities.');
        }
        
        console.log('\n📋 Quick Commands:');
        console.log('   • View status: docker compose ps');
        console.log('   • View logs: docker compose logs -f');
        console.log('   • Enrich metadata: docker compose run --rm importer python /app/scripts/delta-metadata-update-safe.py');
        console.log('   • Restart: docker compose restart');
        console.log('   • Stop: docker compose down');
        
        console.log('\n💡 To re-run full setup, first stop services with: docker compose down');
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
  
  // Enrich metadata (new in v2.5.19)
  await enrichMetadata();
  
  // Start the watcher
  await startWatcher();
  
  // Show final instructions
  await showFinalInstructions();
  
  if (rl) rl.close();
}

main().catch(error => {
  console.error('❌ Setup failed:', error);
  if (rl) rl.close();
  process.exit(1);
});