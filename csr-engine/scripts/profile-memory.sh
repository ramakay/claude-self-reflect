#!/usr/bin/env bash
# Memory profiling harness for csr-engine (macOS only — relies on `/usr/bin/time -l`
# and `vmmap`, both BSD tools). See CLAUDE.md's "Context you must know" section for
# the baseline facts this script was built to reproduce.
#
# Usage: profile-memory.sh <binary> <db-path> [--label NAME]
#
# Prints ONE markdown table to stdout (peak RSS + wall time per scenario, plus a
# steady-state footprint row and a drift-reconciliation row). Diagnostic chatter
# goes to stderr so stdout stays a clean report — redirect stdout to capture it:
#   scripts/profile-memory.sh <binary> <db> --label baseline > report.md
set -euo pipefail

# ---------------------------------------------------------------------------
# Args
# ---------------------------------------------------------------------------
if [[ $# -lt 2 ]]; then
    echo "Usage: $0 <binary> <db-path> [--label NAME]" >&2
    exit 1
fi

BINARY="$1"
DB_PATH="$2"
shift 2
LABEL="profile"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --label)
            LABEL="$2"
            shift 2
            ;;
        *)
            echo "Unknown argument: $1" >&2
            exit 1
            ;;
    esac
done

if [[ ! -x "$BINARY" ]]; then
    echo "Binary not found or not executable: $BINARY" >&2
    exit 1
fi
if [[ ! -f "$DB_PATH" ]]; then
    echo "DB not found: $DB_PATH" >&2
    exit 1
fi
BINARY="$(cd "$(dirname "$BINARY")" && pwd)/$(basename "$BINARY")"
DB_PATH="$(cd "$(dirname "$DB_PATH")" && pwd)/$(basename "$DB_PATH")"

for tool in timeout vmmap /usr/bin/time sqlite3 python3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "Required tool not found on PATH: $tool" >&2
        exit 1
    fi
done

# Never touch the live data dir, no matter what caller passes.
LIVE_DATA_DIR="$HOME/.claude-self-reflect"
case "$DB_PATH" in
    "$LIVE_DATA_DIR"*)
        echo "Refusing to run against live data dir: $DB_PATH" >&2
        exit 1
        ;;
esac

# Prefer Homebrew's sqlite3 (has fts5) over the system one when available —
# see CLAUDE.md: "System sqlite3 (macOS): cannot load fts5".
SQLITE3=sqlite3
if [[ -x /opt/homebrew/opt/sqlite/bin/sqlite3 ]]; then
    SQLITE3=/opt/homebrew/opt/sqlite/bin/sqlite3
fi

TIMEOUT_SECS=90
SCRATCH_DIR="/tmp/csr-mem-profile"
mkdir -p "$SCRATCH_DIR"
WORK_DIR="$(mktemp -d "$SCRATCH_DIR/run.XXXXXX")"
cleanup() { rm -rf "$WORK_DIR"; }
trap cleanup EXIT

log() { echo "[profile-memory] $*" >&2; }

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Run one command under `/usr/bin/time -l`, capture peak RSS (bytes) + wall (s).
# Args: stdin_file (or /dev/null) -- command...
run_timed_once() {
    local stdin_file="$1"
    shift
    local timefile="$WORK_DIR/time.$$.$RANDOM.log"
    local outfile="$WORK_DIR/out.$$.$RANDOM.log"
    timeout "$TIMEOUT_SECS" /usr/bin/time -l "$@" <"$stdin_file" >"$outfile" 2>"$timefile" || true
    local rss wall
    rss=$(awk '/maximum resident set size/{print $1; exit}' "$timefile")
    wall=$(awk '/ real /{print $1; exit}' "$timefile")
    if [[ -z "$rss" || -z "$wall" ]]; then
        log "WARNING: could not parse time -l output for: $*"
        cat "$timefile" >&2
        rss=0
        wall=0
    fi
    rm -f "$timefile" "$outfile"
    echo "$rss $wall"
}

