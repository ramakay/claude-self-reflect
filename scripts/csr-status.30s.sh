#!/bin/bash
# <bitbar.title>CSR Status</bitbar.title>
# <bitbar.version>v1.0</bitbar.version>
# <bitbar.author>Claude Self-Reflect</bitbar.author>
# <bitbar.author.github>ramakay</bitbar.author.github>
# <bitbar.desc>Live stats for Claude Self-Reflect conversation memory</bitbar.desc>
# <bitbar.dependencies>csr-engine</bitbar.dependencies>
# <swiftbar.hideAbout>true</swiftbar.hideAbout>
# <swiftbar.hideRunInTerminal>true</swiftbar.hideRunInTerminal>
# <swiftbar.hideDisablePlugin>true</swiftbar.hideDisablePlugin>
#
# Refreshes every 30 seconds (per filename convention: .30s.)
# Install: csr-engine hook install --apply (auto-copies to SwiftBar plugins dir)
# Manual:  cp scripts/csr-status.30s.sh ~/Library/Application\ Support/SwiftBar/Plugins/

if command -v csr-engine &>/dev/null; then
    csr-engine status --swiftbar 2>/dev/null
else
    echo "🧠 CSR offline"
    echo "---"
    echo "csr-engine not found | color=red"
    echo "Install: npx claude-self-reflect | font=Menlo"
fi
