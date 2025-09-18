#!/usr/bin/env node

const { execSync } = require('child_process');
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

if (!needsMigration && fs.existsSync(unifiedStateFile)) {
    console.log('✅ Already using Unified State Management v5.0');
    process.exit(0);
}

if (needsMigration) {
    console.log('📦 Legacy state files detected. Running automatic migration...');
    console.log('📋 Creating backup of existing state files...');

    try {
        // Check if Python is available
        try {
            execSync('python3 --version', { stdio: 'ignore' });
        } catch {
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

        // Run the migration
        console.log(`🚀 Running migration from: ${migrationScript}`);
        const result = execSync(`python3 "${migrationScript}"`, {
            encoding: 'utf-8',
            stdio: 'pipe'
        });

        console.log(result);
        console.log('✅ Migration completed successfully!');
        console.log('🎉 Now using Unified State Management v5.0 (20x faster!)');

    } catch (error) {
        console.log('⚠️  Migration encountered an issue:', error.message);
        console.log('   Your existing state files are preserved.');
        console.log('   To run migration manually: python3 scripts/migrate-to-unified-state.py');
        console.log('   For help: https://github.com/ramakay/claude-self-reflect/issues');
    }
} else {
    console.log('✅ Fresh installation - using Unified State Management v5.0');
}