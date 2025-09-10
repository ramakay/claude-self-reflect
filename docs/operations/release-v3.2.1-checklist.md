# Release v3.2.1 Checklist

## Context
v3.2.0 was released but critical security fixes and personal info removal are still uncommitted.
This patch release addresses CVE-2025-7709 and removes hardcoded personal information.

## Pre-Release
- [x] Check current version: v3.2.0 is latest
- [x] Identify security vulnerabilities: CVE-2025-7709 SQLite3  
- [x] Identify personal info removal: hardcoded username in server.py
- [ ] Security scan passes
- [ ] All CI/CD checks green
- [ ] Contributors acknowledged

## Release Steps
- [ ] Commit security fixes and personal info removal
- [ ] Bump version to v3.2.1 in package.json
- [ ] Tag created and pushed
- [ ] GitHub release created
- [ ] NPM package published (automated)
- [ ] Announcements sent

## Verification
- [ ] GitHub release visible
- [ ] NPM package updated  
- [ ] No rollback needed
- [ ] Security issues resolved
- [ ] No personal information in codebase

## Changes for v3.2.1
1. **Security Fixes**
   - CVE-2025-7709 SQLite3 vulnerability mitigation in all Dockerfiles
   
2. **Privacy Protection**
   - Removed hardcoded personal username from mcp-server/src/server.py
   - Replaced with wildcard pattern for broader compatibility
   
3. **Minor Improvements**
   - Updated .gitignore for temporary files
   - Minor fixes in import scripts