median_of() {
    # args: N numbers -> prints median
    local -a sorted
    mapfile -t sorted < <(printf '%s\n' "$@" | sort -n)
    local n=${#sorted[@]}
    local mid=$((n / 2))
    if ((n % 2 == 1)); then
        echo "${sorted[$mid]}"
    else
        awk -v a="${sorted[$((mid - 1))]}" -v b="${sorted[$mid]}" 'BEGIN{printf "%.4f", (a+b)/2}'
    fi
}

bytes_to_mb() {
    awk -v b="$1" 'BEGIN{printf "%.1f", b/1024/1024}'
}

# Run N (cheap) timed trials of a scenario, print "median_rss_mb median_wall_s".
median_trials() {
    local trials="$1"
    local stdin_file="$2"
    shift 2
    local -a rss_vals=()
    local -a wall_vals=()
    local i out r w
    for ((i = 1; i <= trials; i++)); do
        out=$(run_timed_once "$stdin_file" "$@")
        r=$(echo "$out" | awk '{print $1}')
        w=$(echo "$out" | awk '{print $2}')
        rss_vals+=("$r")
        wall_vals+=("$w")
        log "trial $i/$trials: rss=${r}B wall=${w}s -- $*"
    done
    local med_rss med_wall
    med_rss=$(median_of "${rss_vals[@]}")
    med_wall=$(median_of "${wall_vals[@]}")
    echo "$(bytes_to_mb "$med_rss") $med_wall"
}

# Parse a right-justified vmmap --summary REGION TYPE table column for a row
# whose name starts with `target`. vmmap right-justifies values so a wide
# value can overflow left past its header's "=" underline — we extract
# [prev_column_end+1, this_column_end] rather than [this_column_start, end]
# to capture that overflow correctly. col: 1=VIRTUAL 2=RESIDENT 3=DIRTY
# 4=SWAPPED 5=VOLATILE 6=NONVOL 7=EMPTY 8=COUNT.
parse_vmmap_col() {
    local vmmap_file="$1" target="$2" col="$3"
    awk -v target="$target" -v col="$col" '
        BEGIN { have_sep = 0; found = 0 }
        !have_sep && /^===========/ {
            n = 0; i = 1; len = length($0)
            while (i <= len) {
                c = substr($0, i, 1)
                if (c == "=") {
                    while (i <= len && substr($0, i, 1) == "=") i++
                    n++; ends[n] = i - 1
                } else { i++ }
            }
            have_sep = 1
            next
        }
        have_sep && !found && index($0, target) == 1 {
            lo = ends[col] + 1
            hi = ends[col + 1]
            val = substr($0, lo, hi - lo + 1)
            gsub(/^[ \t]+|[ \t]+$/, "", val)
            print val
            found = 1
        }
    ' "$vmmap_file"
}

vmmap_size_to_mb() {
    local v="$1"
    if [[ -z "$v" ]]; then
        echo "n/a"
        return
    fi
    local num="${v%[KMG]}"
    local suf="${v: -1}"
    case "$suf" in
        K) awk -v n="$num" 'BEGIN{printf "%.2f", n/1024}' ;;
        M) awk -v n="$num" 'BEGIN{printf "%.2f", n}' ;;
        G) awk -v n="$num" 'BEGIN{printf "%.2f", n*1024}' ;;
        *) echo "$v" ;;
    esac
}

# ---------------------------------------------------------------------------
# Binary identity
# ---------------------------------------------------------------------------
BIN_SIZE_BYTES=$(stat -f%z "$BINARY")
BIN_SIZE_MB=$(bytes_to_mb "$BIN_SIZE_BYTES")
BIN_SHA256=$(shasum -a 256 "$BINARY" | awk '{print $1}')

# ---------------------------------------------------------------------------
# Row 1: bare MCP initialize
# ---------------------------------------------------------------------------
log "=== Row 1: bare MCP initialize (median of 3) ==="
INIT_INPUT="$WORK_DIR/init_input.jsonl"
{
    printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"profile-harness","version":"0.1.0"}}}'
    printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
} >"$INIT_INPUT"
read -r ROW1_RSS_MB ROW1_WALL_S <<<"$(median_trials 3 "$INIT_INPUT" "$BINARY" --db-path "$DB_PATH")"

# ---------------------------------------------------------------------------
# Row 2: hook post-tool-use
# ---------------------------------------------------------------------------
log "=== Row 2: hook post-tool-use (median of 3) ==="
PTU_INPUT="$WORK_DIR/post_tool_use.json"
printf '%s' '{"session_id":"probe","tool_name":"Read","tool_input":{"file_path":"/tmp/x"},"cwd":"/tmp"}' >"$PTU_INPUT"
read -r ROW2_RSS_MB ROW2_WALL_S <<<"$(median_trials 3 "$PTU_INPUT" "$BINARY" --db-path "$DB_PATH" hook post-tool-use)"

# ---------------------------------------------------------------------------
# Row 3: hook prompt-submit
# ---------------------------------------------------------------------------
log "=== Row 3: hook prompt-submit (median of 3) ==="
PS_INPUT="$WORK_DIR/prompt_submit.json"
printf '%s' '{"session_id":"probe","prompt":"hello","cwd":"/tmp"}' >"$PS_INPUT"
read -r ROW3_RSS_MB ROW3_WALL_S <<<"$(median_trials 3 "$PS_INPUT" "$BINARY" --db-path "$DB_PATH" hook prompt-submit)"

