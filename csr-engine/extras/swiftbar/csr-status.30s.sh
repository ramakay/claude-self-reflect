#!/bin/bash
# <swiftbar.hideAbout>true</swiftbar.hideAbout>
# <swiftbar.hideRunInTerminal>true</swiftbar.hideRunInTerminal>
# <swiftbar.hideLastUpdated>false</swiftbar.hideLastUpdated>
# <swiftbar.hideDisablePlugin>true</swiftbar.hideDisablePlugin>
#
# CSR Engine — SwiftBar Status Plugin
# Refresh: every 30 seconds (from filename)

# --- Config ---
CSR_BIN="${CSR_ENGINE_PATH:-csr-engine}"
CSR_DIR="${HOME}/.claude-self-reflect"
HOOK_LOG="${CSR_DIR}/hook-timing.log"
FOCUS_FILE="${CSR_DIR}/current-focus.txt"
SUMMARY_FILE="${CSR_DIR}/last-session-summary.txt"

# --- Gather data ---
STATUS_JSON=$("$CSR_BIN" status 2>/dev/null)
if [ $? -ne 0 ] || [ -z "$STATUS_JSON" ]; then
    echo "🧠 ✗ | color=#ff6b6b"
    echo "---"
    echo "CSR Engine not responding | color=#ff6b6b"
    echo "Binary: ${CSR_BIN} | size=11 color=#888888"
    echo "---"
    echo "Retry | refresh=true"
    exit 0
fi

# Parse JSON with jq (required dependency)
conversations=$(echo "$STATUS_JSON" | jq -r '.conversations // 0')
chunks=$(echo "$STATUS_JSON" | jq -r '.chunks // 0')
reflections=$(echo "$STATUS_JSON" | jq -r '.reflections // 0')
projects=$(echo "$STATUS_JSON" | jq -r '.projects // 0')
import_pct=$(echo "$STATUS_JSON" | jq -r '.import_percent // 0')
healthy=$(echo "$STATUS_JSON" | jq -r '.healthy // false')
db_bytes=$(echo "$STATUS_JSON" | jq -r '.db_size_bytes // 0')
db_path=$(echo "$STATUS_JSON" | jq -r '.db_path // "unknown"')

# Enrichment stats
heuristic=$(echo "$STATUS_JSON" | jq -r '.enrichment.heuristic_completed // 0')
v3_done=$(echo "$STATUS_JSON" | jq -r '.enrichment.extracted_v3_completed // 0')
ai_done=$(echo "$STATUS_JSON" | jq -r '.enrichment.ai_narrative_completed // 0')

# Format numbers
db_mb=$(( db_bytes / 1048576 ))

# Health indicator
if [ "$healthy" = "true" ]; then
    health_icon="✓"
    health_color="#4ade80"
    health_text="Healthy"
else
    health_icon="✗"
    health_color="#ff6b6b"
    health_text="Unhealthy"
fi

# Last hook timing from log
last_hook_line=""
last_hook_ms=""
if [ -f "$HOOK_LOG" ]; then
    last_hook_line=$(grep "CSR hook" "$HOOK_LOG" | tail -1)
    last_hook_ms=$(echo "$last_hook_line" | grep -oE 'total=[0-9]+ms' | head -1 | grep -oE '[0-9]+')
fi

# --- Menu Bar Title (alternating lines) ---
echo "🧠 ${chunks}c ${reflections}r ${health_icon} | size=12"
if [ -n "$last_hook_ms" ]; then
    echo "🧠 ${last_hook_ms}ms ${health_icon} | size=12"
fi
echo "---"

# --- Today's Focus ---
if [ -f "$FOCUS_FILE" ] && [ -s "$FOCUS_FILE" ]; then
    focus=$(head -1 "$FOCUS_FILE" | cut -c1-80)
    echo "📌 ${focus} | size=13 color=#a78bfa"
    echo "---"
fi

# --- Last Session Summary ---
if [ -f "$SUMMARY_FILE" ] && [ -s "$SUMMARY_FILE" ]; then
    summary=$(head -3 "$SUMMARY_FILE" | tr '\n' ' ' | cut -c1-120)
    echo "💬 ${summary} | size=11 color=#94a3b8 length=60"
    echo "---"
fi

