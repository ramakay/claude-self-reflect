# Security Notice

## UPDATE: 2025-09-17 - Voyage API Key Rotation Required

### CRITICAL: Immediate Action Required
A Voyage API key was exposed in local test documentation. While it was never committed to git, it should be rotated immediately:
1. **Revoke** the exposed key (prefix: `pa-JbR-`)
2. **Generate** a new Voyage API key
3. **Update** your `.env` file with the new key

All documentation files have been sanitized and .gitignore updated to prevent future exposure.

---

## Previous: Revoked API Keys in Git History

During a routine security scan, we identified some API keys in historical commits. All identified keys have been **revoked and are no longer active**.

### Actions Taken

1. ✅ All exposed API keys have been revoked
2. ✅ Added `.gitleaks.toml` configuration to prevent false positives in CI/CD
3. ✅ The keys are allowlisted in gitleaks config since they're already invalid

### For Contributors

- **No action required** - your local clones are safe
- The exposed keys cannot be used
- History has NOT been rewritten to avoid disruption

### Security Best Practices

- Never commit real API keys
- Use environment variables or `.env` files (gitignored)
- Review changes carefully before committing

If you discover any security issues, please report them responsibly to: user@example.com