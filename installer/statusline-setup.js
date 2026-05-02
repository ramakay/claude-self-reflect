#!/usr/bin/env node
/**
 * Claude Code Statusline Integration Setup
 * Configures CC statusline to show CSR metrics via `csr-engine status --compact`.
 */

import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import { execSync } from 'child_process';
import os from 'os';

const __filename = fileURLToPath(import.meta.url);

class StatuslineSetup {
    constructor() {
        this.homeDir = os.homedir();
        this.claudeDir = path.join(this.homeDir, '.claude');
        this.statuslineWrapper = path.join(this.claudeDir, 'statusline-wrapper.sh');
        this.statuslineBackup = path.join(this.claudeDir, 'statusline-wrapper.sh.backup');
    }

    log(message, type = 'info') {
        const colors = {
            info: '\x1b[36m',
            success: '\x1b[32m',
            warning: '\x1b[33m',
            error: '\x1b[31m'
        };
        console.log(`${colors[type] || ''}${message}\x1b[0m`);
    }

    checkBinary() {
        try {
            execSync('csr-engine status --compact', { encoding: 'utf8', stdio: 'pipe' });
            this.log('csr-engine binary found and working', 'success');
            return true;
        } catch {
            this.log('csr-engine not found in PATH', 'warning');
            this.log('Install it: curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh', 'info');
            return false;
        }
    }

    patchStatuslineWrapper() {
        if (!fs.existsSync(this.statuslineWrapper)) {
            this.log('Claude Code statusline wrapper not found. Skipped.', 'warning');
            return false;
        }

        try {
            let content = fs.readFileSync(this.statuslineWrapper, 'utf8');

            // Already patched with csr-engine?
            if (content.includes('csr-engine status --compact')) {
                this.log('Statusline wrapper already configured for csr-engine', 'success');
                return true;
            }

            // Create backup
            if (!fs.existsSync(this.statuslineBackup)) {
                fs.copyFileSync(this.statuslineWrapper, this.statuslineBackup);
                this.log(`Backup created: ${this.statuslineBackup}`, 'info');
            }

            // Replace old csr-status references with csr-engine status --compact
            const csrReplacement = `# CSR compact status via csr-engine
    CSR_COMPACT=$(csr-engine status --compact 2>/dev/null || echo "")

    if [[ -n "$CSR_COMPACT" ]]; then
        MCP_STATUS="$CSR_COMPACT"
        MCP_COLOR="\\033[1;32m"  # Green
    else
        MCP_STATUS=""
        MCP_COLOR="\\033[1;90m"  # Gray`;

            // Try to replace the MCP bar generation section
            const patterns = [
                /# Use CSR compact status instead of MCP bar[\s\S]*?MCP_COLOR="\\033\[1;90m"  # Gray/,
                /# Create mini progress bar[\s\S]*?MCP_COLOR="\\033\[1;90m"  # Gray/,
                /if \[\[ "\$PERCENTAGE" != "null"[\s\S]*?MCP_COLOR="\\033\[1;90m"  # Gray/,
            ];

            let patched = false;
            for (const pattern of patterns) {
                if (content.match(pattern)) {
                    content = content.replace(pattern, csrReplacement);
                    patched = true;
                    break;
                }
            }

            if (patched) {
                fs.writeFileSync(this.statuslineWrapper, content);
                this.log('Statusline wrapper patched to use csr-engine', 'success');
                return true;
            }

            this.log('Could not find MCP bar section to patch', 'warning');
            return false;
        } catch (error) {
            this.log(`Failed to patch statusline wrapper: ${error.message}`, 'error');
            return false;
        }
    }

    async run() {
        this.log('Setting up Claude Code Statusline Integration...', 'info');

        if (!fs.existsSync(this.claudeDir)) {
            this.log('Claude Code directory not found. Please install Claude Code first.', 'error');
            return false;
        }

        const binaryOk = this.checkBinary();
        if (!binaryOk) {
            this.log('Install csr-engine first, then retry.', 'warning');
            return false;
        }

        const patched = this.patchStatuslineWrapper();

        if (patched) {
            this.log('\nStatusline integration complete!', 'success');
            this.log('CSR metrics will appear in your Claude Code statusline.', 'info');
        } else {
            this.log('\nStatusline wrapper not found or already configured.', 'warning');
            this.log('You can check status manually: csr-engine status --compact', 'info');
        }

        return patched;
    }

    restore() {
        if (fs.existsSync(this.statuslineBackup)) {
            try {
                fs.copyFileSync(this.statuslineBackup, this.statuslineWrapper);
                this.log('Statusline wrapper restored from backup', 'success');
                return true;
            } catch (error) {
                this.log(`Failed to restore: ${error.message}`, 'error');
                return false;
            }
        }
        this.log('No backup found to restore', 'warning');
        return false;
    }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
    const setup = new StatuslineSetup();
    if (process.argv[2] === '--restore') {
        setup.restore();
    } else {
        setup.run().catch(error => {
            console.error('Setup failed:', error);
            process.exit(1);
        });
    }
}

export default StatuslineSetup;
