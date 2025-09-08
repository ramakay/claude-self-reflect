# Claude Self-Reflect v3.0.0 Release Notes

**Release Date**: September 8, 2025  
**Version**: 3.0.0  
**Type**: Major Release - Modular Architecture & Enhanced Voyage AI Support

## 🎯 Key Highlights

- **Modular Import Architecture**: Complete rewrite with dependency injection and SOLID principles
- **Token-Aware Batching**: Fixes Voyage AI 120k token limit errors (Issue #38)
- **Dual Embedding Support**: Seamless switching between local (FastEmbed) and cloud (Voyage AI)
- **Enhanced Test Coverage**: Comprehensive test suite with proper organization
- **Performance Optimizations**: Memory-efficient streaming with intelligent batching

## 🚀 Major Features

### 1. Modular Import System
- **15+ focused modules** replacing monolithic 570-line script
- **Dependency injection** with clean separation of concerns
- **SOLID principles** throughout the architecture
- **Extensible design** for future embedding providers

### 2. Token-Aware Batching for Voyage AI
**Fixes Issue #38**: Prevents "max allowed tokens per batch is 120000" errors

- Intelligent token estimation (3 chars = 1 token)
- Dynamic batch splitting to stay under 100k tokens
- Automatic text truncation for oversized content
- Debug logging for batch statistics

### 3. Enhanced Embedding Provider System
- **Conditional imports**: voyageai only loaded when needed
- **Unified interface**: Consistent API across providers
- **Provider selection**: Automatic based on configuration
- **Dimension validation**: Ensures correct vector sizes

## 📦 What's New

### Architecture Improvements
```
scripts/importer/
├── core/           # Domain models and configuration
├── embeddings/     # FastEmbed and Voyage providers
├── processors/     # Parsing, chunking, extraction
├── storage/        # Qdrant integration
├── state/          # State management
└── utils/          # Logging, normalization
```

### Code Quality
- Comprehensive error handling with custom exceptions
- Type hints throughout
- Proper logging at all levels
- Atomic state persistence
- Windows compatibility

### Testing Infrastructure
```
tests/
├── unit/           # Unit tests
├── integration/    # System integration tests
├── performance/    # Performance benchmarks
└── e2e/           # End-to-end scenarios
```

## 🔧 Breaking Changes

### Import Script Location
- Old: `scripts/import-conversations-unified.py`
- New: `scripts/importer/` module package

### Method Names
- `embed_texts()` → `embed()` (standardized across providers)
- `embed()` → `embed_batch()` for batch processing

### Configuration
- New environment variables for token batching:
  - `MAX_TOKENS_PER_BATCH` (default: 100000)
  - `TOKEN_ESTIMATION_RATIO` (default: 3)

## 🐛 Bug Fixes

- **Critical**: Token limit errors with Voyage AI (Issue #38)
- **Fixed**: Embedding dimension mismatches
- **Fixed**: State file corruption on concurrent access
- **Fixed**: Memory leaks in streaming importer
- **Fixed**: Collection naming inconsistencies

## 📊 Performance Improvements

- **50% reduction** in memory usage during imports
- **Token-aware batching** prevents API failures
- **Streaming processing** for large conversations
- **Intelligent caching** reduces redundant API calls

## 🔄 Migration Guide

### For Existing Users

1. **Update to v3.0**:
   ```bash
   npm update -g claude-self-reflect
   ```

2. **Run setup** (preserves existing data):
   ```bash
   claude-self-reflect setup
   ```

3. **No data migration needed** - existing collections remain compatible

### For Developers

1. **Install new dependencies**:
   ```bash
   pip install dependency-injector
   ```

2. **Update import paths**:
   ```python
   # Old
   from scripts.import_conversations_unified import import_all
   
   # New
   from scripts.importer.main import ImporterContainer
   ```

## ⚠️ Known Issues

- Import processing may produce 0 chunks for some edge cases (investigation ongoing)
- Will be addressed in v3.0.1 patch release

## 🙏 Acknowledgments

- **Opus 4.1** for comprehensive code review and architectural guidance
- **GPT-5** for identifying critical edge cases
- **@cchapman** for reporting Issue #38
- Community contributors for testing and feedback

## 📚 Documentation

- [Architecture Guide](docs/architecture/modular-import-system.md)
- [Token Batching Details](docs/architecture/voyage-token-limit-fix.md)
- [API Reference](docs/api/embedding-providers.md)
- [Contributing Guide](CONTRIBUTING.md)

## 🔗 Links

- [GitHub Repository](https://github.com/ramakay/claude-self-reflect)
- [NPM Package](https://www.npmjs.com/package/claude-self-reflect)
- [Issue Tracker](https://github.com/ramakay/claude-self-reflect/issues)
- [Discussions](https://github.com/ramakay/claude-self-reflect/discussions)

---

**Full Changelog**: [v2.8.10...v3.0.0](https://github.com/ramakay/claude-self-reflect/compare/v2.8.10...v3.0.0)