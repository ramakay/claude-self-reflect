#!/usr/bin/env node
/**
 * Update Manager
 * Detects and fixes missing features, updates configurations
 * Ensures user's installation matches the package capabilities
 */

import { execSync } from 'child_process';
import fs from 'fs';
import path from 'path';
import os from 'os';
import { fileURLToPath } from 'url';
import StatuslineSetup from './statusline-setup.js';
import FastEmbedFallback from './fastembed-fallback.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

class UpdateManager {
    constructor() {
        this.homeDir = os.homedir();
        this.packageRoot = path.dirname(__dirname);
    }

    log(message, type = 'info') {
        const colors = {
            info: '\x1b[36m',
            success: '\x1b[32m',
            warning: '\x1b[33m',
            error: '\x1b[31m'
        };
        const icons = {
            info: 'ℹ',
            success: '✓',
            warning: '⚠',
            error: '✗'
        };
        console.log(`${colors[type]}${icons[type]} ${message}\x1b[0m`);
    }

    // Feature checks
    async checkCCStatusline() {
        try {
            execSync('npm list -g cc-statusline', { stdio: 'ignore' });
            return { installed: true, name: 'cc-statusline', critical: false };
        } catch {
            return { installed: false, name: 'cc-statusline', critical: false, fix: () => this.installCCStatusline() };
        }
    }

    async checkCSRStatusScript() {
        const userBin = path.join(this.homeDir, 'bin', 'csr-status');
        const globalBin = '/usr/local/bin/csr-status';

        const exists = fs.existsSync(userBin) || fs.existsSync(globalBin);
        return {
            installed: exists,
            name: 'csr-status command',
            critical: false,
            fix: () => this.installCSRStatus()
        };
    }

    async checkFastEmbedModel() {
        const modelPath = path.join(this.homeDir, '.cache', 'fastembed', 'sentence-transformers-all-MiniLM-L6-v2');
        const exists = fs.existsSync(modelPath);

        return {
            installed: exists,
            name: 'FastEmbed model',
            critical: true,
            fix: () => this.installFastEmbed()
        };
    }

    async checkASTGrep() {
        // Check for both 'ast-grep' (brew) and 'sg' (npm) binaries
        try {
            execSync('ast-grep --version', { stdio: 'ignore' });
            return { installed: true, name: 'AST-Grep', critical: false };
        } catch {
            // Try 'sg' binary (npm install -g @ast-grep/cli)
            try {
                execSync('sg --version', { stdio: 'ignore' });
                return { installed: true, name: 'AST-Grep (sg)', critical: false };
            } catch {
                return {
                    installed: false,
                    name: 'AST-Grep (optional)',
                    critical: false,
                    fix: () => this.suggestASTGrep()
                };
            }
        }
    }

    async checkDocker() {
        try {
            execSync('docker info', { stdio: 'ignore' });
            return { installed: true, name: 'Docker', critical: true };
        } catch {
            return {
                installed: false,
                name: 'Docker',
                critical: true,
                fix: null,
                error: 'Docker is required. Install Docker Desktop from docker.com'
            };
        }
    }

    async checkQdrant() {
        try {
            const response = await fetch('http://localhost:6333');
            if (response.ok) {
                return { installed: true, name: 'Qdrant', critical: true };
            }
        } catch {}

        return {
            installed: false,
            name: 'Qdrant',
            critical: true,
            fix: () => this.startQdrant()
        };
    }

    async checkDockerComposeConfig() {
        const composePath = path.join(this.packageRoot, 'docker-compose.yaml');

        if (!fs.existsSync(composePath)) {
            return { installed: false, name: 'docker-compose.yaml', critical: true, fix: null };
        }

        const content = fs.readFileSync(composePath, 'utf8');

        // Check for offline FastEmbed configuration
        const hasOfflineConfig = content.includes('HF_HUB_OFFLINE') && content.includes('fastembed:ro');

        return {
            installed: hasOfflineConfig,
            name: 'Docker offline FastEmbed config',
            critical: false,
            fix: () => this.fixDockerConfig()
        };
    }