# ---------------------------------------------------------------------------
# Row 4: steady-state private footprint (single run — server held open 60s)
# ---------------------------------------------------------------------------
log "=== Row 4: steady-state private footprint (vmmap @ t=6s) ==="
VMMAP_OUT="$WORK_DIR/vmmap.txt"
ROW4_STATUS="ok"
ROW4_FOOTPRINT="n/a"
ROW4_MALLOC_DIRTY="n/a"
ROW4_MAPPED_FILE="n/a"
(
    /bin/sleep 60 | "$BINARY" --db-path "$DB_PATH" >"$WORK_DIR/steady_stdout.log" 2>&1 &
    SERVER_PID=$!
    echo "$SERVER_PID" >"$WORK_DIR/steady.pid"
    sleep 6
    if kill -0 "$SERVER_PID" 2>/dev/null; then
        vmmap --summary "$SERVER_PID" >"$VMMAP_OUT" 2>&1 || true
    fi
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
) &
STEADY_WRAPPER_PID=$!
wait "$STEADY_WRAPPER_PID" || true

if [[ -s "$VMMAP_OUT" ]]; then
    RAW_FOOTPRINT=$(awk '/^Physical footprint:/{print $3; exit}' "$VMMAP_OUT")
    RAW_MALLOC_DIRTY=$(parse_vmmap_col "$VMMAP_OUT" "MALLOC_SMALL " 3)
    RAW_MAPPED_FILE=$(parse_vmmap_col "$VMMAP_OUT" "mapped file" 2)
    if [[ -n "$RAW_FOOTPRINT" ]]; then
        ROW4_FOOTPRINT="$(vmmap_size_to_mb "$RAW_FOOTPRINT") MB"
    else
        ROW4_STATUS="unmeasured: could not find 'Physical footprint:' in vmmap output"
    fi
    [[ -n "$RAW_MALLOC_DIRTY" ]] && ROW4_MALLOC_DIRTY="$(vmmap_size_to_mb "$RAW_MALLOC_DIRTY") MB"
    [[ -n "$RAW_MAPPED_FILE" ]] && ROW4_MAPPED_FILE="$(vmmap_size_to_mb "$RAW_MAPPED_FILE") MB"
else
    ROW4_STATUS="unmeasured: vmmap produced no output (process may have exited before t=6s)"
fi

# ---------------------------------------------------------------------------
# Row 5: drift +1 — one chunk_embeddings row the on-disk HNSW cache doesn't know about
# ---------------------------------------------------------------------------
log "=== Row 5: drift +1 (bare initialize against a db with one un-cached chunk) ==="
ROW5_STATUS="ok"
ROW5_RSS_MB="n/a"
ROW5_WALL_S="n/a"
ROW5_DELTA_MB="n/a"
DRIFT_DIR="$WORK_DIR/drift"
mkdir -p "$DRIFT_DIR"
DRIFT_DB="$DRIFT_DIR/csr-engine.db"
DRIFT_ID="csr-profile-drift-synthetic-$(date +%s)"
DB_DIR="$(dirname "$DB_PATH")"

if "$SQLITE3" "$DB_PATH" ".backup '$DRIFT_DB'" 2>"$WORK_DIR/drift_backup_err.log"; then
    if [[ -d "$DB_DIR/index" ]]; then
        cp -R "$DB_DIR/index" "$DRIFT_DIR/index"
    fi
    # Schema-inspect chunk_embeddings first (per task instructions) rather than
    # assuming shape: chunk_id TEXT PK REFERENCES chunks(id), embedding BLOB
    # (little-endian f32, one row per chunk — 384 dims == 1536 bytes, matching
    # EmbeddingEngine::dimension()).
    EMBED_SCHEMA=$("$SQLITE3" "$DRIFT_DB" ".schema chunk_embeddings" 2>/dev/null || true)
    log "chunk_embeddings schema: $EMBED_SCHEMA"
    BEFORE_COUNT=$("$SQLITE3" "$DRIFT_DB" "SELECT COUNT(*) FROM chunk_embeddings;" 2>/dev/null || echo "")

    if [[ -n "$EMBED_SCHEMA" && -n "$BEFORE_COUNT" ]]; then
        INSERT_ERR="$WORK_DIR/drift_insert_err.log"
        if python3 - "$DRIFT_DB" "$DRIFT_ID" >"$WORK_DIR/drift_insert_out.log" 2>"$INSERT_ERR" <<'PYEOF'
import sqlite3
import struct
import random
import sys

