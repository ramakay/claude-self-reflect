#!/bin/bash

# Start Claude Code Router
# This script starts Claude Code with the router configured for Cerebras Qwen Coder

echo "🚀 Starting Claude Code Router with Cerebras Qwen Coder..."
echo ""
echo "Configuration:"
echo "  - Model: Qwen 3 Coder 480B (via Cerebras on OpenRouter)"
echo "  - Context: 128k tokens (paid tier)"
echo "  - Temperature: 0.7 (optimized for code generation)"
echo "  - Top-p: 0.8 (optimized for code generation)"
echo ""
echo "Starting router..."
echo ""

# Start the router
ccr code

# If you want to use the UI mode instead, uncomment the line below:
# ccr ui