# --- Engine Status ---
echo "Engine Status | size=11 color=#888888"
echo "  Conversations   ${conversations} | size=12 font=Menlo"
echo "  Chunks          ${chunks} | size=12 font=Menlo"
echo "  Reflections     ${reflections} | size=12 font=Menlo"
echo "  Projects        ${projects} | size=12 font=Menlo"
echo "  DB Size         ${db_mb} MB | size=12 font=Menlo"
echo "  Import          ${import_pct}% | size=12 font=Menlo"
echo "  Health          ${health_text} | size=12 font=Menlo color=${health_color}"
echo "---"

# --- Enrichment Pipeline ---
echo "Enrichment | size=11 color=#888888"
echo "  Heuristic (L1)   ${heuristic} done | size=12 font=Menlo"
echo "  V3 Extract (L2)  ${v3_done} done | size=12 font=Menlo"
echo "  AI Narrative (L3) ${ai_done} done | size=12 font=Menlo"
echo "---"

# --- Recent Hook Activity ---
echo "Recent Hooks | size=11 color=#888888"
if [ -f "$HOOK_LOG" ]; then
    # Parse last 8 hook entries
    grep "CSR hook" "$HOOK_LOG" | tail -8 | while IFS= read -r line; do
        # Extract: timestamp, hook name, project, total time
        ts=$(echo "$line" | grep -oE '[0-9]{2}:[0-9]{2}:[0-9]{2}' | head -1)
        hook=$(echo "$line" | grep -oE 'hook [a-z-]+' | head -1 | sed 's/hook //')
        proj=$(echo "$line" | grep -oE '\[[a-zA-Z0-9._-]+\]' | head -1 | tr -d '[]')
        total=$(echo "$line" | grep -oE 'total=[0-9]+ms' | head -1 | sed 's/total=//')

        # Color by speed
        ms_val=$(echo "$total" | grep -oE '[0-9]+')
        if [ -n "$ms_val" ] && [ "$ms_val" -lt 20 ]; then
            clr="#4ade80"
        elif [ -n "$ms_val" ] && [ "$ms_val" -lt 100 ]; then
            clr="#fbbf24"
        else
            clr="#ff6b6b"
        fi

        printf "  %s  %-16s %-20s %s\n" "$ts" "$hook" "[$proj]" "$total" | sed "s/$/ | size=11 font=Menlo color=${clr}/"
    done

    # Last injection stats
    last_inject=$(grep "inject:" "$HOOK_LOG" | tail -1)
    if [ -n "$last_inject" ]; then
        items=$(echo "$last_inject" | grep -oE 'items=[0-9]+' | head -1 | sed 's/items=//')
        top_score=$(echo "$last_inject" | grep -oE 'top=\[[^]]*\]' | head -1)
        if [ -n "$items" ]; then
            echo "---"
            echo "Last Injection | size=11 color=#888888"
            echo "  Items: ${items}  ${top_score} | size=11 font=Menlo color=#94a3b8"
        fi
    fi
else
    echo "  No hook log found | size=11 color=#666666"
fi
echo "---"

# --- Quick Actions ---
echo "Quick Actions | size=11 color=#888888"
echo "  ▶ Run Import | bash=${CSR_BIN} param0=--import terminal=true refresh=true"
echo "  ▶ Run Eval (quick) | bash=${CSR_BIN} param0=eval terminal=true"
echo "  ▶ Run Eval (full) | bash=${CSR_BIN} param0=eval param1=--full terminal=true"
echo "  ▶ Backfill Stories | bash=${CSR_BIN} param0=backfill-stories terminal=true refresh=true"
echo "  ▶ System Status (JSON) | bash=${CSR_BIN} param0=status terminal=true"
echo "---"
echo "  Open Hook Log | bash=/usr/bin/open param0=-a param1=Console param2=${HOOK_LOG} terminal=false"
echo "  Open DB Directory | bash=/usr/bin/open param0=${CSR_DIR} terminal=false"
echo "  Reveal Binary | bash=/usr/bin/open param0=-R param1=$(which ${CSR_BIN} 2>/dev/null || echo ${CSR_BIN}) terminal=false"
echo "---"

# --- About ---
version=$("$CSR_BIN" --version 2>/dev/null || echo "unknown")
branch=$(cd /Users/ramakrishnanannaswamy/projects/claude-self-reflect && git branch --show-current 2>/dev/null || echo "unknown")
echo "CSR Engine ${version} | size=10 color=#666666"
echo "Branch: ${branch} | size=10 color=#666666"
echo "Refresh | refresh=true sfimage=arrow.clockwise"
