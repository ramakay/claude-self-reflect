#!/usr/bin/env node

const { spawnSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const os = require('os');

console.log('🔄 Claude Self-Reflect: Checking for required migrations...');

const homeDir = os.homedir();
const csrConfigDir = path.join(homeDir, '.claude-self-reflect', 'config');
const unifiedStateFile = path.join(csrConfigDir, 'unified-state.json');
const legacyFiles = [
    'imported-files.json',
    'skipped_files.json',
    'failed_files.json',
    'import-status.json',
    'streaming-state.json'
];

// Check if migration is needed
const needsMigration = legacyFiles.some(file =>
    fs.existsSync(path.join(csrConfigDir, file))
);

// Check if unified state exists and has proper structure
let unifiedStateValid = false;
if (fs.existsSync(unifiedStateFile)) {
    try {
        const state = JSON.parse(fs.readFileSync(unifiedStateFile, 'utf8'));
        // Check for v5.0 structure
        unifiedStateValid = state.version === '5.0.0' &&
                           state.files &&
                           state.collections &&
                           state.metadata;
    } catch {
        unifiedStateValid = false;
    }
}

if (!needsMigration && unifiedStateValid) {
    console.log('✅ Already using Unified State Management v5.0');
    process.exit(0);
}

if (needsMigration) {
    console.log('📦 Legacy state files detected. Running automatic migration...');
    console.log('📋 Creating backup of existing state files...');

    try {
        // Check if Python is available
        const pythonCheck = spawnSync('python3', ['--version'], {
            stdio: 'ignore',
            shell: false
        });

        if (pythonCheck.error || pythonCheck.status !== 0) {
            console.log('⚠️  Python 3 not found. Migration will run when you first use the MCP server.');
            console.log('   To run migration manually: python3 scripts/migrate-to-unified-state.py');
            process.exit(0);
        }

        // Check if the migration script exists (npm global install location)
        const scriptLocations = [
            path.join(__dirname, 'migrate-to-unified-state.py'),
            path.join(homeDir, '.claude-self-reflect', 'scripts', 'migrate-to-unified-state.py'),
            path.join(process.cwd(), 'scripts', 'migrate-to-unified-state.py')
        ];

        let migrationScript = null;
        for (const location of scriptLocations) {
            if (fs.existsSync(location)) {
                migrationScript = location;
                break;
            }
        }

        if (!migrationScript) {
            console.log('⚠️  Migration script not found. It will run automatically when the MCP server starts.');
            process.exit(0);
        }

        // Run the migration safely using spawnSync to prevent shell injection
        console.log(`🚀 Running migration from: ${migrationScript}`);
        const result = spawnSync('python3', [migrationScript], {
            encoding: 'utf-8',
            stdio: 'pipe',
            shell: false // Explicitly disable shell to prevent injection
        });

        if (result.error) {
            throw result.error;
        }

        if (result.status !== 0) {
            // Categorize errors for better user guidance
            const stderr = result.stderr || '';
            const stdout = result.stdout || '';

            if (stderr.includes('ModuleNotFoundError')) {
                console.log('⚠️  Missing Python dependencies. The MCP server will install them on first run.');
                console.log('   To install manually: pip install -r requirements.txt');
            } else if (stderr.includes('PermissionError') || stderr.includes('Permission denied')) {
                console.log('⚠️  Permission issue accessing state files.');
                console.log('   Try running with appropriate permissions or check file ownership.');
            } else if (stderr.includes('FileNotFoundError')) {
                console.log('⚠️  State files not found at expected location.');
                console.log('   This is normal for fresh installations.');
            } else {
                console.log('⚠️  Migration encountered an issue:');
                console.log(stderr || stdout || `Exit code: ${result.status}`);
            }

            console.log('   Your existing state files are preserved.');
            console.log('   To run migration manually: python3 scripts/migrate-to-unified-state.py');
            console.log('   For help: https://github.com/ramakay/claude-self-reflect/issues');
            process.exit(0); // Exit gracefully, don't fail npm install
        }

        if (result.stdout) {
            console.log(result.stdout);
        }

        // Clean up legacy files after successful migration
        console.log('🧹 Cleaning up legacy state files...');
        let cleanedCount = 0;
        for (const file of legacyFiles) {
            const filePath = path.join(csrConfigDir, file);
            if (fs.existsSync(filePath)) {
                try {
                    // Move to archive instead of deleting (safer)
                    const archiveDir = path.join(csrConfigDir, 'archive');
                    if (!fs.existsSync(archiveDir)) {
                        fs.mkdirSync(archiveDir, { recursive: true });
                    }
                    const archivePath = path.join(archiveDir, `migrated-${file}`);
                    fs.renameSync(filePath, archivePath);
                    cleanedCount++;
                } catch (err) {
                    console.log(`   ⚠️ Could not archive ${file}: ${err.message}`);
                }
            }
        }

        if (cleanedCount > 0) {
            console.log(`   ✓ Archived ${cleanedCount} legacy files to config/archive/`);
        }

        console.log('✅ Migration completed successfully!');
        console.log('🎉 Now using Unified State Management v5.0 (20x faster!)');

    } catch (error) {
        // Handle unexpected errors
        console.log('⚠️  Migration encountered an unexpected issue:', error.message);
        console.log('   Your existing state files are preserved.');
        console.log('   To run migration manually: python3 scripts/migrate-to-unified-state.py');
        console.log('   For help: https://github.com/ramakay/claude-self-reflect/issues');
    }
} else {
    console.log('✅ Fresh installation - using Unified State Management v5.0');
}