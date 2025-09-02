#!/bin/bash
# Watcher loop for Docker container
# Runs the streaming-watcher.py with HOT/WARM/COLD prioritization

# Don't use set -e in retry loops - it can cause premature exits

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
    python /app/scripts/streaming-watcher.py
    EXIT_CODE=$?
    
    if [ $EXIT_CODE -eq 0 ]; then
        echo "[$(date)] Watcher exited cleanly"
        RETRY_COUNT=0
        BACKOFF_SECONDS=5
    else
        echo "[$(date)] Watcher exited with code $EXIT_CODE"
        
        RETRY_COUNT=$((RETRY_COUNT + 1))
        if [ $RETRY_COUNT -ge $MAX_RETRIES ]; then
            echo "[$(date)] Maximum retries reached. Exiting."
            exit 1
        fi
        
        # Add jitter to prevent thundering herd (±20% of backoff)
        JITTER=$(( (RANDOM % (BACKOFF_SECONDS / 5 + 1)) - (BACKOFF_SECONDS / 10) ))
        SLEEP_TIME=$((BACKOFF_SECONDS + JITTER))
        [ $SLEEP_TIME -lt 1 ] && SLEEP_TIME=1
        
        echo "[$(date)] Restarting in $SLEEP_TIME seconds (base: $BACKOFF_SECONDS, jitter: $JITTER)..."
        sleep $SLEEP_TIME
        
        # Exponential backoff (max 300 seconds)
        BACKOFF_SECONDS=$((BACKOFF_SECONDS * 2))
        if [ $BACKOFF_SECONDS -gt 300 ]; then
            BACKOFF_SECONDS=300
        fi
    fi
done