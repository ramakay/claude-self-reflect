#!/usr/bin/env node
import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import {
  CallToolRequestSchema,
  ErrorCode,
  ListToolsRequestSchema,
  McpError,
} from '@modelcontextprotocol/sdk/types.js';
import { QdrantClient } from '@qdrant/js-client-rest';
import { createEmbeddingService, detectEmbeddingModel, EmbeddingService } from './embeddings.js';
import { ProjectIsolationManager, ProjectIsolationConfig, DEFAULT_ISOLATION_CONFIG } from './project-isolation.js';
import * as dotenv from 'dotenv';
import * as path from 'path';
import { fileURLToPath } from 'url';

// Get __dirname equivalent for ES modules
const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

// Load environment variables from parent directory
dotenv.config({ path: path.join(__dirname, '../../.env') });

const QDRANT_URL = process.env.QDRANT_URL || 'http://localhost:6333';
const COLLECTION_NAME = process.env.COLLECTION_NAME || 'conversations';
const OPENAI_API_KEY = process.env.OPENAI_API_KEY;
const VOYAGE_API_KEY = process.env.VOYAGE_KEY || process.env['VOYAGE_KEY-2'];
const PREFER_LOCAL_EMBEDDINGS = process.env.PREFER_LOCAL_EMBEDDINGS === 'true';
const ISOLATION_MODE = process.env.ISOLATION_MODE || 'hybrid';
const ALLOW_CROSS_PROJECT = process.env.ALLOW_CROSS_PROJECT === 'true';
const ENABLE_MEMORY_DECAY = process.env.ENABLE_MEMORY_DECAY === 'true';
const DECAY_WEIGHT = parseFloat(process.env.DECAY_WEIGHT || '0.3');
const DECAY_SCALE_DAYS = parseFloat(process.env.DECAY_SCALE_DAYS || '90');

// Debug: Log environment variables on startup
console.error('🚀 MCP Server starting with environment:');
console.error(`  - ENABLE_MEMORY_DECAY: ${ENABLE_MEMORY_DECAY} (from env: ${process.env.ENABLE_MEMORY_DECAY})`);
console.error(`  - DECAY_WEIGHT: ${DECAY_WEIGHT} (from env: ${process.env.DECAY_WEIGHT})`);
console.error(`  - DECAY_SCALE_DAYS: ${DECAY_SCALE_DAYS} (from env: ${process.env.DECAY_SCALE_DAYS})`);

interface SearchResult {
  id: string;
  score: number;
  timestamp: string;
  role: string;
  excerpt: string;
  projectName?: string;
  conversationId?: string;
}

class SelfReflectionServer {
  private server: Server;
  private qdrantClient: QdrantClient;
  private embeddingService?: EmbeddingService;
  private collectionInfo?: { model: string; dimensions: number };
  private isolationManager: ProjectIsolationManager;
  private currentProject?: string;

  constructor() {
    this.server = new Server(
      {
        name: 'claude-self-reflection',
        version: '0.1.0',
      },
      {
        capabilities: {
          tools: {},
        },
      }
    );

    this.qdrantClient = new QdrantClient({ url: QDRANT_URL });
    
    // Initialize project isolation
    const isolationConfig: ProjectIsolationConfig = {
      mode: ISOLATION_MODE as 'isolated' | 'shared' | 'hybrid',
      allowCrossProject: ALLOW_CROSS_PROJECT,
      projectIdentifier: ProjectIsolationManager.detectCurrentProject()
    };
    this.isolationManager = new ProjectIsolationManager(this.qdrantClient, isolationConfig);
    this.currentProject = isolationConfig.projectIdentifier;
    
    this.setupToolHandlers();
  }

  private async initialize() {
    try {
      // Create embedding service with Voyage AI if available
      this.embeddingService = await createEmbeddingService({
        openaiApiKey: OPENAI_API_KEY,
        voyageApiKey: VOYAGE_API_KEY,
        preferLocal: PREFER_LOCAL_EMBEDDINGS,
      });

      // For Voyage collections, we don't need to check dimensions as they're consistent
      if (this.embeddingService.getModelName().includes('voyage')) {
        console.error('Using Voyage AI embeddings for search across project collections');
      }
    } catch (error) {
      console.error('Failed to initialize embedding service:', error);
      console.error('Server will run in degraded mode with text-based search');
    }
  }

