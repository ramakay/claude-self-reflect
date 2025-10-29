# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 7.0.x   | :white_check_mark: |
| 6.0.x   | :white_check_mark: |
| < 6.0   | :x:                |

## Deployment Security Models

Claude Self-Reflect supports two deployment modes with different security considerations:

### Standalone Mode (Default)

**Description**: Single-user deployment on personal workstation or laptop.

**Security Characteristics**:
- No QDRANT_API_KEY required (unauthenticated Qdrant)
- All data stays on localhost
- Protected by system-level authentication (user login)
- Suitable for personal use and development

**When to Use**:
- Personal development environment
- Single-user workstation
- Local testing and experimentation
- No network exposure needed

**Security Considerations**:
- Qdrant accessible only from localhost (default: http://localhost:6333)
- No authentication required for Qdrant operations
- Docker containers run as non-root user (UID 1001)
- File permissions restrict access to owner only

### Shared Mode (Multi-User)

**Description**: Multi-user deployment on shared server or cloud instance.

**Security Characteristics**:
- QDRANT_API_KEY required (authenticated Qdrant)
- Multiple users share the same Qdrant instance
- API key protects against unauthorized access
- Suitable for team deployments

**When to Use**:
- Shared development server
- Team collaboration environment
- Cloud-hosted deployment
- Network-exposed Qdrant instance

**Security Requirements**:
1. Set strong QDRANT_API_KEY in .env:
   ```bash
   QDRANT_API_KEY=your-secure-random-key-here
   ```

2. Configure Qdrant with authentication:
   ```yaml
   # config/qdrant-config.yaml
   service:
     api_key: ${QDRANT_API_KEY}
   ```

3. Use HTTPS/TLS for network traffic (if exposed)

4. Implement network-level access controls (firewall, VPN)

## Privacy & Data Security

### Privacy & Data Exchange

| Mode | Data Storage | External API Calls | Data Sent | Search Quality |
|------|--------------|-------------------|-----------|----------------|
| **Local (Default)** | Your machine only | None | Nothing leaves your computer | Good - uses efficient local embeddings |
| **Cloud (Opt-in)** | Your machine | Voyage AI | Conversation text for embedding generation | Better - uses state-of-the-art models |
| **Batch Automation (Opt-in)** | Your machine | Anthropic Batch API | Narrative summaries only | Best - 9.3x better search quality |

**Note**: Cloud mode and batch automation send data to external APIs. Review privacy policies before enabling:
- Voyage AI: https://www.voyageai.com/privacy
- Anthropic: https://www.anthropic.com/privacy

### Data Protection
- **Local by default**: Your conversations never leave your machine unless you explicitly enable cloud features
- **No telemetry**: We don't track usage or collect any data
- **Secure storage**: All data stored in Docker volumes with proper permissions
- **API keys**: Stored in .env file with 600 permissions (read/write by owner only)

## Reporting a Vulnerability

**IMPORTANT**: Do not create public GitHub issues for security vulnerabilities.

### How to Report

1. **GitHub Security Advisories** (preferred):
   - Go to https://github.com/ramakay/claude-self-reflect/security/advisories
   - Click "Report a vulnerability"

2. **Include**:
   - Description of the vulnerability
   - Steps to reproduce
   - Potential impact
   - Suggested fix (if any)

3. **Expected Response**:
   - Acknowledgment within 48 hours
   - Initial assessment within 7 days
   - Fix timeline estimate within 14 days

### Disclosure Policy

- We follow coordinated disclosure
- Security fixes released as patch versions
- Public disclosure after fix is available
- Credit given to security researchers (with permission)

## Security Best Practices

### Docker Security

1. **Non-Root Users** (v7.0+):
   - All containers run as appuser (UID 1001)
   - No privileged operations required
   - Prevents privilege escalation attacks

2. **Volume Permissions**:
   ```bash
   # Correct ownership for Docker volumes
   chown -R 1001:1001 ~/.claude-self-reflect
   ```

3. **Resource Limits**:
   - Memory limits prevent DoS via resource exhaustion
   - CPU limits prevent runaway processes
   - See docker-compose.yaml for configuration

### File System Security

1. **State File Protection** (v7.0+):
   - Atomic writes prevent corruption
   - File locking (fcntl) prevents race conditions
   - UTF-8 encoding enforced

2. **Directory Permissions**:
   ```bash
   # Recommended permissions
   chmod 700 ~/.claude-self-reflect
   chmod 600 ~/.claude-self-reflect/config/*.json
   ```

### API Key Management

1. **Environment Variables**:
   - Never commit .env files to git
   - Use .env.example as template
   - Rotate API keys regularly

2. **Batch Automation Keys** (v7.0+):
   - ANTHROPIC_API_KEY: Required for batch features
   - QDRANT_API_KEY: Required for shared mode only
   - VOYAGE_KEY: Required only if using cloud embeddings

3. **Key Rotation**:
   ```bash
   # Update .env file
   vim ~/.claude-self-reflect/.env

   # Restart services
   docker compose down
   docker compose --profile batch-automation up -d
   ```

### Network Security

1. **Standalone Mode** (Default):
   - Qdrant bound to localhost only
   - No external network exposure
   - Docker network isolation

2. **Shared Mode**:
   - Use HTTPS/TLS for Qdrant
   - Implement firewall rules
   - Consider VPN for access
   - Enable Qdrant authentication

3. **Port Configuration**:
   ```yaml
   # Standalone (default)
   ports:
     - "127.0.0.1:6333:6333"

   # Shared (requires auth)
   ports:
     - "0.0.0.0:6333:6333"
   environment:
     - QDRANT_API_KEY=${QDRANT_API_KEY}
   ```

## Container Security Notice

**Known Vulnerabilities**: Our Docker images are continuously monitored and may show vulnerabilities in base system libraries.

- **Why they exist**: We use official Python Docker images based on Debian stable
- **Actual risk is minimal** because:
  - Most CVEs are in unused system libraries
  - Security patches are backported by Debian
  - Containers run as non-root users (UID 1001)
  - Local-only tool with no network exposure by default
- **What we're doing**: Regular updates, security monitoring, and evaluating alternative base images

**For production environments**:
```bash
# Run containers with read-only root filesystem
docker run --read-only --tmpfs /tmp claude-self-reflect
```

## Known Security Considerations

### Qdrant Authentication (v7.0+)

**Issue**: Standalone mode runs Qdrant without authentication.

**Mitigation**:
- Qdrant bound to localhost only (not network-exposed)
- Docker network isolation prevents external access
- System-level authentication (user login) protects access

**Action Required**: None for standalone mode. For shared mode, enable QDRANT_API_KEY.

### Batch Automation Data Transmission (v7.0+)

**Issue**: Batch automation sends conversation summaries to Anthropic Batch API.

**Mitigation**:
- Feature disabled by default
- Users must explicitly enable with ANTHROPIC_API_KEY
- Data transmitted over HTTPS
- Only narrative summaries sent (not full conversations)

**Action Required**: Review privacy implications before enabling batch automation.

## Security Audit History

- v7.0.0 (2025-10-28): Security hardening (7 critical + 5 high priority fixes)
- v3.3.1 (2025-09-14): GPT-5 comprehensive security review
- v2.8.5 (2025-09-02): CVE-2025-58050 mitigation
- v2.3.3 (2025-07-27): Command injection fixes

## Security Updates

Security updates are released as patch versions (x.y.Z):

- Critical: Released within 48 hours
- High: Released within 7 days
- Medium: Released within 30 days
- Low: Included in next regular release

## Contact

For security inquiries:
- GitHub Security Advisories (preferred)
- GitHub Discussions (security-related)
- Issue Tracker (for non-security bugs only)

## License

This security policy is part of the Claude Self-Reflect project and is licensed under the MIT License.