    // Fix functions
    async installCCStatusline() {
        // Check npm is available
        try {
            execSync('npm --version', { stdio: 'ignore' });
        } catch {
            this.log('npm is required but not found', 'error');
            this.log('Please install Node.js and npm from nodejs.org', 'error');
            return false;
        }

        this.log('Installing cc-statusline...', 'info');
        try {
            execSync('npm install -g cc-statusline', { stdio: 'inherit' });
            this.log('cc-statusline installed', 'success');
            return true;
        } catch (error) {
            // Check for permission errors
            const isPermissionError = error.code === 'EACCES' ||
                                     error.code === 'EPERM' ||
                                     (error.stderr && error.stderr.toString().includes('EACCES')) ||
                                     (error.message && error.message.includes('permission'));

            if (isPermissionError) {
                this.log('Failed to install cc-statusline: Permission denied', 'error');
                this.log('Try one of:', 'info');
                this.log('  1. Run with elevated privileges: sudo npm install -g cc-statusline', 'info');
                this.log('  2. Use a node version manager like nvm', 'info');
                this.log('  3. Configure npm for user-local installs', 'info');
            } else {
                this.log(`Failed to install cc-statusline: ${error.message}`, 'error');
            }
            return false;
        }
    }

    async installCSRStatus() {
        this.log('Setting up csr-status command...', 'info');
        const statuslineSetup = new StatuslineSetup();
        return await statuslineSetup.installGlobalCommand();
    }

    async installFastEmbed() {
        this.log('Setting up FastEmbed model...', 'info');
        const fallback = new FastEmbedFallback();
        return await fallback.run();
    }

    async suggestASTGrep() {
        this.log('AST-Grep is optional but recommended for code quality features', 'info');
        this.log('Install with: brew install ast-grep  (macOS)', 'info');
        this.log('         or: npm install -g @ast-grep/cli', 'info');
        return true;  // Not critical
    }

    async startQdrant() {
        this.log('Starting Qdrant...', 'info');
        try {
            execSync('docker compose up -d qdrant', {
                cwd: this.packageRoot,
                stdio: 'inherit'
            });
            this.log('Qdrant started', 'success');
            return true;
        } catch (error) {
            this.log(`Failed to start Qdrant: ${error.message}`, 'error');
            return false;
        }
    }

    async fixDockerConfig() {
        this.log('Updating Docker Compose configuration...', 'info');
        const fallback = new FastEmbedFallback();
        return await fallback.configureDockerCompose();
    }

    async run() {
        this.log('Analyzing installation...', 'info');
        console.log();

        // Run all checks
        const checks = [
            this.checkDocker(),
            this.checkQdrant(),
            this.checkFastEmbedModel(),
            this.checkDockerComposeConfig(),
            this.checkCCStatusline(),
            this.checkCSRStatusScript(),
            this.checkASTGrep()
        ];

        const results = await Promise.all(checks);

        // Categorize results
        const missing = results.filter(r => !r.installed);
        const critical = missing.filter(r => r.critical);
        const optional = missing.filter(r => !r.critical);

        // Display status
        console.log('📊 Installation Status:\n');
        for (const result of results) {
            const icon = result.installed ? '✅' : (result.critical ? '❌' : '⚠️ ');
            const status = result.installed ? 'Installed' : 'Missing';
            console.log(`${icon} ${result.name}: ${status}`);
        }
        console.log();

        // Handle critical issues
        const unresolvedCritical = [];
        if (critical.length > 0) {
            this.log(`Found ${critical.length} critical issue(s) that need fixing`, 'error');
            console.log();

            for (const issue of critical) {
                if (issue.error) {
                    this.log(issue.error, 'error');
                    unresolvedCritical.push(issue);
                } else if (issue.fix) {
                    this.log(`Fixing: ${issue.name}...`, 'info');
                    const success = await issue.fix();
                    if (!success) {
                        this.log(`Failed to fix: ${issue.name}`, 'error');
                        unresolvedCritical.push(issue);
                    }
                } else {
                    // Issue has no fix and no error message - track as unresolved
                    this.log(`${issue.name} is missing (no automatic fix available)`, 'error');
                    unresolvedCritical.push(issue);
                }
            }
        }

        // Handle optional issues
        if (optional.length > 0) {
            this.log(`Found ${optional.length} optional feature(s) to install`, 'warning');
            console.log();

            for (const issue of optional) {
                if (issue.fix) {
                    this.log(`Installing: ${issue.name}...`, 'info');
                    await issue.fix();
                }
            }
        }

        // Final status
        console.log();
        if (unresolvedCritical.length === 0 && optional.length === 0) {
            this.log('All features are up to date! ✨', 'success');
        } else if (unresolvedCritical.length === 0) {
            this.log('Core features are working. Optional features installed.', 'success');
        } else {
            this.log('Please address critical issues before using Claude Self-Reflect', 'error');
            process.exit(1);
        }

        console.log();
        this.log('Run "claude-self-reflect doctor" for detailed diagnostics', 'info');
    }
}

// Run if called directly
if (import.meta.url === `file://${process.argv[1]}`) {
    const manager = new UpdateManager();
    manager.run().catch(error => {
        console.error('Update failed:', error);
        process.exit(1);
    });
}

export default UpdateManager;