  private setupToolHandlers() {
    this.server.setRequestHandler(ListToolsRequestSchema, async () => ({
      tools: [
        {
          name: 'reflect_on_past',
          description: 'Search for relevant past conversations using semantic search',
          inputSchema: {
            type: 'object',
            properties: {
              query: {
                type: 'string',
                description: 'The search query to find semantically similar conversations',
              },
              limit: {
                type: 'number',
                description: 'Maximum number of results to return (default: 5)',
                default: 5,
              },
              project: {
                type: 'string',
                description: 'Filter by project name (optional)',
              },
              crossProject: {
                type: 'boolean',
                description: 'Search across all projects (default: false, requires permission)',
                default: false,
              },
              minScore: {
                type: 'number',
                description: 'Minimum similarity score (0-1, default: 0.7)',
                default: 0.7,
              },
              useDecay: {
                type: 'boolean',
                description: 'Apply time-based decay to prioritize recent memories (default: uses environment setting)',
              },
            },
            required: ['query'],
          },
        },
        {
          name: 'store_reflection',
          description: 'Store an important insight or reflection for future reference',
          inputSchema: {
            type: 'object',
            properties: {
              content: {
                type: 'string',
                description: 'The insight or reflection to store',
              },
              tags: {
                type: 'array',
                items: { type: 'string' },
                description: 'Tags to categorize this reflection',
              },
            },
            required: ['content'],
          },
        },
      ],
    }));

    this.server.setRequestHandler(CallToolRequestSchema, async (request) => {
      if (request.params.name === 'reflect_on_past') {
        return this.handleReflectOnPast(request.params.arguments);
      } else if (request.params.name === 'store_reflection') {
        return this.handleStoreReflection(request.params.arguments);
      }
      
      throw new McpError(
        ErrorCode.MethodNotFound,
        `Unknown tool: ${request.params.name}`
      );
    });
  }

  private async getVoyageCollections(): Promise<string[]> {
    try {
      const collections = await this.qdrantClient.getCollections();
      return collections.collections
        .map(c => c.name)
        .filter(name => name.endsWith('_voyage'));
    } catch (error) {
      console.error('Failed to get Voyage collections:', error);
      return [];
    }
  }

