#!/usr/bin/env node
/**
 * Claude Code Statusline Integration Setup
 * Automatically configures CC statusline to show CSR metrics
 */

import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';
import { execSync } from 'child_process';
import os from 'os';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

class StatuslineSetup {
    constructor() {
        this.homeDir = os.homedir();
        this.claudeDir = path.join(this.homeDir, '.claude');
        this.csrScript = path.join(path.dirname(__dirname), 'scripts', 'csr-status');
        this.globalBin = '/usr/local/bin/csr-status';
        this.statuslineWrapper = path.join(this.claudeDir, 'statusline-wrapper.sh');
        this.statuslineBackup = path.join(this.claudeDir, 'statusline-wrapper.sh.backup');
    }

    checkCCStatusline() {
        try {
            execSync('npm list -g cc-statusline', { stdio: 'ignore' });
            this.log('cc-statusline is installed', 'success');
            return true;
        } catch {
            this.log('cc-statusline not found', 'warning');
            return false;
        }
    }

    installCCStatusline() {
        if (this.checkCCStatusline()) {
            return true;
        }

        this.log('Installing cc-statusline...', 'info');
        try {
            execSync('npm install -g cc-statusline', { stdio: 'inherit' });
            this.log('cc-statusline installed successfully', 'success');
            return true;
        } catch (error) {
            this.log(`Failed to install cc-statusline: ${error.message}`, 'error');
            this.log('Statusline features will not be available', 'warning');
            return false;
        }
    }

    log(message, type = 'info') {
        const colors = {
            info: '\x1b[36m',
            success: '\x1b[32m',
            warning: '\x1b[33m',
            error: '\x1b[31m'
        };
        console.log(`${colors[type]}${message}\x1b[0m`);
    }

    checkPrerequisites() {
        // Check npm is available
        try {
            execSync('npm --version', { stdio: 'ignore' });
        } catch {
            this.log('npm is required but not found', 'error');
            this.log('Please install Node.js and npm from nodejs.org', 'error');
            return false;
        }

        // Check if Claude Code directory exists
        if (!fs.existsSync(this.claudeDir)) {
            this.log('Claude Code directory not found. Please ensure Claude Code is installed.', 'warning');
            return false;
        }

        // Check if csr-status script exists
        if (!fs.existsSync(this.csrScript)) {
            this.log('CSR status script not found. Please ensure the package is installed correctly.', 'error');
            return false;
        }

        return true;
    }

    installGlobalCommand() {
        try {
            // Check if we need sudo
            const needsSudo = !this.canWriteTo('/usr/local/bin');

            if (fs.existsSync(this.globalBin)) {
                // Check if it's already pointing to our script
                try {
                    const target = fs.readlinkSync(this.globalBin);
                    if (target === this.csrScript) {
                        this.log('Global csr-status command already installed', 'success');
                        return true;
                    }
                } catch (e) {
                    // Not a symlink or can't read, will replace
                }
            }

            // Try user-local installation first (no sudo needed)
            const userBin = path.join(this.homeDir, 'bin');
            const userLocalBin = path.join(userBin, 'csr-status');

            if (!fs.existsSync(userBin)) {
                fs.mkdirSync(userBin, { recursive: true });
            }

            // Create symlink in ~/bin
            try {
                if (fs.existsSync(userLocalBin)) {
                    fs.unlinkSync(userLocalBin);
                }
                fs.symlinkSync(this.csrScript, userLocalBin);
                // Note: Symlink permissions don't matter - source script permissions are used

                this.log('csr-status installed to ~/bin (no sudo required)', 'success');
                this.log('Add ~/bin to PATH: export PATH="$HOME/bin:$PATH"', 'info');

                // Check if ~/bin is in PATH
                const pathDirs = process.env.PATH.split(':');
                if (!pathDirs.includes(userBin) && !pathDirs.includes('~/bin')) {
                    this.log('⚠️ Add this to ~/.bashrc or ~/.zshrc:', 'warning');
                    this.log('   export PATH="$HOME/bin:$PATH"', 'info');
                }

                return true;
            } catch (userError) {
                this.log(`User-local install failed: ${userError.message}`, 'warning');
            }

            // Fallback to global install if user has sudo access
            if (needsSudo) {
                this.log('Attempting global install (requires sudo)...', 'info');
                this.log('Corporate machines may not allow this - using user-local is fine', 'info');
            }

            // Create symlink
            const cmd = needsSudo
                ? `sudo ln -sf "${this.csrScript}" "${this.globalBin}"`
                : `ln -sf "${this.csrScript}" "${this.globalBin}"`;

            execSync(cmd, { stdio: 'inherit' });

            // Make executable
            const chmodCmd = needsSudo
                ? `sudo chmod +x "${this.globalBin}"`
                : `chmod +x "${this.globalBin}"`;
            execSync(chmodCmd);

            this.log('Global csr-status command installed successfully', 'success');
            return true;
        } catch (error) {
            this.log('Statusline installation skipped (no sudo access)', 'info');
            this.log('This is normal on corporate machines', 'info');
            this.log('✅ Core MCP search works fine without statusline!', 'success');
            return false;  // Return false but don't treat as critical error
        }
    }

    canWriteTo(dir) {
        try {
            fs.accessSync(dir, fs.constants.W_OK);
            return true;
        } catch {
            return false;
        }
    }

