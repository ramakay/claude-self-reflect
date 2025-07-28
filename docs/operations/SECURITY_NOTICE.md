# Security Notice

## Revoked API Keys in Git History

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

If you discover any security issues, please report them responsibly to: ramakay@me.com