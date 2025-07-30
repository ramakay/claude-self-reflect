# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 2.3.x   | :white_check_mark: |
| 2.2.x   | :x:                |
| < 2.2   | :x:                |

## Reporting a Vulnerability

If you discover a security vulnerability, please **DO NOT** create a public issue. Instead:

1. Email the details to the maintainer through GitHub
2. Include:
   - Description of the vulnerability
   - Steps to reproduce
   - Potential impact
   - Suggested fix (if any)

We aim to respond within 48 hours and will work with you to understand and address the issue promptly.

## Security Measures

### Code Security
- No hardcoded API keys or secrets
- Environment variables for sensitive configuration
- Regular dependency updates
- Automated security scanning in CI/CD

### Repository Security
- Branch protection on `main`
- Required PR reviews
- Automated security checks must pass
- No direct commits to main branch

### Data Security
- Local embeddings by default (no data sent to cloud)
- Optional cloud embeddings with explicit opt-in
- No tracking or analytics
- All data stored locally in Docker volumes

### CI/CD Security
- Secrets scanning with Gitleaks
- Dependency vulnerability scanning
- Docker image security scanning with Trivy
- Code quality checks with Bandit
- File permission validation

## Best Practices for Contributors

1. **Never commit secrets**: Use environment variables
2. **Check dependencies**: Run `npm audit` and `pip-audit` before submitting PRs
3. **Test locally**: Ensure all tests pass before pushing
4. **Use .gitignore**: Don't commit generated files or local data
5. **Review changes**: Double-check your commits for sensitive data

## Recommended GitHub Settings

For repository administrators:

### Branch Protection Rules for `main`:
- ✅ Require pull request reviews before merging
- ✅ Dismiss stale pull request approvals when new commits are pushed
- ✅ Require status checks to pass before merging:
  - `python-test`
  - `npm-package-test`
  - `docker-build`
  - `secrets-scan`
  - `dependency-scan`
- ✅ Require branches to be up to date before merging
- ✅ Include administrators
- ✅ Do not allow force pushes
- ✅ Do not allow deletions

### Security Settings:
- ✅ Enable Dependabot security updates
- ✅ Enable secret scanning
- ✅ Enable push protection for secrets
- ✅ Enable vulnerability alerts

## Security Audit History

- **v2.4.11**: Security update to address CVE-2025-7458:
  - Updated all Docker base images from Python 3.11-slim to Python 3.12-slim
  - Added explicit `apt-get upgrade` in all Dockerfiles for system package updates
  - SQLite updated from vulnerable 3.40.1 to 3.50.1
  - Applied to: importer, watcher, mcp-server, streaming-importer, importer-isolated
- **v2.3.9**: Added gitleaks configuration to handle false positives and historical secrets
- **v2.3.7**: Major security cleanup - removed 250+ internal files, secured .env permissions
- **v2.3.3**: Migrated to local embeddings by default for privacy
- **v2.0.0**: Complete rewrite with security-first design

## Historical Security Notice

During routine security scanning, some API keys were found in git history. These have been:
- ✅ **Revoked** - All identified keys are no longer active
- ✅ **Allowlisted** in `.gitleaks.toml` since they're already invalid
- ✅ **Preserved in history** to avoid disrupting contributors

No action is required from contributors. Your local clones remain safe.