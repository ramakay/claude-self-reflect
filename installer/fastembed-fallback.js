#!/usr/bin/env node
/**
 * FastEmbed Fallback Installer
 * Automatically detects SSL/proxy issues and downloads model from Google Cloud Storage
 */

import fs from 'fs';
import path from 'path';
import { execSync, spawnSync } from 'child_process';
import https from 'https';
import os from 'os';

class FastEmbedFallback {
    constructor() {
        this.homeDir = os.homedir();
        this.cacheDir = path.join(this.homeDir, '.cache', 'fastembed');
        this.modelName = 'sentence-transformers-all-MiniLM-L6-v2';
        this.modelFile = `${this.modelName}.tar.gz`;
        this.gcsUrl = `https://storage.googleapis.com/qdrant-fastembed/${this.modelFile}`;
    }

    log(message, type = 'info') {
        const colors = {
            info: '\x1b[36m',
            success: '\x1b[32m',
            warning: '\x1b[33m',
            error: '\x1b[31m'
        };
        const prefix = {
            info: 'ℹ',
            success: '✓',
            warning: '⚠',
            error: '✗'
        };
        console.log(`${colors[type]}${prefix[type]} ${message}\x1b[0m`);
    }

    checkModelExists() {
        const modelPath = path.join(this.cacheDir, this.modelName);
        return fs.existsSync(modelPath);
    }

    async testHuggingFace() {
        this.log('Testing HuggingFace connectivity...', 'info');
        try {
            execSync('curl -s -m 5 https://huggingface.co > /dev/null 2>&1', { timeout: 5000 });
            this.log('HuggingFace is accessible', 'success');
            return true;
        } catch (error) {
            this.log('HuggingFace blocked by proxy/firewall', 'warning');
            return false;
        }
    }

    downloadFromGCS() {
        this.log('Downloading FastEmbed model from Google Cloud Storage...', 'info');
        this.log('(Using GCS mirror to bypass corporate proxies)', 'info');

        // Create cache directory
        if (!fs.existsSync(this.cacheDir)) {
            fs.mkdirSync(this.cacheDir, { recursive: true });
        }

        const tarPath = path.join(this.cacheDir, this.modelFile);

        try {
            // Download with curl (handles proxies better than Node's https)
            // SECURITY: Use array-based arguments to prevent shell injection
            this.log(`Downloading ${this.modelFile} (79MB)...`, 'info');
            const curlResult = spawnSync('curl', ['-L', '-o', tarPath, this.gcsUrl], {
                stdio: 'inherit',
                timeout: 300000  // 5 minute timeout
            });

            if (curlResult.error || curlResult.status !== 0) {
                throw new Error(`curl failed: ${curlResult.error?.message || `exit code ${curlResult.status}`}`);
            }

            this.log('Download complete. Extracting...', 'success');

            // Extract (with 2 minute timeout to prevent hanging)
            // SECURITY: Use array-based arguments to prevent shell injection
            const tarResult = spawnSync('tar', ['-xzf', tarPath, '-C', this.cacheDir], {
                stdio: 'inherit',
                timeout: 120000  // 2 minute timeout
            });

            if (tarResult.error || tarResult.status !== 0) {
                throw new Error(`tar extraction failed: ${tarResult.error?.message || `exit code ${tarResult.status}`}`);
            }

            // Verify extraction
            if (this.checkModelExists()) {
                this.log('FastEmbed model installed successfully!', 'success');

                // Clean up tar file
                fs.unlinkSync(tarPath);

                return true;
            } else {
                this.log('Model extraction failed', 'error');
                return false;
            }
        } catch (error) {
            this.log(`Download failed: ${error.message}`, 'error');
            return false;
        }
    }

