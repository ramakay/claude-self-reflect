# Packaging Workflow for Developers

## Overview

v6.0.0 introduced a convention-based packaging system using the `src/` directory pattern. This guide explains how to add new files and maintain the package.

## Directory Structure (v6.0.0+)

```
claude-self-reflect/
├── src/                          # Production code (PACKAGED)
│   ├── runtime/                  # Import & state management
│   ├── cli/                      # CLI tools
│   └── importer/                 # Modular importer
├── scripts/                      # Development tools (EXCLUDED from package)
│   ├── dev/                      # Debug, check, analyze scripts
│   ├── migration/                # One-time migration scripts
│   ├── quality/                  # Quality analysis tools
│   ├── maintenance/              # Cleanup, optimize scripts
│   ├── auto-migrate.cjs          # Essential migration (PACKAGED)
│   ├── migrate-to-unified-state.py  # Essential (PACKAGED)
│   └── csr-status                # Essential CLI (PACKAGED)
├── mcp-server/                   # MCP server (PACKAGED)
├── installer/                    # npm installer (PACKAGED)
├── shared/                       # Shared utilities (PACKAGED)
├── config/                       # Config files (PACKAGED)
└── tests/                        # Test files (EXCLUDED)
```

## Adding Production Code

### Add to `src/runtime/`
For production scripts used in imports, state management, or Docker containers:

```bash
# 1. Create your file
touch src/runtime/my-new-feature.py

# 2. No package.json changes needed!
# Wildcard src/**/*.py automatically includes it
```

### Add to `src/cli/`
For CLI tools users will run directly:

```bash
touch src/cli/my-cli-tool.py
# Also covered by src/**/*.py wildcard
```

### Add to `src/importer/`
For importer module enhancements:

```bash
touch src/importer/processors/my-processor.py
# Covered by src/**/*.py wildcard
```

## Adding Development Scripts

Development scripts go in `scripts/` subdirectories and are **automatically excluded** from npm package via `.npmignore`.

### Categories

**scripts/dev/** - Debug, check, analyze, test
```bash
mv my-debug-script.py scripts/dev/
```

**scripts/migration/** - One-time migrations
```bash
mv migrate-feature-x.py scripts/migration/
```

**scripts/quality/** - Quality analysis, pattern checking
```bash
mv check-quality-x.py scripts/quality/
```

**scripts/maintenance/** - Cleanup, optimize, stats
```bash
mv cleanup-feature.py scripts/maintenance/
```

## package.json `files` Array

**Simple and robust** (v6.0.0+):

```json
{
  "files": [
    "installer/**/*.js",
    "scripts/auto-migrate.cjs",
    "scripts/migrate-to-unified-state.py",
    "scripts/csr-status",
    "mcp-server/src/**/*.py",
    "mcp-server/pyproject.toml",
    "mcp-server/run-mcp*.sh",
    "src/**/*.py",           // ← Wildcards work safely!
    "src/**/*.sh",           // ← No manual file tracking!
    "shared/**/*.py",
    ".claude/agents/*.md",
    "!.claude/agents/*test*.md",
    "config/qdrant-config.yaml",
    "docker-compose.yaml",
    "Dockerfile.*",
    ".env.example",
    "README.md",
    "LICENSE"
  ]
}
```

### Why Wildcards Work Now

**Before v6.0.0**: `scripts/**/*.py` pulled in 100+ dev files
**After v6.0.0**: `src/**/*.py` only includes production code

Production and dev files are now **physically separated**.

## Validation

### Before Committing

```bash
# 1. Run package test
python tests/test_npm_package_contents.py

# 2. Run import validator
python scripts/dev/validate-package-imports.py

# 3. Check package size
npm pack --dry-run
```

### What to Check

- **Total files**: Should be ~115 files
- **Package size**: Should be ~220-230 KB
- **No scripts/dev/** files**: Dev scripts excluded
- **All src/** files**: Production code included

## Common Tasks

### Add a New Runtime Script

```bash
# 1. Create file
cat > src/runtime/new-feature.py << 'EOF'
#!/usr/bin/env python3
from metadata_extractor import MetadataExtractor
# ... your code ...
EOF

# 2. Make executable if needed
chmod +x src/runtime/new-feature.py

# 3. Test packaging
npm pack --dry-run | grep "new-feature.py"
# Should show: npm notice ... src/runtime/new-feature.py

# 4. Commit
git add src/runtime/new-feature.py
git commit -m "feat: add new-feature runtime script"
```

### Update Docker Container Script

If changing a script used in Docker:

```bash
# 1. Edit src/runtime/my-script.py
vim src/runtime/my-script.py

# 2. Verify path in docker-compose.yaml
grep "my-script.py" docker-compose.yaml
# Should show: python /app/src/runtime/my-script.py

# 3. Rebuild container
docker compose build importer

# 4. Test
docker compose up importer
```

### Migrate Old Script to src/

```bash
# 1. Move file
git mv scripts/old-prod-script.py src/runtime/

# 2. Update imports inside file if needed
# Change: from utils import X
# To:     from src.runtime.utils import X

# 3. Update references
grep -r "scripts/old-prod-script.py" .
# Update docker-compose.yaml, Dockerfiles, agents/, etc.

# 4. Test
python src/runtime/old-prod-script.py
npm pack --dry-run | grep "old-prod-script.py"
```

## Troubleshooting

### "Module not found" after packaging

**Problem**: Script works locally but fails in package

**Solution**: Check if it's in npm pack output
```bash
npm pack --dry-run | grep "your-file.py"
```

If missing, ensure it's in `src/` directory (not `scripts/`).

### Package size too large

**Problem**: Package exceeds 300 KB

**Solution**: Check for leaked dev files
```bash
npm pack --dry-run | grep "scripts/" | grep -v "auto-migrate\|migrate-to-unified-state\|csr-status"
```

If you see dev scripts, they leaked. Check `.npmignore` patterns.

### Import errors in Docker

**Problem**: `ModuleNotFoundError` in Docker container

**Solution**: Verify `PYTHONPATH=/app` in Dockerfile
```dockerfile
ENV PYTHONPATH=/app
```

And check volume mounts use `/app/src`:
```yaml
volumes:
  - ./src:/app/src:ro
```

## Migration from v5.0.x

See: [v6.0.0 Migration Guide](../operations/v6.0.0-migration-guide.md)

## References

- Issue #97: src/ restructuring
- Issue #98: scripts/ organization
- Issue #87: Package validation
- PR #XXX: Convention-based packaging
