# Architecture Details

For those who want to understand the plumbing.

## System Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     Claude Code/Desktop                         │
│                        (MCP Client)                             │
└────────────────────────────┬───────────────────────────────────┘
                             │ MCP Protocol
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│                       MCP Server                                │
│                    (Python FastMCP)                             │
│  ┌─────────────────┐        ┌──────────────────────┐          │
│  │ store_reflection │        │  reflect_on_past     │          │
│  └─────────────────┘        └──────────────────────┘          │
└────────────────────────────┬───────────────────────────────────┘
                             │
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│                    Qdrant Vector Database                       │
│  ┌─────────────────────────────────────────────────────────┐  │
│  │  Collections: conv_<project_hash>_local/voyage          │  │
│  │  - Conversation embeddings (384/1024 dims)             │  │
│  │  - Metadata (timestamp, project, context)               │  │
│  └─────────────────────────────────────────────────────────┘  │
└────────────────────────────┬───────────────────────────────────┘
                             │
                             ▼
┌─────────────────────────────────────────────────────────────────┐
│                     Import Pipeline                             │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────────┐  │
│  │  Read    │→ │  Chunk   │→ │ Embed    │→ │ Store in     │  │
│  │  JSONL   │  │  (500    │  │ (Local/  │  │ Qdrant       │  │
│  │  Files   │  │  tokens) │  │ Voyage)  │  │              │  │
│  └──────────┘  └──────────┘  └──────────┘  └──────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

The system consists of four main components:

### 1. Claude Code/Desktop
The MCP client that requests memory operations. This is your interface to the memory system.

### 2. MCP Server
Python-based service using FastMCP framework. Provides two simple tools:
- `store_reflection` - Store important insights and decisions
- `reflect_on_past` - Search through conversation history

### 3. Import Pipeline
Python service that processes conversation logs:
- Reads JSONL files from Claude conversation logs (`~/.claude/projects/*/conversations/`)
- Creates conversation chunks for context (500 tokens each)
- Generates embeddings using:
  - **Local (Default)**: FastEmbed with all-MiniLM-L6-v2 (384 dimensions)
  - **Cloud (Optional)**: Voyage AI voyage-3-large (1024 dimensions)
- Stores directly in Qdrant with metadata
- **Streaming Mode**: Real-time import with intelligent prioritization:
  - 🔥 HOT (< 5 min): 2-second intervals for immediate visibility
  - 🌡️ WARM (< 24 hr): Normal priority, 60-second intervals
  - ❄️ COLD (> 24 hr): Batch processing, max 5 per cycle

### 4. Qdrant Vector Database
- Stores conversation embeddings with metadata
- Provides fast semantic similarity search
- Built-in vector indexing and retrieval
- Local-first: your data stays on your machine
- Per-project collections for isolation

## Data Flow

### 1. Import Flow

```
~/.claude/projects/*/conversations/*.jsonl
                    │
                    ▼
            ┌──────────────┐
            │ Import Script│
            └──────┬───────┘
                   │
        ┌──────────┴──────────┐
        ▼                     ▼
┌─────────────────┐   ┌─────────────────┐
│ Parse JSONL     │   │ Extract         │
│ Messages        │   │ Metadata        │
└────────┬────────┘   └────────┬────────┘
         │                     │
         └──────────┬──────────┘
                    ▼
           ┌──────────────┐
           │ Chunk Text   │
           │ (500 tokens) │
           └──────┬───────┘
                  ▼
           ┌──────────────┐
           │ Generate     │
           │ Embeddings   │
           │(Local/Cloud) │
           └──────┬───────┘
                  ▼
           ┌──────────────┐
           │ Store in     │
           │ Qdrant       │
           └──────────────┘
```

### 2. Search Flow

```
"What did we discuss about authentication?"
                    │
                    ▼
            ┌──────────────┐
            │ MCP Server   │
            │ reflect_on_  │
            │ past()       │
            └──────┬───────┘
                   ▼
            ┌──────────────┐
            │ Generate     │
            │ Query        │
            │ Embedding    │
            └──────┬───────┘
                   ▼
            ┌──────────────┐
            │ Search       │
            │ Qdrant       │
            │ (Similarity) │
            └──────┬───────┘
                   ▼
            ┌──────────────┐
            │ Return Top   │
            │ K Results    │
            └──────┬───────┘
                   ▼
            ┌──────────────┐
            │ Format &     │
            │ Present to   │
            │ Claude       │
            └──────────────┘
```

## Project Structure