    configureDockerCompose() {
        this.log('Configuring docker-compose for offline model...', 'info');

        const dockerComposePath = path.join(process.cwd(), 'docker-compose.yaml');

        if (!fs.existsSync(dockerComposePath)) {
            this.log('docker-compose.yaml not found, skipping configuration', 'warning');
            return false;
        }

        try {
            const content = fs.readFileSync(dockerComposePath, 'utf8');

            // Check if already configured
            if (content.includes('HF_HUB_OFFLINE')) {
                this.log('docker-compose already configured for offline mode', 'success');
                return true;
            }

            // Line-by-line processing is more reliable than regex for YAML
            const lines = content.split('\n');
            const services = ['importer', 'watcher', 'streaming-importer', 'async-importer', 'safe-watcher', 'mcp-server'];
            let currentService = null;
            let inEnvironment = false;
            let environmentIndent = '';

            for (let i = 0; i < lines.length; i++) {
                const line = lines[i];
                const trimmed = line.trim();

                // Track which service we're in
                for (const service of services) {
                    if (trimmed === `${service}:`) {
                        currentService = service;
                        inEnvironment = false;
                        break;
                    }
                }

                // Detect environment section
                if (currentService && trimmed === 'environment:') {
                    inEnvironment = true;
                    environmentIndent = line.match(/^(\s+)/)?.[1] || '    ';
                }

                // Add HF_HUB_OFFLINE after first environment entry
                if (inEnvironment && trimmed.startsWith('-') && !content.includes('HF_HUB_OFFLINE')) {
                    lines.splice(i + 1, 0, `${environmentIndent}  - HF_HUB_OFFLINE=1`);
                    inEnvironment = false;
                }

                // Add volume mount after volumes section
                if (currentService && trimmed === 'volumes:' && !content.includes('.cache/fastembed')) {
                    const volumeIndent = line.match(/^(\s+)/)?.[1] || '    ';
                    // Find next line with volume entry
                    if (lines[i + 1] && lines[i + 1].trim().startsWith('-')) {
                        lines.splice(i + 2, 0, `${volumeIndent}  - ~/.cache/fastembed:/root/.cache/fastembed:ro`);
                    }
                }

                // Reset when we exit a service (next service or same-level key)
                if (currentService && line.match(/^\s{0,2}\w+:/) && !trimmed.startsWith(`${currentService}:`)) {
                    currentService = null;
                    inEnvironment = false;
                }
            }

            // Write back
            fs.writeFileSync(dockerComposePath, lines.join('\n'));
            this.log('docker-compose.yaml updated for offline mode', 'success');
            return true;
        } catch (error) {
            this.log(`Failed to update docker-compose: ${error.message}`, 'error');
            return false;
        }
    }

    configureMCPServer() {
        this.log('Configuring MCP server for offline model...', 'info');

        const mcpRunScript = path.join(process.cwd(), 'mcp-server', 'run-mcp.sh');

        if (!fs.existsSync(mcpRunScript)) {
            this.log('run-mcp.sh not found, skipping configuration', 'warning');
            return false;
        }

        try {
            let content = fs.readFileSync(mcpRunScript, 'utf8');

            // Check if already configured
            if (content.includes('HF_HUB_OFFLINE')) {
                this.log('MCP server already configured for offline mode', 'success');
                return true;
            }

            // Add exports at the beginning
            const exports = `
# Offline FastEmbed configuration (auto-added by installer)
export HF_HUB_OFFLINE=1
export FASTEMBED_CACHE_PATH="$HOME/.cache/fastembed"

`;

            // Insert after shebang
            content = content.replace(/(#!\/bin\/bash\n)/, `$1${exports}`);

            fs.writeFileSync(mcpRunScript, content);
            this.log('MCP server configured for offline mode', 'success');
            return true;
        } catch (error) {
            this.log(`Failed to update run-mcp.sh: ${error.message}`, 'error');
            return false;
        }
    }

    async run() {
        this.log('🔍 Checking FastEmbed model availability...', 'info');

        // Check if model already exists
        if (this.checkModelExists()) {
            this.log('FastEmbed model already installed ✓', 'success');
            return true;
        }

        // Test HuggingFace connectivity
        const hfAccessible = await this.testHuggingFace();

        if (!hfAccessible) {
            this.log('Corporate proxy detected - using Google Cloud Storage mirror', 'warning');

            // Download from GCS
            if (!this.downloadFromGCS()) {
                this.log('Failed to download model from GCS', 'error');
                return false;
            }

            // Configure for offline use
            this.configureDockerCompose();
            this.configureMCPServer();

            this.log('✅ FastEmbed configured for offline use', 'success');
            this.log('Your installation will now work behind corporate proxies', 'info');
            return true;
        }

        // HuggingFace is accessible, let Python handle the download
        this.log('HuggingFace accessible - standard installation will work', 'info');
        return true;
    }
}

// Run if called directly
if (import.meta.url === `file://${process.argv[1]}`) {
    const fallback = new FastEmbedFallback();
    fallback.run().catch(error => {
        console.error('FastEmbed fallback failed:', error);
        process.exit(1);
    });
}

export default FastEmbedFallback;