db_path, drift_id = sys.argv[1], sys.argv[2]
random.seed(42)
vec = [random.uniform(-0.1, 0.1) for _ in range(384)]
blob = struct.pack("<384f", *vec)

conn = sqlite3.connect(db_path)
conn.execute("PRAGMA foreign_keys=ON")
conn.execute(
    "INSERT INTO chunks (id, conversation_id, project_name, timestamp, content, "
    "message_count, source) VALUES (?, ?, ?, ?, ?, ?, ?)",
    (
        drift_id,
        "csr-profile-drift-conv",
        "csr-profile-drift-project",
        "2026-09-08T00:00:00Z",
        "synthetic drift probe row (profile-memory.sh)",
        1,
        "conversation",
    ),
)
conn.execute(
    "INSERT INTO chunk_embeddings (chunk_id, embedding) VALUES (?, ?)",
    (drift_id, blob),
)
conn.commit()
conn.close()
print(f"inserted {len(blob)}-byte embedding for {drift_id}")
PYEOF
        then
            AFTER_COUNT=$("$SQLITE3" "$DRIFT_DB" "SELECT COUNT(*) FROM chunk_embeddings;" 2>/dev/null || echo "")
            FK_VIOLATIONS=$("$SQLITE3" "$DRIFT_DB" "PRAGMA foreign_key_check;" 2>/dev/null || echo "")
            if [[ "$AFTER_COUNT" == "$((BEFORE_COUNT + 1))" && -z "$FK_VIOLATIONS" ]]; then
                log "drift row loaded cleanly: chunk_embeddings $BEFORE_COUNT -> $AFTER_COUNT"
                read -r ROW5_RSS_MB ROW5_WALL_S <<<"$(run_timed_once "$INIT_INPUT" "$BINARY" --db-path "$DRIFT_DB")"
                ROW5_RSS_MB=$(bytes_to_mb "$ROW5_RSS_MB")
                if [[ "$ROW1_RSS_MB" != "0.0" ]]; then
                    ROW5_DELTA_MB=$(awk -v a="$ROW5_RSS_MB" -v b="$ROW1_RSS_MB" 'BEGIN{printf "%+.1f", a-b}')
                fi
            else
                ROW5_STATUS="unmeasured: post-insert verification failed (count $BEFORE_COUNT -> $AFTER_COUNT, fk_violations='$FK_VIOLATIONS')"
            fi
        else
            ROW5_STATUS="unmeasured: chunk_embeddings insert failed: $(tr '\n' ' ' <"$INSERT_ERR")"
        fi
    else
        ROW5_STATUS="unmeasured: could not read chunk_embeddings schema/count from db copy"
    fi
else
    ROW5_STATUS="unmeasured: sqlite3 .backup failed: $(tr '\n' ' ' <"$WORK_DIR/drift_backup_err.log")"
fi

# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------
echo "# csr-engine memory profile — label: $LABEL"
echo
echo "- Binary: \`$BINARY\`"
echo "- Size: ${BIN_SIZE_MB} MB (${BIN_SIZE_BYTES} bytes)"
echo "- SHA256: \`$BIN_SHA256\`"
echo "- DB: \`$DB_PATH\`"
echo "- Date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo
echo "| # | Scenario | Peak RSS / Footprint (MB) | Wall (s) | Notes |"
echo "|---|----------|---------------------------:|---------:|-------|"
printf '| 1 | bare MCP initialize (median of 3) | %s | %s | - |\n' "$ROW1_RSS_MB" "$ROW1_WALL_S"
printf '| 2 | hook post-tool-use (median of 3) | %s | %s | - |\n' "$ROW2_RSS_MB" "$ROW2_WALL_S"
printf '| 3 | hook prompt-submit (median of 3) | %s | %s | - |\n' "$ROW3_RSS_MB" "$ROW3_WALL_S"
if [[ "$ROW4_STATUS" == "ok" ]]; then
    printf '| 4 | steady-state private footprint @ t=6s | %s | n/a | MALLOC_SMALL dirty=%s; mapped file resident=%s |\n' \
        "$ROW4_FOOTPRINT" "$ROW4_MALLOC_DIRTY" "$ROW4_MAPPED_FILE"
else
    printf '| 4 | steady-state private footprint @ t=6s | unmeasured | n/a | %s |\n' "$ROW4_STATUS"
fi
if [[ "$ROW5_STATUS" == "ok" ]]; then
    printf '| 5 | drift +1 bare initialize | %s | %s | delta vs row 1 = %s MB |\n' "$ROW5_RSS_MB" "$ROW5_WALL_S" "$ROW5_DELTA_MB"
else
    printf '| 5 | drift +1 bare initialize | unmeasured | n/a | %s |\n' "$ROW5_STATUS"
fi