```
claude-self-reflect/
├── mcp-server/           # Python MCP server using FastMCP
│   ├── src/              # Server source code
│   │   ├── server.py     # Main MCP server implementation
│   │   └── server_v2.py  # Enhanced version with better error handling
│   ├── pyproject.toml    # Python package configuration
│   └── run-mcp.sh        # MCP startup script
├── scripts/              # Import and utility scripts
│   ├── import-*.py       # Various import scripts for conversations
│   └── test-*.py         # Test scripts for features
├── .claude/agents/       # Claude sub-agents for specialized tasks
├── config/               # Configuration files
├── data/                 # Qdrant vector database storage
└── docs/                 # Documentation and guides
```

## Technical Specifications

### Embeddings

#### Local Mode (Default)
- **Model**: FastEmbed all-MiniLM-L6-v2
- **Dimensions**: 384
- **Context**: 500 tokens per chunk
- **Cost**: Free (runs locally)
- **Privacy**: All processing on your machine

#### Cloud Mode (Optional)
- **Model**: Voyage AI voyage-3-large
- **Dimensions**: 1024
- **Context**: 500 tokens per chunk
- **Free tier**: 200M tokens/month
- **Accuracy**: Better semantic search

### Vector Database
- **Engine**: Qdrant
- **Storage**: Local disk
- **Collections**: Per-project isolation
- **Naming**: 
  - Local: `conv_<md5_hash>_local`
  - Cloud: `conv_<md5_hash>_voyage`
  - Hash: MD5 of project directory path for unique identification

### Performance
- **Search latency**: 200-350ms end-to-end (10-40x faster than v2.4.4)
- **Import speed**: 
  - HOT: 2-second response for new conversations
  - Batch: ~1000 conversations/minute
- **Memory overhead**: 50MB operational target with smart chunking

## Security & Privacy

- All data stored locally
- No cloud dependencies for core functionality
- Local mode (default): No external API calls
- Cloud mode: API keys only used for embedding generation
- Conversations stored locally in both modes

## Additional Details

### Import Process Workflow

```
┌─────────────────────────────────────────────────────────┐
│                  Import Process                         │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  1. Scan ~/.claude/projects/*/conversations/           │
│     └─> Find all .jsonl files                         │
│                                                         │
│  2. For each JSONL file:                              │
│     ├─> Parse conversation messages                    │
│     ├─> Extract project metadata                       │
│     └─> Track processing progress                      │
│                                                         │
│  3. Chunk conversations:                               │
│     ├─> Split into 500-token segments                  │
│     ├─> Preserve context boundaries                    │
│     └─> Add overlap for continuity                     │
│                                                         │
│  4. Generate embeddings:                               │
│     ├─> Local: Process with FastEmbed (no API)         │
│     ├─> Cloud: Batch API calls to Voyage AI            │
│     └─> Handle rate limits (cloud mode only)           │
│                                                         │
│  5. Store in Qdrant:                                   │
│     ├─> Create/update collection                       │
│     ├─> Insert vectors with metadata                   │
│     └─> Build index for fast retrieval                 │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

### Semantic Search Operation

```
┌─────────────────────────────────────────────────────────┐
│              Semantic Search Process                    │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  Query: "authentication discussions"                   │
│    │                                                    │
│    ▼                                                    │
│  ┌─────────────────────────────────────┐              │
│  │ 1. Preprocess Query                 │              │
│  │    - Normalize text                 │              │
│  │    - Extract key concepts           │              │
│  └──────────────┬──────────────────────┘              │
│                 ▼                                       │
│  ┌─────────────────────────────────────┐              │
│  │ 2. Generate Query Embedding         │              │
│  │    - Voyage AI API call             │              │
│  │    - 1024-dimensional vector        │              │
│  └──────────────┬──────────────────────┘              │
│                 ▼                                       │
│  ┌─────────────────────────────────────┐              │
│  │ 3. Vector Similarity Search         │              │
│  │    - Cosine similarity              │              │
│  │    - Cross-collection search        │              │
│  │    - Top-K retrieval                │              │
│  └──────────────┬──────────────────────┘              │
│                 ▼                                       │
│  ┌─────────────────────────────────────┐              │
│  │ 4. Post-process Results             │              │
│  │    - Rank by relevance              │              │
│  │    - Deduplicate                    │              │
│  │    - Format for presentation        │              │
│  └──────────────┬──────────────────────┘              │
│                 ▼                                       │
│  Results: Relevant conversation chunks                  │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

## See Also

- [Components Guide](components.md) - Deep dive into each component