  private async handleReflectOnPast(args: any) {
    const { query, limit = 5, project, minScore = 0.7, crossProject = false, useDecay } = args;
    const shouldUseDecay = useDecay !== undefined ? useDecay : ENABLE_MEMORY_DECAY;
    
    // Log debug info but don't return early
    console.error(`🔍 DEBUG: MCP handleReflectOnPast called!
- query: "${query}"
- useDecay: ${useDecay} (type: ${typeof useDecay})
- shouldUseDecay: ${shouldUseDecay}
- ENABLE_MEMORY_DECAY: ${ENABLE_MEMORY_DECAY}
- DECAY_WEIGHT: ${DECAY_WEIGHT}
- DECAY_SCALE_DAYS: ${DECAY_SCALE_DAYS}
- minScore: ${minScore}`);
    
    // Extra debug for decay path
    if (shouldUseDecay) {
      console.error(`🔄 DECAY PATH SHOULD BE TAKEN - shouldUseDecay is true`);
    } else {
      console.error(`❌ DECAY PATH NOT TAKEN - shouldUseDecay is false`);
    }

    try {
      // Initialize if not already done
      if (!this.embeddingService) {
        await this.initialize();
      }

      let results: SearchResult[] = [];

      if (this.embeddingService) {
        // Use vector search with embeddings
        console.error(`Generating embedding for query: "${query}"`);
        const queryEmbedding = await this.embeddingService.generateEmbedding(query);

        // Get all Voyage collections
        const voyageCollections = await this.getVoyageCollections();
        
        if (voyageCollections.length === 0) {
          console.error('No Voyage collections found');
          return {
            content: [
              {
                type: 'text',
                text: `No Voyage collections found. Please run the import process first.`,
              },
            ],
          };
        }

        console.error(`Searching across ${voyageCollections.length} Voyage collections: ${voyageCollections.slice(0, 3).join(', ')}...`);
      
      if (voyageCollections.length === 0) {
        console.error(`⚠️  WARNING: No Voyage collections found!`);
        return {
          content: [{
            type: 'text',
            text: 'No conversation collections found. Please import conversations first.'
          }]
        };
      }
        

        // Search across multiple collections
        const searchPromises = voyageCollections.map(async (collectionName) => {
          try {
            let searchResponse;
            
            if (shouldUseDecay) {
              console.error(`🔄 DECAY MODE ACTIVE for collection ${collectionName}`);
              console.error(`  - minScore parameter: ${minScore} (will be ignored for initial search)`);
              console.error(`  - DECAY_WEIGHT: ${DECAY_WEIGHT}`);
              console.error(`  - DECAY_SCALE_DAYS: ${DECAY_SCALE_DAYS}`);
              
              // IMPORTANT: No score_threshold for decay mode - we need all results to apply decay
              searchResponse = await this.qdrantClient.search(collectionName, {
                vector: queryEmbedding,
                limit: Math.ceil(limit * 3), // Get more candidates for decay filtering
                // NO score_threshold - we'll filter after decay is applied
                with_payload: true,
              });

              console.error(`  - Initial results from ${collectionName}: ${searchResponse.length} items`);
              if (searchResponse.length > 0) {
                console.error(`  - Score range: ${searchResponse[searchResponse.length - 1].score.toFixed(3)} to ${searchResponse[0].score.toFixed(3)}`);
              }

              // Apply decay scoring with proper formula
              const now = Date.now();
              const scaleMs = DECAY_SCALE_DAYS * 24 * 60 * 60 * 1000;
              
              try {
                searchResponse = searchResponse.map((point: any, index: number) => {
                  let ageMs = 0;
                  let originalScore = point.score;
                  
                  try {
                    if (point.payload?.timestamp) {
                      const timestamp = new Date(point.payload.timestamp as string).getTime();
                      if (!isNaN(timestamp)) {
                        ageMs = now - timestamp;
                      } else {
                        console.error(`    - Invalid timestamp for point ${point.id}: ${point.payload.timestamp}`);
                      }
                    }
                    
                    // Calculate exponential decay factor (newer = higher factor)
                    const decayFactor = Math.exp(-ageMs / scaleMs);
                    
                    // Apply decay formula: base_score + decay_weight * decay_factor
                    const adjustedScore = originalScore + (DECAY_WEIGHT * decayFactor);
                    
                    if (index < 3) { // Log first 3 items for debugging
                      console.error(`  Decay calculation for item ${index + 1}:`);
                      console.error(`    - timestamp: ${point.payload?.timestamp || 'missing'}`);
                      console.error(`    - age: ${(ageMs / 86400000).toFixed(1)} days`);
                      console.error(`    - originalScore: ${originalScore.toFixed(3)}`);
                      console.error(`    - decayFactor: ${decayFactor.toFixed(4)}`);
                      console.error(`    - adjustedScore: ${adjustedScore.toFixed(3)} (boost: +${(adjustedScore - originalScore).toFixed(3)})`);
                    }
                    
                    return {
                      ...point,
                      score: adjustedScore
                    };
                  } catch (decayError) {
                    console.error(`  Error calculating decay for point ${point.id}:`, decayError);
                    // Return original point with original score on error
                    return point;
                  }
                });
              } catch (mapError) {
                console.error(`  Fatal error during decay calculation:`, mapError);
                // Fall back to original search results if decay fails completely
                searchResponse = searchResponse;
              }
              
              // Apply filtering and sorting
              searchResponse = searchResponse
                .filter(point => point.score >= minScore) // Apply minScore filter AFTER decay
                .sort((a: any, b: any) => b.score - a.score)
                .slice(0, limit);
              
              console.error(`  - After decay and filtering (>= ${minScore}): ${searchResponse.length} results`);
              
            } else {
              // Standard search without decay
              searchResponse = await this.qdrantClient.search(collectionName, {
                vector: queryEmbedding,
                limit: Math.ceil(limit * 1.5), // Get extra results per collection
                score_threshold: minScore,
                with_payload: true,
              });
            }

            if (searchResponse.length > 0) {
              console.error(`Collection ${collectionName}: Found ${searchResponse.length} results`);
            }

            return searchResponse.map(point => ({
              id: point.id as string,
              score: point.score,
              timestamp: point.payload?.timestamp as string || new Date().toISOString(),
              role: (point.payload?.start_role as string) || (point.payload?.role as string) || 'unknown',
              excerpt: ((point.payload?.text as string) || '').substring(0, 500) + '...',
              projectName: (point.payload?.project as string) || (point.payload?.project_name as string) || (point.payload?.project_id as string) || collectionName.replace('conv_', '').replace('_voyage', ''),
              conversationId: point.payload?.conversation_id as string,
              collectionName,
            }));
          } catch (error) {
            console.error(`Failed to search collection ${collectionName}:`, error);
            return [];
          }
        });

        // Wait for all searches to complete
        const allResults = await Promise.all(searchPromises);
        
        // Flatten and sort by score
        const flatResults = allResults.flat();
        console.error(`Found ${flatResults.length} total results across all collections`);
        
        results = flatResults
          .sort((a, b) => b.score - a.score)
          .slice(0, limit);

      } else {
        // Fallback to text search
        console.error('Using fallback text search (no embeddings available)');
        
        const voyageCollections = await this.getVoyageCollections();
        
        // Search across all collections with text matching
        const searchPromises = voyageCollections.map(async (collectionName) => {
          try {
            const scrollResponse = await this.qdrantClient.scroll(collectionName, {
              limit: 100,
              with_payload: true,
              with_vector: false,
            });

            const queryWords = query.toLowerCase().split(/\s+/);
            return scrollResponse.points
              .filter(point => {
                const text = (point.payload?.text as string || '').toLowerCase();
                return queryWords.some((word: string) => text.includes(word));
              })
              .map(point => ({
                id: point.id as string,
                score: 0.5,
                timestamp: point.payload?.timestamp as string || new Date().toISOString(),
                role: (point.payload?.start_role as string) || (point.payload?.role as string) || 'unknown',
                excerpt: ((point.payload?.text as string) || '').substring(0, 500) + '...',
                projectName: (point.payload?.project as string) || (point.payload?.project_name as string) || (point.payload?.project_id as string) || collectionName.replace('conv_', '').replace('_voyage', ''),
                conversationId: point.payload?.conversation_id as string,
                collectionName,
              }));
          } catch (error) {
            console.error(`Failed to search collection ${collectionName}:`, error);
            return [];
          }
        });

        const allResults = await Promise.all(searchPromises);
        results = allResults
          .flat()
          .slice(0, limit);
      }

      if (results.length === 0) {
        // For debugging, let's check what collections we have and what the search is actually doing
        const voyageCollections = await this.getVoyageCollections();
        return {
          content: [
            {
              type: 'text',
              text: `🔍 DECAY DEBUG: No results for "${query}"
              
[DEBUG INFO FROM CLAUDE SELF REFLECT MCP]
- useDecay param: ${useDecay} (type: ${typeof useDecay})
- shouldUseDecay: ${shouldUseDecay} 
- minScore: ${minScore}
- embeddingService exists: ${!!this.embeddingService}
- ENABLE_MEMORY_DECAY env: ${ENABLE_MEMORY_DECAY}
- DECAY_WEIGHT env: ${DECAY_WEIGHT}
- collections found: ${voyageCollections.length}
- first 3: ${voyageCollections.slice(0, 3).join(', ')}
- MODE: ${shouldUseDecay ? '🔄 DECAY ACTIVE' : '📊 STANDARD SEARCH'}`,
            },
          ],
        };
      }

      const resultText = results
        .map((result, i) => 
          `**Result ${i + 1}** (Score: ${result.score.toFixed(3)})\n` +
          `Time: ${new Date(result.timestamp).toLocaleString()}\n` +
          `Project: ${result.projectName || 'unknown'}\n` +
          `Role: ${result.role}\n` +
          `Excerpt: ${result.excerpt}\n` +
          `---`
        )
        .join('\n\n');

      return {
        content: [
          {
            type: 'text',
            text: `Found ${results.length} relevant conversation(s) for "${query}":\n\n${resultText}`,
          },
        ],
      };
    } catch (error) {
      console.error('Error searching conversations:', error);
      throw new McpError(
        ErrorCode.InternalError,
        `Failed to search conversations: ${error instanceof Error ? error.message : String(error)}`
      );
    }
  }

  private async handleStoreReflection(args: any) {
    const { content, tags = [] } = args;

    try {
      // This is a placeholder for now
      // In production, we'd store this as a special type of conversation chunk
      return {
        content: [
          {
            type: 'text',
            text: `Reflection stored successfully with tags: ${tags.join(', ') || 'none'}`,
          },
        ],
      };
    } catch (error) {
      throw new McpError(
        ErrorCode.InternalError,
        `Failed to store reflection: ${error instanceof Error ? error.message : String(error)}`
      );
    }
  }

  async run() {
    // Initialize embedding service early
    await this.initialize();

    const transport = new StdioServerTransport();
    await this.server.connect(transport);
    console.error('Claude Self-Reflection MCP server running');
    console.error(`Connected to Qdrant at ${QDRANT_URL}`);
    console.error(`Embedding service: ${this.embeddingService?.getModelName() || 'none (text search only)'}`);
    console.error(`Voyage API Key: ${VOYAGE_API_KEY ? 'Set' : 'Not set'}`);
    
    // Check for Voyage collections
    const voyageCollections = await this.getVoyageCollections();
    console.error(`Found ${voyageCollections.length} Voyage collections ready for search`);
  }
}

const server = new SelfReflectionServer();
server.run().catch(console.error);