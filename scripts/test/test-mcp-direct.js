#!/usr/bin/env node

import { spawn } from 'child_process';
import { createInterface } from 'readline';

// Configuration
const MCP_PATH = './claude-self-reflection/dist/index.js';
const TEST_QUERY = 'Voyage AI embeddings';

async function testMCP() {
  console.log('Starting MCP server...');
  
  // Check for required environment variable
  if (!process.env.VOYAGE_KEY) {
    console.error('Error: VOYAGE_KEY environment variable not set');
    console.error('Please set: export VOYAGE_KEY="your-voyage-api-key"');
    process.exit(1);
  }

  // Start the MCP server
  const mcp = spawn('node', [MCP_PATH], {
    env: {
      ...process.env,
      QDRANT_URL: 'http://localhost:6333',
      VOYAGE_KEY: process.env.VOYAGE_KEY
    },
    stdio: ['pipe', 'pipe', 'pipe']
  });

  const rl = createInterface({
    input: mcp.stdout,
    crlfDelay: Infinity
  });

  const errRl = createInterface({
    input: mcp.stderr,
    crlfDelay: Infinity
  });

  // Log stderr output
  errRl.on('line', (line) => {
    console.error('[MCP]', line);
  });

  // Wait for server to start
  await new Promise(resolve => setTimeout(resolve, 2000));

  // Send initialization request
  const initRequest = {
    jsonrpc: '2.0',
    method: 'initialize',
    params: {
      protocolVersion: '0.1.0',
      clientInfo: {
        name: 'test-client',
        version: '1.0.0'
      }
    },
    id: 1
  };

  console.log('Sending initialize request...');
  mcp.stdin.write(JSON.stringify(initRequest) + '\n');

  // Wait for response
  await new Promise(resolve => setTimeout(resolve, 1000));

  // Send search request
  const searchRequest = {
    jsonrpc: '2.0',
    method: 'tools/call',
    params: {
      name: 'reflect_on_past',
      arguments: {
        query: TEST_QUERY,
        limit: 5
      }
    },
    id: 2
  };

  console.log(`\nSearching for: "${TEST_QUERY}"...`);
  mcp.stdin.write(JSON.stringify(searchRequest) + '\n');

  // Listen for responses
  rl.on('line', (line) => {
    try {
      const response = JSON.parse(line);
      console.log('\nResponse:', JSON.stringify(response, null, 2));
    } catch (e) {
      console.log('Output:', line);
    }
  });

  // Keep process alive for a bit
  await new Promise(resolve => setTimeout(resolve, 5000));

  console.log('\nTest complete');
  mcp.kill();
  process.exit(0);
}

testMCP().catch(console.error);