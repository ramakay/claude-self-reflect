# 16K Narrative for conv_35a2864c_local

**Conversation ID**: 456f476d-2176-4bbb-b44e-9610fb0677b7
**Project**: unknown
**Length**: 3435 chars

---

## Search Index

GitHub issue #41 Docker container missing scripts npm package installation fix, system memory crisis resolution with ccusage statusline process cleanup, comprehensive npm release v2.7.1 with CI/CD monitoring and open-source maintenance workflow

---

## Full Narrative

# GitHub Issue Resolution and System Memory Crisis Fix

## Search Summary
A comprehensive project involving two critical issues: resolving GitHub issue #41 with Docker container script availability in npm packages, and diagnosing/fixing a system memory crisis caused by accumulating ccusage statusline processes. The session culminated in a successful npm release (v2.7.1) and complete system recovery.

## Problem-Solution Mapping
- **Request**: Fix GitHub issue #41 regarding missing scripts in Docker containers when installed via npm, plus diagnose system memory crisis
- **Solution Type**: debugging + fix + release management
- **Tools Used**: gh CLI, Read, Edit, Write, Bash, Docker
- **Files Modified**: 
  - `Dockerfile.importer` - Added script copying
  - `docker-compose.yaml` - Fixed PREFER_LOCAL_EMBEDDINGS defaults
  - `~/.claude/statusline.sh` - Added timeout and process cleanup
  - `package.json` - Version bump to 2.7.1
  - `README.md` - Fixed broken Repobeats image

## Technical Pattern
- **Pattern Name**: NPM Package Docker Integration Fix
- **When to use**: When npm global packages need Docker containers with local scripts
- **Steps**: 
  1. Identify volume mount vs COPY instruction mismatch
  2. Update Dockerfile to COPY scripts during build
  3. Fix environment variable defaults consistency
  4. Create comprehensive test scripts for validation
  5. Publish npm release with proper CI/CD monitoring

## Implementation Details

### Memory Crisis Resolution
Diagnosed 20+ zombie Node.js processes from `ccusage statusline --offline` command consuming 4-8GB RAM and high CPU. Root cause was statusline script spawning processes without timeout or cleanup. Applied three-part fix:
- Process cleanup before spawning new ones
- 2-second timeout protection
- 10-second caching system

### Docker Container Fix
Issue #41 identified that npm-installed packages couldn't access scripts in Docker containers. The problem was that `docker-compose.yaml` used volume mounts (`./scripts:/scripts:ro`) which only work from repository directory, not npm installation directory. Fixed by:
- Adding `COPY scripts/ /app/scripts/` to Dockerfile.importer
- Standardizing PREFER_LOCAL_EMBEDDINGS defaults across services
- Creating comprehensive test infrastructure

### Release Process
Managed complete open-source maintainer workflow:
- Rebased and pushed changes to main branch
- Version bump from 2.7.0 to 2.7.1
- Created git tags and published to npm registry
- Monitored CI/CD pipelines and GitHub Actions
- Verified package availability and content integrity

## Validation & Outcome
- **System Recovery**: Memory usage dropped from 25GB to 568MB, eliminated zombie processes
- **Docker Tests**: Container now successfully accesses scripts at `/app/scripts/`
- **CI/CD Success**: All Python (3.10-3.12) and Node (18.x, 20.x) tests passed
- **NPM Publication**: Package v2.7.1 successfully published with 50 files, 101.3 kB
- **Security Scans**: CodeQL and security scans all passed
- **Issue Status**: Updated with solution, awaiting user confirmation

## Search Keywords
- **Primary**: docker container scripts npm package statusline memory leak
- **Secondary**: ccusage processes zombie cleanup timeout github issue release management
- **Frameworks/Tools**: docker-compose dockerfile npm github-actions qdrant supabase
- **Pattern Tags**: npm-docker-integration memory-leak-fix release-workflow open-source-maintenance

---

## Metadata

**Tools**: gh, Read, Edit, Write, Bash, Docker

**Concepts**: docker, npm, github-actions, memory-leak, process-management, security-scanning, ci-cd, open-source-maintenance, package-publishing, system-monitoring

**Files**: Dockerfile.importer, docker-compose.yaml, ~/.claude/statusline.sh, package.json, README.md
