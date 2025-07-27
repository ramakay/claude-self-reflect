#!/usr/bin/env node

import { spawn } from 'child_process';
import { createInterface } from 'readline';

// Set environment variables
const env = {
  ...process.env,
  VOYAGE_KEY: process.env.VOYAGE_KEY || '',
  QDRANT_URL: 'http://localhost:6333',
  PREFER_LOCAL_EMBEDDINGS: 'false'
};

if (!env.VOYAGE_KEY) {
  console.error('Error: VOYAGE_KEY environment variable not set');
  console.error('Please set: export VOYAGE_KEY="your-voyage-api-key"');
  process.exit(1);
}

console.log('Starting MCP server with environment:');
console.log('VOYAGE_KEY:', env.VOYAGE_KEY ? 'Set' : 'Not set');
console.log('QDRANT_URL:', env.QDRANT_URL);

// Start the MCP server
const mcp = spawn('node', [
  '/Users/ramakrishnanannaswamy/claude-self-reflect/qdrant-mcp-stack/claude-self-reflection/dist/index.js'
], { env });

// Handle stderr (where debug logs go)
mcp.stderr.on('data', (data) => {
  console.error('MCP Debug:', data.toString());
});

// Create readline interface for communication
const rl = createInterface({
  input: mcp.stdout,
  crlfDelay: Infinity
});

let messageBuffer = '';

rl.on('line', (line) => {
  if (line.trim()) {
    messageBuffer += line + '\n';
    
    // Try to parse as complete JSON-RPC message
    try {
      const message = JSON.parse(messageBuffer);
      console.log('Received:', JSON.stringify(message, null, 2));
      messageBuffer = '';
      
      // If we got the initialization response, send a tools list request
      if (message.result && message.result.protocolVersion) {
        console.log('\nSending tools list request...');
        const toolsRequest = {
          jsonrpc: '2.0',
          id: 2,
          method: 'tools/list',
          params: {}
        };
        mcp.stdin.write(JSON.stringify(toolsRequest) + '\n');
      }
    } catch (e) {
      // Not a complete message yet
    }
  }
});

// Send initialization request
console.log('Sending initialization request...');
const initRequest = {
  jsonrpc: '2.0',
  id: 1,
  method: 'initialize',
  params: {
    protocolVersion: '2024-11-05',
    capabilities: {},
    clientInfo: {
      name: 'test-client',
      version: '1.0.0'
    }
  }
};

mcp.stdin.write(JSON.stringify(initRequest) + '\n');

// Handle process exit
mcp.on('close', (code) => {
  console.log(`MCP server exited with code ${code}`);
  process.exit(0);
});

// Give it 5 seconds to respond
setTimeout(() => {
  console.log('\nTimeout - closing connection');
  mcp.kill();
}, 5000);