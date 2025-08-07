#!/bin/bash
# Monitor memory usage during import cycles

echo "Starting memory monitoring for claude-reflection-watcher..."
echo "Time | Memory Usage | CPU % | Status"
echo "----------------------------------------"

while true; do
    # Get current timestamp
    timestamp=$(date "+%H:%M:%S")
    
    # Get stats for watcher container
    stats=$(docker stats claude-reflection-watcher --no-stream --format "{{.MemUsage}} | {{.CPUPerc}}" 2>/dev/null)
    
    if [ -z "$stats" ]; then
        echo "$timestamp | Container not running"
        break
    fi
    
    # Check if import is running by looking at logs
    recent_log=$(docker logs claude-reflection-watcher --tail 1 2>&1 | head -1)
    if [[ "$recent_log" == *"Running"* ]] || [[ "$recent_log" == *"Importing"* ]]; then
        status="IMPORTING"
    elif [[ "$recent_log" == *"Error"* ]] || [[ "$recent_log" == *"failed"* ]]; then
        status="ERROR"
    else
        status="IDLE"
    fi
    
    echo "$timestamp | $stats | $status"
    
    # Sleep for 1 second
    sleep 1
done