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
            return { installed: true, name: 'cc-statusline', critical: true };
        } catch {
            return { installed: false, name: 'cc-statusline', critical: true, fix: () => this.installCCStatusline() };
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
            return { installed: true, name: 'AST-Grep', critical: true };
        } catch {
            // Try 'sg' binary (npm install -g @ast-grep/cli)
            try {
                execSync('sg --version', { stdio: 'ignore' });
                return { installed: true, name: 'AST-Grep (sg)', critical: true };
            } catch {
                return {
                    installed: false,
                    name: 'AST-Grep',
                    critical: true,
                    fix: () => this.installASTGrep()
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
        // Check if fetch is available (Node.js 18+)
        if (typeof fetch === 'undefined') {
            this.log('fetch not available, skipping Qdrant health check', 'warning');
            return {
                installed: false,
                name: 'Qdrant',
                critical: true,
                fix: () => this.startQdrant(),
                error: 'Cannot verify Qdrant (fetch not available)'
            };
        }

        const controller = new AbortController();
        const timeout = setTimeout(() => controller.abort(), 3000); // 3 second timeout

        try {
            const response = await fetch('http://localhost:6333', {
                signal: controller.signal
            });

            // Clear timeout immediately after successful fetch
            clearTimeout(timeout);

            if (response.ok) {
                // Timeout already cleared above, safe to return
                return { installed: true, name: 'Qdrant', critical: true };
            } else {
                // Timeout already cleared above, log and fall through
                this.log(`Qdrant responded with status: ${response.status}`, 'warning');
            }
        } catch (error) {
            clearTimeout(timeout);

            if (error.name === 'AbortError') {
                this.log('Qdrant health check timed out after 3 seconds', 'warning');
            } else {
                this.log(`Qdrant health check failed: ${error.message}`, 'warning');
            }
        }

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

    async installASTGrep() {
        // Try npm installation first (works on all platforms)
        this.log('Installing AST-Grep via npm...', 'info');
        try {
            execSync('npm install -g @ast-grep/cli', { stdio: 'inherit' });
            this.log('AST-Grep installed successfully', 'success');
            return true;
        } catch (npmError) {
            // Check for permission errors
            const isPermissionError = npmError.code === 'EACCES' ||
                                     npmError.code === 'EPERM' ||
                                     (npmError.stderr && npmError.stderr.toString().includes('EACCES')) ||
                                     (npmError.message && npmError.message.includes('permission'));

            if (isPermissionError) {
                this.log('Failed to install AST-Grep via npm: Permission denied', 'error');
                this.log('Alternative installation methods:', 'info');
                this.log('  1. With sudo: sudo npm install -g @ast-grep/cli', 'info');
                this.log('  2. With brew: brew install ast-grep  (macOS/Linux)', 'info');
                this.log('  3. Use nvm for user-local npm installs', 'info');
                return false;
            }

            // If npm fails for other reasons, try suggesting brew on macOS
            if (process.platform === 'darwin') {
                this.log('npm installation failed. Checking for Homebrew...', 'warning');
                try {
                    execSync('brew --version', { stdio: 'ignore' });
                    this.log('Install AST-Grep with: brew install ast-grep', 'info');
                } catch {
                    this.log('Homebrew not found. Install from: https://brew.sh', 'info');
                }
            }

            this.log(`AST-Grep installation failed: ${npmError.message}`, 'error');
            return false;
        }
    }

    async startQdrant() {
        this.log('Starting Qdrant...', 'info');

        // Try Docker Compose v2 first (docker compose)
        try {
            execSync('docker compose up -d qdrant', {
                cwd: this.packageRoot,
                stdio: 'inherit'
            });
            this.log('Qdrant started (Docker Compose v2)', 'success');
            return true;
        } catch (v2Error) {
            this.log('Docker Compose v2 failed, trying v1...', 'warning');

            // Fallback to Docker Compose v1 (docker-compose)
            try {
                execSync('docker-compose up -d qdrant', {
                    cwd: this.packageRoot,
                    stdio: 'inherit'
                });
                this.log('Qdrant started (Docker Compose v1)', 'success');
                return true;
            } catch (v1Error) {
                this.log(`Failed to start Qdrant with both v1 and v2: ${v1Error.message}`, 'error');
                return false;
            }
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

        // Run all checks with Promise.allSettled to prevent throw on first failure
        // Store checks as objects with name property for maintainability
        const checks = [
            { name: 'Docker', fn: () => this.checkDocker() },
            { name: 'Qdrant', fn: () => this.checkQdrant() },
            { name: 'FastEmbed', fn: () => this.checkFastEmbedModel() },
            { name: 'Docker Config', fn: () => this.checkDockerComposeConfig() },
            { name: 'cc-statusline', fn: () => this.checkCCStatusline() },
            { name: 'csr-status', fn: () => this.checkCSRStatusScript() },
            { name: 'AST-Grep', fn: () => this.checkASTGrep() }
        ];

        const settledResults = await Promise.allSettled(checks.map(c => c.fn()));

        // Convert settled results to standard format, treating rejections as failures
        const results = settledResults.map((result, index) => {
            if (result.status === 'fulfilled') {
                return result.value;
            } else {
                // Rejected check - treat as critical failure
                this.log(
                    `Check failed: ${checks[index].name} - ${result.reason?.message || result.reason}`,
                    'error'
                );
                return {
                    installed: false,
                    critical: true,
                    name: checks[index].name,
                    error: `Check threw error: ${result.reason?.message || result.reason}`
                };
            }
        });

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
                    } else {
                        // Re-verify the fix worked by re-running the check
                        this.log(`Verifying fix for ${issue.name}...`, 'info');
                        const recheckName = issue.name.toLowerCase();
                        let recheckResult;

                        if (recheckName.includes('docker') && recheckName.includes('config')) {
                            // Docker config specific check
                            recheckResult = await this.checkDockerComposeConfig();
                        } else if (recheckName.includes('docker') && !recheckName.includes('config')) {
                            recheckResult = await this.checkDocker();
                        } else if (recheckName.includes('qdrant')) {
                            // Give Qdrant a moment to come online before rechecking
                            this.log('Waiting 5 seconds for Qdrant to start...', 'info');
                            await new Promise(resolve => setTimeout(resolve, 5000));
                            recheckResult = await this.checkQdrant();
                        } else if (recheckName.includes('fastembed')) {
                            recheckResult = await this.checkFastEmbedModel();
                        } else if (recheckName.includes('cc-statusline')) {
                            recheckResult = await this.checkCCStatusline();
                        } else if (recheckName.includes('csr-status')) {
                            recheckResult = await this.checkCSRStatusScript();
                        } else if (recheckName.includes('ast-grep')) {
                            recheckResult = await this.checkASTGrep();
                        }

                        // Guard against undefined recheckResult (no matching verifier)
                        if (recheckResult === undefined) {
                            this.log(`No verifier found for ${issue.name} - cannot verify fix`, 'error');
                            unresolvedCritical.push(issue);
                        } else if (!recheckResult.installed) {
                            this.log(`Fix verification failed for ${issue.name}`, 'error');
                            unresolvedCritical.push(issue);
                        }
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
