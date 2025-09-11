# Release v3.2.1 Checklist

## Context
v3.2.0 was released but critical security fixes and personal info removal are still uncommitted.
This patch release addresses CVE-2025-7709 and removes hardcoded personal information.

## Pre-Release
- [x] Check current version: v3.2.0 is latest
- [x] Identify security vulnerabilities: CVE-2025-7709 SQLite3  
- [x] Identify personal info removal: hardcoded username in server.py
- [x] Security scan passes
- [x] All CI/CD checks green
- [x] Contributors acknowledged

## Release Steps
- [x] Commit security fixes and personal info removal
- [x] Bump version to v3.2.1 in package.json
- [x] Tag created and pushed
- [x] GitHub release created
- [x] NPM package published (automated)
- [x] Announcements sent

## Verification
- [x] GitHub release visible
- [x] NPM package updated  
- [x] No rollback needed
- [x] Security issues resolved
- [x] No personal information in codebase

## RELEASE COMPLETED SUCCESSFULLY ✅
**Released:** v3.2.1 at 2025-09-10T04:11:24Z
**NPM Version:** 3.2.1 published successfully  
**CI/CD Pipeline:** All jobs completed successfully
**GitHub Release:** https://github.com/ramakay/claude-self-reflect/releases/tag/v3.2.1

## Changes for v3.2.1
1. **Security Fixes**
   - CVE-2025-7709 SQLite3 vulnerability mitigation in all Dockerfiles
   
2. **Privacy Protection**
   - Removed hardcoded personal username from mcp-server/src/server.py
   - Replaced with wildcard pattern for broader compatibility
   
3. **Minor Improvements**
   - Updated .gitignore for temporary files
   - Minor fixes in import scripts