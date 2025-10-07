# Corporate Proxy Installation Guide

## Overview
This guide addresses common issues when installing Claude Self-Reflect on corporate machines with:
- SSL certificate interception (man-in-the-middle proxies)
- Restricted sudo access
- Blocked downloads from HuggingFace/PyPI

## Issue 1: FastEmbed Model Download Blocked

### Symptom
```
SSL: CERTIFICATE_VERIFY_FAILED
Error downloading sentence-transformers/all-MiniLM-L6-v2
```

### Root Cause
Corporate proxies intercept SSL connections, causing Python's certificate verification to fail when downloading FastEmbed models from HuggingFace.

### Solution: Manual Model Installation

#### Step 1: Download Model from Google Cloud Storage
```bash
# FastEmbed mirrors models on Google Cloud Storage (no SSL issues)
cd ~/.cache
mkdir -p fastembed
cd fastembed

# Download the model (79MB)
curl -O https://storage.googleapis.com/qdrant-fastembed/sentence-transformers-all-MiniLM-L6-v2.tar.gz

# Extract
tar -xzf sentence-transformers-all-MiniLM-L6-v2.tar.gz
```

#### Step 2: Configure Docker to Use Offline Model
Edit `docker-compose.yaml` to add cache mount and offline mode:

```yaml
services:
  importer:
    volumes:
      - ${HOME}/.cache/fastembed:/root/.cache/fastembed:ro  # Add this line
    environment:
      - HF_HUB_OFFLINE=1  # Force offline mode
```

#### Step 3: Configure MCP Server
Edit `mcp-server/run-mcp.sh`:

```bash
export HF_HUB_OFFLINE=1
export FASTEMBED_CACHE_PATH="$HOME/.cache/fastembed"
```

### Verification
```bash
# Test Docker can access model
docker compose run --rm importer python -c "from fastembed import TextEmbedding; print('Model loaded')"
```

---

## Issue 2: Statusline Not Installing (No Sudo Access)

### Symptom
`csr-status: command not found` when running Claude Code

### Root Cause
The npm postinstall script tries to create a symlink at `/usr/local/bin/csr-status` which requires sudo access.

### Solution: Manual Statusline Setup

#### Option A: User-Local Binary (No Sudo Required)
```bash
# Create user-local bin directory
mkdir -p ~/bin

# Find npm global package location
NPM_PREFIX=$(npm config get prefix)
CSR_SCRIPT="$NPM_PREFIX/lib/node_modules/claude-self-reflect/scripts/csr-status"

# Create symlink in user directory
ln -sf "$CSR_SCRIPT" ~/bin/csr-status
chmod +x ~/bin/csr-status

# Add to PATH (add to ~/.bashrc or ~/.zshrc)
export PATH="$HOME/bin:$PATH"

# Apply immediately (or restart terminal)
source ~/.bashrc  # or source ~/.zshrc
```

#### Option B: Request IT to Create Symlink
```bash
# Provide this command to your IT department
sudo ln -sf /opt/homebrew/lib/node_modules/claude-self-reflect/scripts/csr-status /usr/local/bin/csr-status
sudo chmod +x /usr/local/bin/csr-status
```

#### Option C: Skip Statusline Integration
Statusline is optional. Core MCP search functionality works without it.

### Verification
```bash
csr-status --compact
# Should show: [81%][✨] or similar
```

---

## Issue 3: AST-Grep Not Installed

### Symptom
Quality analysis features don't work, AST-Grep commands fail

### Root Cause
AST-Grep is an optional dependency for code quality analysis, not automatically installed by npm package.

### Solution: Install AST-Grep

#### macOS (Homebrew)
```bash
brew install ast-grep
```

#### Linux/Corporate Machine
```bash
# Option A: npm global install (no sudo)
npm install -g @ast-grep/cli

# Option B: Download binary directly
curl -LO https://github.com/ast-grep/ast-grep/releases/latest/download/ast-grep-linux-x64.zip
unzip ast-grep-linux-x64.zip
mv ast-grep ~/bin/  # User-local bin directory
chmod +x ~/bin/ast-grep
```

### Verification
```bash
ast-grep --version
# Should show: ast-grep 0.39.4 or later
```

### Impact if Not Installed
- Core MCP search works fine
- Code quality analysis features unavailable
- `/fix-quality` command won't work
- Quality gates in commits still work (uses Python patterns)

---

## Issue 4: Correct Metadata Script Path

