# Setup Wizard v2 - UX Improvements Plan

## Overview

Based on user feedback (Issue #27) and installation challenges, the setup wizard needs significant improvements to provide a smoother, more intuitive experience.

## Current Pain Points

1. **Unclear Installation Scope**
   - Users confused about user-level vs project-level installation
   - No guidance on when to use which approach
   - MCP installation steps are opaque

2. **Upgrade Path Issues**
   - No clear process for upgrading existing installations
   - Broken Python environments cause silent failures
   - Docker containers need manual cleanup

3. **Poor Error Recovery**
   - Cryptic error messages
   - No automatic recovery options
   - Users left in partially installed states

4. **Lack of Feedback**
   - Minimal progress indicators
   - No confirmation of successful steps
   - Silent failures during installation

## Proposed Improvements

### 1. Intelligent Installation Detection

```javascript
class SetupWizard {
  async detectExistingInstallation() {
    const checks = {
      dockerRunning: await this.checkDocker(),
      qdrantRunning: await this.checkQdrant(),
      mcpInstalled: await this.checkMCP(),
      venvExists: await this.checkVenv(),
      dataExists: await this.checkExistingData()
    };
    
    return this.analyzeInstallationState(checks);
  }
}
```

**Features**:
- Auto-detect all components
- Identify partial installations
- Suggest appropriate action (upgrade/repair/fresh)

### 2. Clear Installation Modes

#### Interactive Mode Selection
```
Claude Self-Reflect Setup Wizard v2.0
=====================================

Detected: Existing installation (partial)
Current project: /Users/you/myproject

Select installation mode:

1. 🏠 User-Level Installation (Recommended)
   - Share memories across all projects
   - Single MCP server for all Claude windows
   - Best for: Individual developers

2. 📁 Project-Level Installation
   - Isolated memories for this project only
   - Separate MCP server per project
   - Best for: Teams, sensitive projects

3. 🔧 Repair Existing Installation
   - Fix broken components
   - Preserve existing data
   - Detected issues: Broken venv, MCP not connected

4. 🔄 Upgrade from v2.4.x
   - Migrate data and settings
   - Update all components
   - Clean old configurations

Choice [1-4]:
```

### 3. Enhanced Progress Feedback

```
Setting up Claude Self-Reflect...

[✓] Docker Desktop detected and running
[✓] Creating dedicated Docker network
[⠼] Starting Qdrant vector database...
    ├─ Pulling latest image
    ├─ Creating volume for data persistence
    └─ Starting container with optimized settings
[•] Setting up Python environment
[•] Installing MCP server
[•] Configuring Claude Code integration
[•] Importing existing conversations
```

**Features**:
- Real-time status updates
- Nested progress for complex operations
- Clear error messages with solutions
- Time estimates for long operations

### 4. Smart Error Recovery

```javascript
async handleSetupError(error, context) {
  const recovery = {
    DOCKER_NOT_RUNNING: {
      message: "Docker Desktop is not running",
      solutions: [
        "1. Start Docker Desktop manually",
        "2. Let setup wizard start it (macOS only)",
        "3. Install Docker Desktop from docker.com"
      ],
      autoFix: () => this.startDocker()
    },
    
    VENV_BROKEN: {
      message: "Python environment is corrupted",
      solutions: [
        "1. Let wizard rebuild environment (recommended)",
        "2. Manually fix Python path",
        "3. Skip Python setup (Docker-only mode)"
      ],
      autoFix: () => this.rebuildVenv()
    },
    
    MCP_CONNECTION_FAILED: {
      message: "Cannot connect to MCP server",
      solutions: [
        "1. Restart Claude Code",
        "2. Reinstall MCP server",
        "3. Check firewall settings"
      ],
      autoFix: () => this.reinstallMCP()
    }
  };
  
  return this.presentRecoveryOptions(recovery[error.code]);
}
```

### 5. New CLI Commands

#### `claude-self-reflect doctor`
```bash
$ claude-self-reflect doctor

Claude Self-Reflect Diagnostic Report
====================================

Component Status:
✓ Docker Desktop: Running (v4.25.0)
✓ Qdrant: Healthy (10,234 vectors)
✗ Python venv: Missing dependencies
✓ MCP Server: Installed (user-level)
⚠ Import Watcher: Not running

Issues Found:
1. Python environment missing 'qdrant-client'
   Fix: Run 'claude-self-reflect repair --component python'

2. Import watcher container stopped
   Fix: Run 'docker-compose up -d watcher'

Overall Health: 75% - Minor issues detected
```

#### `claude-self-reflect upgrade`
```bash
$ claude-self-reflect upgrade

Claude Self-Reflect Upgrade Assistant
====================================

Current version: 2.4.15
Latest version: 2.5.0

This upgrade includes:
- Critical Docker memory fixes
- Improved project search
- Better error messages

Steps that will be performed:
1. Backup existing data (./backup-2024-08-04/)
2. Stop all services
3. Update npm package
4. Rebuild Docker containers
5. Migrate configurations
6. Restart services
7. Verify installation

Estimated time: 5-7 minutes

Continue? [Y/n]:
```

#### `claude-self-reflect reset`
```bash
$ claude-self-reflect reset

⚠️  Claude Self-Reflect Reset Tool
==================================

This will:
- Stop and remove all Docker containers
- Remove Python virtual environment
- Clear MCP configuration
- Preserve your conversation data

Your data location: ~/claude-self-reflect-data
Data size: 1.2 GB (10,234 conversations)

Options:
1. Soft reset (keep data)
2. Hard reset (remove everything)
3. Cancel

Choice [1-3]:
```

### 6. Configuration Wizard

```javascript
class ConfigurationWizard {
  async configure() {
    const config = {};
    
    // Embedding choice with clear explanation
    config.embeddings = await this.prompt({
      message: "Select embedding model:",
      choices: [
        {
          value: "local",
          label: "Local (FastEmbed) - 100% private, no API needed",
          description: "Good accuracy, runs on your machine"
        },
        {
          value: "voyage",
          label: "Cloud (Voyage AI) - Better accuracy, requires API key",
          description: "State-of-the-art embeddings, data sent to cloud"
        }
      ],
      default: "local"
    });
    
    // Memory decay explanation
    config.decay = await this.prompt({
      message: "Enable memory decay?",
      description: `
        Memory decay gradually reduces relevance of old conversations
        like human memory. Recent conversations stay prominent while
        old ones fade. This improves search relevance over time.
      `,
      default: true
    });
    
    return config;
  }
}
```

### 7. Post-Installation Verification

```
Installation Complete! 🎉

Verifying your setup...
[✓] Docker containers running
[✓] Qdrant accepting connections
[✓] MCP server responding
[✓] Test search successful
[✓] Import watcher active

Quick Start:
1. Open Claude Code
2. Ask: "What conversations have we had about Docker?"
3. Claude will automatically use the reflection specialist

Your installation:
- Type: User-level
- Mode: Local embeddings (private)
- Projects tracked: 5
- Conversations: 10,234
- Status: Fully operational

Need help? Run 'claude-self-reflect doctor'
```

## Implementation Approach

### Phase 1: Core Detection Logic
- Implement installation state detection
- Add health check functionality
- Create recovery strategies

### Phase 2: Interactive UI
- Build progress indicator system
- Implement mode selection interface
- Add configuration wizard

### Phase 3: New Commands
- Implement `doctor` command
- Implement `upgrade` command
- Implement `reset` command

### Phase 4: Testing & Polish
- Test all error scenarios
- Add comprehensive help text
- Create video walkthrough

## Success Metrics

1. **Installation Success Rate**: >95% first-time success
2. **Upgrade Success Rate**: >90% without manual intervention
3. **Error Recovery Rate**: >80% automatic recovery
4. **User Satisfaction**: Positive feedback on ease of use
5. **Support Tickets**: 50% reduction in installation issues

## Timeline

- Week 1: Core detection and recovery logic
- Week 2: Interactive UI and progress system
- Week 3: New CLI commands
- Week 4: Testing and documentation

## Future Enhancements

1. **GUI Installer**: Electron-based visual installer
2. **Cloud Backup**: Optional encrypted cloud backup
3. **Multi-User Setup**: Team installation mode
4. **Plugin System**: Extensible setup for custom needs