    patchStatuslineWrapper() {
        if (!fs.existsSync(this.statuslineWrapper)) {
            this.log('Claude Code statusline wrapper not found. Statusline integration skipped.', 'warning');
            return false;
        }

        try {
            // Read current wrapper
            let content = fs.readFileSync(this.statuslineWrapper, 'utf8');

            // Check if already patched with new approach
            if (content.includes('CSR compact status instead of MCP bar')) {
                this.log('Statusline wrapper already patched', 'success');
                return true;
            }

            // Create backup
            if (!fs.existsSync(this.statuslineBackup)) {
                fs.copyFileSync(this.statuslineWrapper, this.statuslineBackup);
                this.log(`Backup created: ${this.statuslineBackup}`, 'info');
            }

            // Find and replace the MCP bar generation section
            const barPattern = /# Create mini progress bar[\s\S]*?MCP_COLOR="\\033\[1;90m"  # Gray/;

            if (content.match(barPattern)) {
                // Replace the entire bar generation section
                const replacement = `# Use CSR compact status instead of MCP bar
    # This shows both import percentage and code quality in format: [100% <time>][🟢:A+]
    CSR_COMPACT=$(csr-status --compact 2>/dev/null || echo "")

    if [[ -n "$CSR_COMPACT" ]]; then
        MCP_STATUS="$CSR_COMPACT"

        # Color based on content
        if [[ "$CSR_COMPACT" == *"100%"* ]]; then
            MCP_COLOR="\\033[1;32m"  # Green for complete
        elif [[ "$CSR_COMPACT" == *"[🟢:"* ]]; then
            MCP_COLOR="\\033[1;32m"  # Green for good quality
        elif [[ "$CSR_COMPACT" == *"[🟡:"* ]]; then
            MCP_COLOR="\\033[1;33m"  # Yellow for medium quality
        elif [[ "$CSR_COMPACT" == *"[🔴:"* ]]; then
            MCP_COLOR="\\033[1;31m"  # Red for poor quality
        else
            MCP_COLOR="\\033[1;36m"  # Cyan default
        fi
    else
        MCP_STATUS=""
        MCP_COLOR="\\033[1;90m"  # Gray`;

                content = content.replace(barPattern, replacement);

                // Write back
                fs.writeFileSync(this.statuslineWrapper, content);
                this.log('Statusline wrapper patched successfully (replaced MCP bar)', 'success');
                return true;
            } else {
                // Fallback: Look for the PERCENTAGE check
                const altPattern = /if \[\[ "\$PERCENTAGE" != "null"[\s\S]*?MCP_COLOR="\\033\[1;90m"  # Gray/;

                if (content.match(altPattern)) {
                    const replacement = `# Use CSR compact status instead of MCP bar
    # This shows both import percentage and code quality in format: [100% <time>][🟢:A+]
    CSR_COMPACT=$(csr-status --compact 2>/dev/null || echo "")

    if [[ -n "$CSR_COMPACT" ]]; then
        MCP_STATUS="$CSR_COMPACT"

        # Color based on content
        if [[ "$CSR_COMPACT" == *"100%"* ]]; then
            MCP_COLOR="\\033[1;32m"  # Green for complete
        elif [[ "$CSR_COMPACT" == *"[🟢:"* ]]; then
            MCP_COLOR="\\033[1;32m"  # Green for good quality
        elif [[ "$CSR_COMPACT" == *"[🟡:"* ]]; then
            MCP_COLOR="\\033[1;33m"  # Yellow for medium quality
        elif [[ "$CSR_COMPACT" == *"[🔴:"* ]]; then
            MCP_COLOR="\\033[1;31m"  # Red for poor quality
        else
            MCP_COLOR="\\033[1;36m"  # Cyan default
        fi
    else
        MCP_STATUS=""
        MCP_COLOR="\\033[1;90m"  # Gray`;

                    content = content.replace(altPattern, replacement);

                    // Write back
                    fs.writeFileSync(this.statuslineWrapper, content);
                    this.log('Statusline wrapper patched successfully (replaced PERCENTAGE section)', 'success');
                    return true;
                } else {
                    this.log('Could not find MCP bar section to replace', 'warning');
                    return false;
                }
            }
        } catch (error) {
            this.log(`Failed to patch statusline wrapper: ${error.message}`, 'error');
            return false;
        }
    }

    validateIntegration() {
        try {
            // Test csr-status command
            const output = execSync('csr-status --compact', { encoding: 'utf8' });
            if (output) {
                this.log(`CSR status output: ${output.trim()}`, 'success');
                return true;
            }
        } catch (error) {
            this.log('CSR status command not working properly', 'warning');
            return false;
        }
        return false;
    }

    async run() {
        this.log('🚀 Setting up Claude Code Statusline Integration...', 'info');

        if (!this.checkPrerequisites()) {
            this.log('Prerequisites check failed', 'error');
            return false;
        }

        const steps = [
            { name: 'Install cc-statusline', fn: () => this.installCCStatusline() },
            { name: 'Install global command', fn: () => this.installGlobalCommand() },
            { name: 'Patch statusline wrapper', fn: () => this.patchStatuslineWrapper() },
            { name: 'Validate integration', fn: () => this.validateIntegration() }
        ];

        let success = true;
        for (const step of steps) {
            this.log(`\n📋 ${step.name}...`, 'info');
            if (!step.fn()) {
                success = false;
                this.log(`❌ ${step.name} failed`, 'error');
            }
        }

        if (success) {
            this.log('\n✅ Statusline integration completed successfully!', 'success');
            this.log('The CSR status will now appear in your Claude Code statusline.', 'info');
            this.log('Format: [import%][🟢:grade] for compact quality metrics', 'info');
        } else {
            this.log('\n⚠️ Statusline integration completed with warnings', 'warning');
            this.log('Some features may need manual configuration.', 'warning');
        }

        return success;
    }

    // Restore original statusline if needed
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
        } else {
            this.log('No backup found to restore', 'warning');
            return false;
        }
    }
}

// Run if called directly
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