### Symptom
`docker compose run --rm importer python /app/scripts/delta-metadata-update-safe.py` fails with "No such file"

### Root Cause
Documentation shows outdated path. Script moved in v6.0 restructuring.

### Solution: Use Correct Path
```bash
# CORRECT (v6.0+)
docker compose run --rm importer python /app/src/runtime/delta-metadata-update-safe.py

# OLD (v5.x and earlier)
docker compose run --rm importer python /app/scripts/delta-metadata-update-safe.py
```

### Verification
```bash
docker compose run --rm importer ls -la /app/src/runtime/delta-metadata-update-safe.py
# Should show the file exists
```

---

## Complete Corporate Installation Checklist

### Pre-Installation
- [ ] Docker Desktop installed and running
- [ ] Node.js 18+ installed
- [ ] Determine if you have sudo access
- [ ] Check if corporate proxy blocks HuggingFace

### Installation Steps
```bash
# 1. Install npm package
npm install -g claude-self-reflect

# 2. Manual FastEmbed model download (if proxy blocks HuggingFace)
mkdir -p ~/.cache/fastembed
cd ~/.cache/fastembed
curl -O https://storage.googleapis.com/qdrant-fastembed/sentence-transformers-all-MiniLM-L6-v2.tar.gz
tar -xzf sentence-transformers-all-MiniLM-L6-v2.tar.gz

# 3. Run setup wizard
claude-self-reflect setup
# Select "Local (FastEmbed)" for embedding mode
# Choose default paths

# 4. Manual statusline setup (if no sudo)
mkdir -p ~/bin
NPM_PREFIX=$(npm config get prefix)
ln -sf "$NPM_PREFIX/lib/node_modules/claude-self-reflect/scripts/csr-status" ~/bin/csr-status
echo 'export PATH="$HOME/bin:$PATH"' >> ~/.bashrc  # or ~/.zshrc
source ~/.bashrc  # Apply immediately (or restart terminal)

# 5. Optional: Install AST-Grep
brew install ast-grep  # macOS
# or
npm install -g @ast-grep/cli  # Any platform

# 6. Start services
docker compose up -d qdrant

# 7. Import conversations
docker compose run --rm importer python /app/src/runtime/import-conversations-unified.py --limit 5
```

### Post-Installation Verification
```bash
# Check Docker
docker ps | grep qdrant
# Should show: claude-reflection-qdrant running

# Check MCP
python mcp-server/src/status.py
# Should show percentage > 0%

# Check statusline (optional)
csr-status
# Should show import status

# Test MCP search
# In Claude: "search for docker issues we discussed"
```

---

## Troubleshooting

### "Import shows 0% but search works"
This is normal after manual FastEmbed installation. The import happened correctly. Trust the search results, not the status percentage.

### "csr-status: command not found"
Statusline is optional. Either:
1. Follow "Manual Statusline Setup" above
2. Skip statusline and use `python mcp-server/src/status.py` directly

### "Model mismatch between import and MCP"
If you initially used the wrong model:
```bash
# Delete collections
docker compose run --rm importer python -c "
from qdrant_client import QdrantClient
client = QdrantClient(url='http://qdrant:6333')
for collection in client.get_collections().collections:
    print(f'Deleting {collection.name}')
    client.delete_collection(collection.name)
"

# Re-import with correct model
docker compose run --rm importer python /app/src/runtime/import-conversations-unified.py
```

### "Docker mount errors on macOS"
Ensure you're using v6.0.1+ which fixed Docker volume mount issues for global npm installs.

---

## Environment Variables Reference

Add these to your `~/.bashrc` or `~/.zshrc` for corporate environments:

```bash
# FastEmbed offline mode
export HF_HUB_OFFLINE=1
export FASTEMBED_CACHE_PATH="$HOME/.cache/fastembed"

# User-local binaries
export PATH="$HOME/bin:$PATH"

# Optional: Disable SSL verification (NOT RECOMMENDED, use GCS mirrors instead)
# export REQUESTS_CA_BUNDLE=""
# export SSL_CERT_FILE=""
```

---

## Support

If you encounter issues not covered here:
1. Check system status: `python mcp-server/src/status.py`
2. View Docker logs: `docker compose logs`
3. Open an issue: https://github.com/ramakay/claude-self-reflect/issues

Include:
- Operating system and version
- Docker version (`docker --version`)
- Node version (`node --version`)
- Error messages from logs
- Whether you're behind a corporate proxy
