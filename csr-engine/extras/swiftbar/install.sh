#!/bin/bash
# Install CSR SwiftBar plugin
#
# Prerequisites:
#   brew install swiftbar jq
#   csr-engine in PATH

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PLUGIN="csr-status.30s.sh"

# Detect SwiftBar plugin directory
SWIFTBAR_DIR="${HOME}/Library/Application Support/SwiftBar"
if [ ! -d "$SWIFTBAR_DIR" ]; then
    # Try default plugin folder location
    SWIFTBAR_DIR="${HOME}/.config/SwiftBar"
fi

# Check dependencies
if ! command -v jq &>/dev/null; then
    echo "Error: jq is required. Install with: brew install jq"
    exit 1
fi

if ! command -v csr-engine &>/dev/null; then
    echo "Warning: csr-engine not in PATH. Set CSR_ENGINE_PATH in the plugin."
fi

# Check if SwiftBar is installed
if ! [ -d "/Applications/SwiftBar.app" ] && ! command -v swiftbar &>/dev/null; then
    echo "SwiftBar not found. Install it:"
    echo "  brew install --cask swiftbar"
    echo ""
    echo "Then re-run this script."
    exit 1
fi

# Create plugin directory if needed
if [ ! -d "$SWIFTBAR_DIR" ]; then
    echo "SwiftBar plugin directory not found at: $SWIFTBAR_DIR"
    echo "Launch SwiftBar first and set a plugin folder, then re-run."
    exit 1
fi

# Symlink the plugin
TARGET="${SWIFTBAR_DIR}/${PLUGIN}"
if [ -L "$TARGET" ] || [ -f "$TARGET" ]; then
    echo "Updating existing plugin..."
    rm "$TARGET"
fi

ln -s "${SCRIPT_DIR}/${PLUGIN}" "$TARGET"
echo "Installed: ${TARGET} -> ${SCRIPT_DIR}/${PLUGIN}"

# Create focus/summary files if they don't exist
touch "${HOME}/.claude-self-reflect/current-focus.txt"
touch "${HOME}/.claude-self-reflect/last-session-summary.txt"

echo ""
echo "Done! The CSR status icon should appear in your menu bar."
echo "If SwiftBar is running, it will pick up the plugin automatically."
echo ""
echo "To test manually:"
echo "  ${SCRIPT_DIR}/${PLUGIN}"
