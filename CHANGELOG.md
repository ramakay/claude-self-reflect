# Changelog

All notable changes to Claude Self-Reflect will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.4.4] - 2025-07-29

### Added
- XML-structured response format for reflection-specialist agent
  - All search results now return structured XML instead of markdown
  - Consistent error handling with XML format for all failure cases
  - Metadata section includes search performance metrics
  - Clear separation of summary, results, analysis, and metadata sections

### Changed
- Reflection-specialist agent completely restructured to use XML format
  - Easier parsing for main agent without regex
  - Structured data extraction with predictable field locations
  - Error responses also follow XML format for consistency

### Benefits
- Main agent can extract specific fields like `<score>`, `<project>`, `<timestamp>` directly
- No ambiguity in data boundaries or formatting
- Extensible schema allows adding new fields without breaking parsers
- Better integration capabilities for downstream tools

## [2.4.3] - 2025-07-28

### Added
- Project-scoped search functionality in MCP server (#14)
  - New optional `project` parameter for `reflect_on_past` tool
  - Default behavior: searches only current project based on working directory
  - Cross-project search with `project="all"`
  - Specific project search by name
- Comprehensive project-scoped search documentation (docs/project-scoped-search.md)
- Proactive cross-project search suggestions in reflection-specialist agent
- Project search troubleshooting section in docs
- Cross-project search strategies in advanced usage guide

### Changed
- **BREAKING**: Search behavior now defaults to project-scoped instead of searching all projects
  - Previous behavior (search all) now requires explicit `project="all"`
  - Improves focus, relevance, and performance (~100ms faster)
  - Project isolation enhances privacy between different work contexts
- Project names in search use folder names instead of full paths with dashes
- Reflection-specialist agent now indicates which project was searched

### Documentation
- Added detailed project-scoped search section to README with migration notes
- Created comprehensive guide at docs/project-scoped-search.md
- Updated reflection-specialist agent with proactive search patterns
- Enhanced advanced usage guide with cross-project strategies
- Added troubleshooting section for common project search issues

### Migration Notes
- Users upgrading from v2.4.2 or earlier will experience different search behavior
- To restore old behavior, explicitly request "search all projects" in queries
- Existing conversations remain searchable but are now filtered by project
- See [Project-Scoped Search Guide](docs/project-scoped-search.md) for details

## [2.4.2] - 2025-07-28

### Added
- Docker volume migration from bind mounts for better data persistence (PR #16)
- Exponential backoff for Voyage AI API calls with retry logic (PR #15)
- New reflect-tester agent for comprehensive system validation
- Performance baseline documentation for both embedding modes
- Tenacity library (9.1.2) for safe retry handling

### Changed
- Updated backup/restore scripts to work with Docker named volumes
- Enhanced testing infrastructure with phased approach
- Pinned all dependencies to specific versions for reproducible builds
- Fixed missing voyageai dependency in scripts/requirements.txt

### Fixed
- Agent documentation missing reflection-specialist
- Voyage AI import failures in Docker setup
- Collection naming for test projects
- Security scan false positives from accidentally committed SARIF file

### Security
- No vulnerabilities found in pip-audit scan
- All dependencies now pinned to specific versions

### Documentation
- Clarified that per-project isolation already exists (#14)
- Added workaround for npm global install path issues (#13)

## [2.4.0] - 2025-07-28

### Added
- Gitleaks configuration (`.gitleaks.toml`) for better CI/CD security scanning
- Support for handling false positives in security scans

### Changed
- Improved documentation clarity around privacy and security
- Removed unnecessary security alerts from README

### Security
- Enhanced CI/CD pipeline with proper secret scanning configuration
- Better handling of historical commits in security scans

## [2.3.7] - 2025-07-27

### Security
- Major security cleanup to reduce attack surface
- Removed archived TypeScript MCP implementation (31 files, no longer needed)
- Removed 70+ internal scripts and test files from git tracking
- Removed binary database directories (qdrant_storage/, data/) from git
- Set secure permissions (600) on .env configuration files
- Reduced codebase by ~17,000 lines and 250+ files

### Changed
- Updated .gitignore to prevent exposure of sensitive files and internal tools
- Moved internal scripts to untracked directories
- Kept only essential scripts needed for setup and validation

## [2.3.6] - 2025-07-27

### Changed
- Updated README "After" example to show actual reflection specialist sub-agent format
- Added explanation that reflection specialist is automatically spawned
- Emphasized local-first approach with optional cloud enhancement

### Documentation
- Improved clarity on how sub-agents appear in Claude's interface
- Better example using FastEmbed vs cloud decision instead of generic JWT auth

## [2.3.5] - 2025-07-27

### Changed
- Made technical documentation more neutral between local and cloud embedding options
- Removed promotional language about Voyage AI's cost-effectiveness
- Presented both embedding options equally without bias

## [2.3.4] - 2025-07-27

### Added
- Comprehensive embedding mode documentation
- Migration guide for switching between local and cloud modes
- Enhanced setup wizard with prominent embedding choice warnings
- Confirmation prompt for embedding mode selection

### Changed
- Setup wizard now clearly explains that embedding choice is semi-permanent
- Updated all documentation to emphasize the complexity of switching modes
- Improved troubleshooting guide with embedding mode issues section

### Documentation
- Created detailed embedding migration guide
- Updated installation guide with embedding mode selection
- Enhanced advanced usage guide with technical embedding details
- Added warnings throughout docs about mode switching implications

## [2.3.3] - 2025-07-27

### 🔒 Security Release

### Changed
- **BREAKING**: Complete migration from sentence-transformers to FastEmbed for local embeddings
- **Default Mode**: Local embeddings now default for privacy (no external API calls)
- **Docker Memory**: Increased container memory limits to 2GB for stability
- **Security Improvements**:
  - Fixed command injection vulnerabilities in installer
  - Patched vulnerable dependencies (pydantic CVE-2024-3772)
  - Enhanced configuration security

### Added
- **Local Embeddings by Default**: Uses FastEmbed with sentence-transformers/all-MiniLM-L6-v2 model
- **Unified Import System**: Single import script supports both local and cloud embeddings
- **JSONL Parser Fix**: Proper line-by-line parsing for Claude conversation files
- **Enhanced Documentation**:
  - Privacy mode comparison table in README
  - Security update notice with GitHub info block
  - Clear warnings about data exchange in cloud mode

### Fixed
- Import failures due to incorrect JSON parsing (JSONL format)
- Memory exhaustion in Docker containers when processing large files
- MCP server initialization with local embeddings
- Reflection specialist agent support for both embedding modes

### Security
- Environment variable configuration for all sensitive settings
- Local-first approach for privacy-conscious users
- Enhanced security scanning in CI/CD pipeline

## [2.3.0] - Unreleased

### Added
- **Setup Wizard Command-Line Arguments** - Non-interactive installation support
  - `--voyage-key=<key>` for direct API key configuration
  - `--local` flag for local-only mode without semantic search
  - Automatic detection of non-interactive environments (no TTY)
- **Watcher Integration** - Continuous conversation monitoring built into setup
  - Automatic file watching after initial import
  - Real-time conversation indexing
  - Configurable watch intervals
- **System Health Dashboard** - Comprehensive health monitoring (`health-check.sh`)
  - Qdrant status with vector and collection counts
  - MCP server connection status
  - Import queue and last import timing
  - Docker container resource usage
  - Automatic update checking
  - Search performance metrics
- **Enhanced Docker Support**
  - Isolated importer container (`Dockerfile.importer-isolated`)
  - Streaming importer with progress updates (`Dockerfile.streaming-importer`)
  - Watcher container for continuous monitoring (`Dockerfile.watcher`)
  - Optimized docker-compose configurations
- **Python SSL Support** - Improved handling of Python installations
  - Automatic detection of pyenv SSL issues
  - Fallback to Homebrew Python on macOS
  - Clear error messages and remediation steps

### Changed
- **Setup Wizard Improvements**
  - Better error handling for missing dependencies
  - Clearer prompts and user feedback
  - Support for both interactive and non-interactive modes
  - Automatic Python path detection and configuration
- **Documentation Structure**
  - Reorganized docs into logical categories
  - Added troubleshooting guides for common issues
  - Improved installation instructions with platform-specific notes
  - Better examples and use cases
- **Import Process**
  - Streaming import with real-time progress
  - Better handling of large conversation files
  - Improved error recovery and retry logic
  - More efficient vector embedding batch processing

### Fixed
- **Reddit User Feedback Issues**
  - Python SSL module errors with pyenv installations
  - Non-interactive installation failures in CI/CD environments
  - Qdrant health check endpoint (changed from `/health` to `/`)
  - Docker container permission issues on Linux
  - Memory leaks in long-running import processes
- **CI/CD Pipeline**
  - Updated workflows for Python-based structure
  - Fixed test paths and dependencies
  - Added Python version matrix testing (3.10, 3.11, 3.12)
- **Setup Wizard Bugs**
  - Fixed readline interface errors in non-TTY environments
  - Corrected Python path detection on various platforms
  - Improved error messages for missing dependencies

### Security
- Better isolation of import processes using Docker
- Improved handling of sensitive configuration

## [2.2.1] - 2025-01-25

### Fixed
- NPM package installation issues
- Missing dependencies in package.json

## [2.2.0] - 2025-01-24

### Added
- Voyage AI streaming embeddings support
- Improved import performance (2x faster)
- Better error handling in setup wizard

### Changed
- Default to Voyage AI for better search accuracy
- Simplified installation process

## [2.1.0] - 2025-01-23

### Added
- Support for multiple embedding providers
- Local embedding model option
- Cross-project search functionality

### Fixed
- Memory usage optimization for large conversation sets
- Import speed improvements

## [2.0.1] - 2025-01-22

### Fixed
- CI/CD workflow for Python-based structure
- Updated test configurations
- Fixed directory references in workflows

## [2.0.0] - 2025-01-22

### Changed
- **BREAKING**: Complete restructure from TypeScript to Python MCP server
- **BREAKING**: NPM package now serves as installation wizard only
- **BREAKING**: Renamed MCP from `claude-self-reflection` to `claude-self-reflect`
- Archived TypeScript implementation to `archived/typescript-mcp/`
- Renamed directories for clarity:
  - `claude-reflect` → `mcp-server`
  - `claude_reflect` → `src`
- Simplified configuration structure

### Added
- New installation CLI with interactive setup wizard
- `claude-self-reflect doctor` command for diagnostics
- Python wheel distribution for MCP server
- Comprehensive migration guide

### Removed
- TypeScript MCP server implementation (archived)
- Direct MCP functionality from NPM package

### Migration Guide
Users upgrading from v1.x need to:
1. Uninstall the old package: `npm uninstall -g claude-self-reflection`
2. Install the new package: `npm install -g claude-self-reflect`
3. Run setup wizard: `claude-self-reflect setup`
4. Update Claude Desktop configuration to use `claude-self-reflect` instead of `claude-self-reflection`

## [1.0.0] - 2025-01-14

### 🎉 Initial Release

#### Added
- **One-command installation** - Get up and running in under 5 minutes
- **Semantic search** across all Claude Desktop conversations
- **Continuous import** - Automatically watches for new conversations
- **Multiple embedding providers** - Support for OpenAI, Voyage AI, and local models
- **Cross-project search** - Search across all your Claude projects simultaneously
- **Privacy-first design** - All data stays local, no cloud dependencies
- **MCP-native integration** - Built specifically for Claude Desktop
- **Comprehensive documentation** - Full setup guide and troubleshooting
- **Health monitoring** - Built-in health check and status commands
- **Backup/restore** functionality for data safety
- **Docker-based deployment** for easy setup and isolation

#### Performance
- 66.1% search accuracy with Voyage AI embeddings
- <100ms search latency for 100K+ conversations
- ~1,000 conversations/minute import speed
- ~1GB memory usage per million conversations

#### Supported Platforms
- macOS (Apple Silicon and Intel)
- Linux (Ubuntu 20.04+, Debian 11+)
- Windows (via WSL2)

#### Known Issues
- Large conversation files (>10MB) may slow down initial import
- Search accuracy varies by embedding model choice
- Windows native support requires WSL2

### Migration from Neo4j
If you're migrating from the original Neo4j-based memento-stack:
1. Export your data using the old system's export function
2. Install Claude Self-Reflect
3. Import will automatically process your conversation history
4. All conversation data is preserved with improved search capabilities

---

## Contributing

We welcome contributions! See our [Contributing Guide](CONTRIBUTING.md) for details.

To add to this changelog:
1. Add your changes under the [Unreleased] section
2. Use the appropriate category: Added, Changed, Deprecated, Removed, Fixed, Security
3. Include PR numbers and contributor credits

Example:
```
### Added
- New feature description (#123) - @contributor
```