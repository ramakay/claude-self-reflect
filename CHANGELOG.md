# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [7.0.0] - 2025-01-26

### 🔒 Security

- **CRITICAL**: Added non-root user (`appuser` UID 1001) to all batch automation Dockerfiles
- **CRITICAL**: Replaced hardcoded paths with environment variables throughout codebase
- **CRITICAL**: Fixed volume mount path mismatch (now uses `/home/appuser/.claude-self-reflect`)
- **CRITICAL**: Updated API key documentation (standalone vs shared deployment modes)
- Added Qdrant connection retry logic with exponential backoff (5 retries, max 32s)
- Sanitized PII (personal paths) from all documentation files

### ✨ Features

- **Batch Automation**: New batch monitoring and watcher services
  - `batch-watcher`: Queues conversations, triggers batch processing (every 10 files or 30 min)
  - `batch-monitor`: Monitors Batch API jobs, triggers evaluation generation
  - Hot/warm/cold priority system for responsive processing
- **Configuration**: Centralized configuration system (`src/runtime/config.py`)
- **File Locking**: Added fcntl-based file locking for queue state (prevents race conditions)

### 🐛 Fixes

- Increased batch-watcher memory to 2GB
- Increased subprocess timeout to 1800s (30 minutes)
- Added `encoding='utf-8'` to all file operations
- Fixed circular import risks

### 🚀 Docker Improvements

- Added health checks (30s interval, 3 retries)
- Configured log rotation (10MB max, 3 files)
- Environment-based configuration throughout

### 💥 BREAKING CHANGES

- Volume paths now use `/home/appuser/.claude-self-reflect` (was `/root/.claude-self-reflect`)
- Run `docker compose down && docker compose up -d` to recreate containers

## [6.0.5] - 2025-01-18

- Fixed init-permissions network race on macOS

[7.0.0]: https://github.com/ramakay/claude-self-reflect/compare/v6.0.5...v7.0.0
[6.0.5]: https://github.com/ramakay/claude-self-reflect/releases/tag/v6.0.5
