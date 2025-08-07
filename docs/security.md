# Security

## Container Security Notice
⚠️ **Known Vulnerabilities**: Our Docker images are continuously monitored by Snyk and may show vulnerabilities in base system libraries. We want to be transparent about this:

- **Why they exist**: We use official Python Docker images based on Debian stable, which prioritizes stability over latest versions
- **Actual risk is minimal** because:
  - Most CVEs are in unused system libraries or require local access
  - Security patches are backported by Debian (version numbers don't reflect patches)
  - Our containers run as non-root users with minimal permissions
  - This is a local-only tool with no network exposure
- **What we're doing**: Regular updates, security monitoring, and evaluating alternative base images

**For production or security-sensitive environments**, consider:
- Building your own hardened images
- Running with additional security constraints (see below)
- Evaluating if the tool meets your security requirements

For maximum security:
```bash
# Run containers with read-only root filesystem
docker run --read-only --tmpfs /tmp claude-self-reflect
```

## Privacy & Data Security

### 🔒 Privacy & Data Exchange

| Mode | Data Storage | External API Calls | Data Sent | Search Quality |
|------|--------------|-------------------|-----------|----------------|
| **Local (Default)** | Your machine only | None | Nothing leaves your computer | Good - uses efficient local embeddings |
| **Cloud (Opt-in)** | Your machine | Voyage AI | Conversation text for embedding generation | Better - uses state-of-the-art models |

**Note**: Cloud mode sends conversation content to Voyage AI for processing. Review their [privacy policy](https://www.voyageai.com/privacy) before enabling.

### Data Protection
- **Local by default**: Your conversations never leave your machine unless you explicitly enable cloud embeddings
- **No telemetry**: We don't track usage or collect any data
- **Secure storage**: All data stored in Docker volumes with proper permissions
- **API keys**: Stored in .env file with 600 permissions (read/write by owner only)

### Security Best Practices
1. **Keep Docker updated**: Regularly update Docker Desktop/Engine
2. **Review permissions**: Check file permissions on config directories
3. **Audit API keys**: Only use cloud mode if you trust Voyage AI with your data
4. **Monitor resources**: Use Docker's built-in monitoring for container behavior
5. **Network isolation**: Containers run in isolated Docker networks

## Vulnerability Reporting

See our [Security Policy](../SECURITY.md) for vulnerability reporting procedures.

If you discover a security vulnerability, please email security@[domain] with:
- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fixes (if any)

We take security seriously and will respond to valid reports within 48 hours.