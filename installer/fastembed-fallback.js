#!/usr/bin/env node
/**
 * FastEmbed Fallback Installer
 * Automatically detects SSL/proxy issues and downloads model from Google Cloud Storage
 */

import fs from 'fs';
import path from 'path';
import { execSync } from 'child_process';
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
            this.log(`Downloading ${this.modelFile} (79MB)...`, 'info');
            execSync(`curl -L -o "${tarPath}" "${this.gcsUrl}"`, {
                stdio: 'inherit',
                timeout: 300000  // 5 minute timeout
            });

            this.log('Download complete. Extracting...', 'success');

            // Extract
            execSync(`tar -xzf "${tarPath}" -C "${this.cacheDir}"`, { stdio: 'inherit' });

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
            let content = fs.readFileSync(dockerComposePath, 'utf8');

            // Check if already configured
            if (content.includes('HF_HUB_OFFLINE')) {
                this.log('docker-compose already configured for offline mode', 'success');
                return true;
            }

            // Add cache mount and offline mode to all services that need it
            const services = ['importer', 'watcher', 'streaming-importer', 'async-importer', 'safe-watcher', 'mcp-server'];

            for (const service of services) {
                // Add volume mount for cache
                const volumePattern = new RegExp(`(\\s+${service}:[\\s\\S]*?volumes:[\\s\\S]*?)(\\s+-\\s+.*?\\n)(\\s+environment:)`, 'g');

                if (volumePattern.test(content)) {
                    content = content.replace(volumePattern, (match, p1, p2, p3) => {
                        if (!match.includes('.cache/fastembed')) {
                            return `${p1}${p2}      - ~/.cache/fastembed:/root/.cache/fastembed:ro\\n${p3}`;
                        }
                        return match;
                    });
                }

                // Add HF_HUB_OFFLINE environment variable
                const envPattern = new RegExp(`(\\s+${service}:[\\s\\S]*?environment:[\\s\\S]*?)(\\s+-\\s+.*?\\n)(\\s+restart:)`, 'g');

                if (envPattern.test(content)) {
                    content = content.replace(envPattern, (match, p1, p2, p3) => {
                        if (!match.includes('HF_HUB_OFFLINE')) {
                            return `${p1}${p2}      - HF_HUB_OFFLINE=1\\n${p3}`;
                        }
                        return match;
                    });
                }
            }

            // Write back
            fs.writeFileSync(dockerComposePath, content);
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
