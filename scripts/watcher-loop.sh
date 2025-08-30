#!/bin/bash
# Watcher loop for Docker container
# Runs the streaming-watcher.py with HOT/WARM/COLD prioritization

set -e

echo "Starting Claude Self-Reflect Streaming Watcher v3.0.0"
echo "HOT/WARM/COLD prioritization enabled"
echo "=========================================="

# Ensure config directory exists
mkdir -p /config

# Set Python path to include scripts directory
export PYTHONPATH=/app/scripts:$PYTHONPATH

# Main loop - restart on failure with backoff
RETRY_COUNT=0
MAX_RETRIES=10
BACKOFF_SECONDS=5

while true; do
    echo "[$(date)] Starting watcher (attempt $((RETRY_COUNT + 1))/$MAX_RETRIES)..."
    
    # Run the streaming watcher
    if python /app/scripts/streaming-watcher.py; then
        echo "[$(date)] Watcher exited cleanly"
        RETRY_COUNT=0
        BACKOFF_SECONDS=5
    else
        EXIT_CODE=$?
        echo "[$(date)] Watcher exited with code $EXIT_CODE"
        
        RETRY_COUNT=$((RETRY_COUNT + 1))
        if [ $RETRY_COUNT -ge $MAX_RETRIES ]; then
            echo "[$(date)] Maximum retries reached. Exiting."
            exit 1
        fi
        
        echo "[$(date)] Restarting in $BACKOFF_SECONDS seconds..."
        sleep $BACKOFF_SECONDS
        
        # Exponential backoff (max 300 seconds)
        BACKOFF_SECONDS=$((BACKOFF_SECONDS * 2))
        if [ $BACKOFF_SECONDS -gt 300 ]; then
            BACKOFF_SECONDS=300
        fi
    fi
done