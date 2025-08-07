#!/bin/bash
# Monitor streaming importer memory usage with operational memory calculation

echo "Starting memory monitoring for streaming-importer..."
echo "Note: Operational memory = Total memory - 237MB (FastEmbed model)"
echo "============================================================"

# Create CSV log file
LOG_FILE="memory-log-$(date +%Y%m%d-%H%M%S).csv"
echo "Timestamp,Container,TotalMemory(MB),OperationalMemory(MB),CPU(%),Status" > "$LOG_FILE"

# Color codes for output
RED='\033[0;31m'
YELLOW='\033[1;33m'
GREEN='\033[0;32m'
NC='\033[0m' # No Color

# Monitor for 2 minutes with 5-second intervals (24 samples)
DURATION=120
INTERVAL=5
SAMPLES=$((DURATION / INTERVAL))

echo "Monitoring for $DURATION seconds ($SAMPLES samples at ${INTERVAL}s intervals)"
echo "Log file: $LOG_FILE"
echo ""

for i in $(seq 1 $SAMPLES); do
    # Get current stats
    STATS=$(docker stats --no-stream --format "{{.Container}},{{.MemUsage}},{{.CPUPerc}}" claude-reflection-streaming 2>/dev/null)
    
    if [ -z "$STATS" ]; then
        echo "$(date +%H:%M:%S),streaming-importer,0,0,0,NOT_RUNNING" >> "$LOG_FILE"
        echo -e "${RED}[$(date +%H:%M:%S)] Container not running${NC}"
    else
        # Parse memory usage
        MEMORY_RAW=$(echo $STATS | cut -d',' -f2 | cut -d'/' -f1)
        MEMORY=$(echo $MEMORY_RAW | sed 's/MiB//' | sed 's/GiB/*1024/' | bc 2>/dev/null || echo "0")
        CPU=$(echo $STATS | cut -d',' -f3)
        
        # Calculate operational memory (total - FastEmbed model)
        OPERATIONAL=$(echo "$MEMORY - 237" | bc 2>/dev/null || echo "0")
        
        # Determine status
        STATUS="OK"
        COLOR=$GREEN
        if (( $(echo "$OPERATIONAL > 50" | bc -l 2>/dev/null || echo 0) )); then
            STATUS="EXCEEDED"
            COLOR=$RED
        elif (( $(echo "$OPERATIONAL > 40" | bc -l 2>/dev/null || echo 0) )); then
            STATUS="WARNING"
            COLOR=$YELLOW
        fi
        
        # Log to CSV
        echo "$(date +%H:%M:%S),streaming-importer,$MEMORY,$OPERATIONAL,$CPU,$STATUS" >> "$LOG_FILE"
        
        # Display to console
        echo -e "${COLOR}[$(date +%H:%M:%S)] Total: ${MEMORY}MB | Operational: ${OPERATIONAL}MB | CPU: $CPU | Status: $STATUS${NC}"
        
        # Alert if operational memory exceeds limits
        if [ "$STATUS" = "EXCEEDED" ]; then
            echo -e "${RED}⚠️  ALERT: Operational memory ${OPERATIONAL}MB exceeds 50MB limit!${NC}"
        fi
    fi
    
    # Wait for next sample (except on last iteration)
    if [ $i -lt $SAMPLES ]; then
        sleep $INTERVAL
    fi
done

echo ""
echo "============================================================"
echo "Memory monitoring complete. Results saved to: $LOG_FILE"

# Generate summary statistics
echo ""
echo "Summary Statistics:"
echo "-------------------"

# Calculate averages using awk
awk -F',' '
    NR>1 && $3>0 {
        total+=$3; 
        operational+=$4; 
        count++; 
        if ($4 > max_op) max_op=$4;
        if ($4 < min_op || min_op=="") min_op=$4;
        if ($6 == "EXCEEDED") exceeded++;
        if ($6 == "WARNING") warning++;
    }
    END {
        if (count > 0) {
            print "Average Total Memory: " total/count " MB";
            print "Average Operational Memory: " operational/count " MB";
            print "Peak Operational Memory: " max_op " MB";
            print "Min Operational Memory: " min_op " MB";
            print "Samples Exceeded (>50MB): " exceeded+0;
            print "Samples Warning (>40MB): " warning+0;
            print "Total Samples: " count;
        } else {
            print "No valid samples collected";
        }
    }
' "$LOG_FILE"

echo ""
echo "Test Evidence:"
echo "- Log file: $LOG_FILE"
echo "- Use 'cat $LOG_FILE' to view raw data"
echo "- Import this CSV into spreadsheet for graphs"