#!/usr/bin/env bash
# Memory profiling harness for csr-engine (macOS only — relies on `/usr/bin/time -l`
# and `vmmap`, both BSD tools; runs on the stock /bin/bash 3.2). Reproduces the
# per-process startup cost of the engine (HNSW cache load + model) across the MCP
# and hook entry points.
#
# Usage: profile-memory.sh <binary> <db-path> [--label NAME]
#
# Prints ONE markdown table to stdout (peak RSS + wall time per scenario, plus a
# steady-state footprint row and a drift-reconciliation row). Diagnostic chatter
# goes to stderr so stdout stays a clean report — redirect stdout to capture it:
#   scripts/profile-memory.sh <binary> <db> --label baseline > report.md
#
# Isolation: every child process runs with HOME pointed at a scratch directory
# (so hook-timing.log, mcp-binary.txt and the probe caches land there, never in
# the real ~/.claude-self-reflect), with an explicit --projects-dir inside that
# scratch HOME, and with the fastembed model cache COPIED in from the real
# ~/Library/Caches so no scenario ever downloads. Hook rows run against a fresh
# SQLite backup of <db-path> (hooks may write); rows that only read use it in
# place. The real live data dir is refused as <db-path> even through a symlink,
# and its hook-timing.log / mcp-binary.txt mtimes are checked before and after.
#
# Every trial must exit 0 AND leave the marker its scenario is expected to
# produce (the MCP initialize response, the hook's own timing line); a row with
# any failed trial is reported as unmeasured with the reason, never as a number.
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

for tool in timeout vmmap /usr/bin/time sqlite3 python3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "Required tool not found on PATH: $tool" >&2
        exit 1
    fi
done

# Fully resolve both paths (symlinks included) before any guard looks at them.
BINARY="$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$BINARY")"
DB_PATH="$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$DB_PATH")"

# Never touch the live data dir, no matter what caller passes.
LIVE_DATA_DIR="$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$HOME/.claude-self-reflect")"
case "$DB_PATH" in
    "$LIVE_DATA_DIR"/*)
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
# Fully resolved (macOS /tmp is a symlink to /private/tmp): the hooks compare
# their canonicalized cwd against $HOME, so the scratch HOME must be canonical.
WORK_DIR="$(python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$(mktemp -d "$SCRATCH_DIR/run.XXXXXX")")"
cleanup() { rm -rf "$WORK_DIR"; }
trap cleanup EXIT

log() { echo "[profile-memory] $*" >&2; }

# ---------------------------------------------------------------------------
# Isolated HOME for every child process
# ---------------------------------------------------------------------------
FAKE_HOME="$WORK_DIR/home"
PROJECTS_DIR="$FAKE_HOME/.claude/projects"
HOOK_CWD="$FAKE_HOME/project"
mkdir -p "$FAKE_HOME/.claude-self-reflect" "$PROJECTS_DIR" "$HOOK_CWD" "$FAKE_HOME/Library/Caches/csr-engine"

REAL_MODEL_CACHE="$HOME/Library/Caches/csr-engine/fastembed"
if [[ ! -d "$REAL_MODEL_CACHE" ]]; then
    echo "fastembed model cache not found at $REAL_MODEL_CACHE — run \`csr-engine setup\` once so no scenario has to download" >&2
    exit 1
fi
cp -R "$REAL_MODEL_CACHE" "$FAKE_HOME/Library/Caches/csr-engine/fastembed"
# Probe caches (intent/reaction exemplar embeddings): copying them keeps
# prompt-submit at its steady-state cost instead of a first-run re-embed.
for probe in intent_probes.json reaction_probes.json; do
    if [[ -f "$LIVE_DATA_DIR/$probe" ]]; then
        cp "$LIVE_DATA_DIR/$probe" "$FAKE_HOME/.claude-self-reflect/$probe"
    fi
done
# A real file for the Edit hook to track — the installed PostToolUse matcher
# (Edit|Write|MultiEdit|NotebookEdit) is what fires this hook in production.
printf 'fn probe() -> u32 {\n    42\n}\n' >"$HOOK_CWD/probe.rs"

live_mtime() {
    local f="$LIVE_DATA_DIR/$1"
    if [[ -e "$f" ]]; then stat -f %m "$f"; else echo "absent"; fi
}
LIVE_TIMING_MTIME_BEFORE="$(live_mtime hook-timing.log)"
LIVE_STAMP_MTIME_BEFORE="$(live_mtime mcp-binary.txt)"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Run one scenario under `/usr/bin/time -l` with the isolated HOME.
# Args: stdin_file expect_regex db_path [engine args...]
# Prints "rss_bytes wall_s footprint_bytes ok" on success, "0 0 0 fail" on any
# failure (non-zero exit, timeout, missing marker, unparsable timing), with
# diagnostics on stderr. RSS counts file-backed pages too; the footprint is the
# private dirty peak, which is what memory pressure actually sees.
# The child's stderr is copied to $LAST_STDERR for the caller to inspect
# (a fixed file, because callers invoke this inside command substitution).
LAST_STDERR="$WORK_DIR/last_stderr.log"
run_timed_once() {
    local stdin_file="$1" expect="$2" db="$3"
    shift 3
    local timefile="$WORK_DIR/time.$$.$RANDOM.log"
    local outfile="$WORK_DIR/out.$$.$RANDOM.log"
    local status=0
    (
        export HOME="$FAKE_HOME"
        unset CSR_DISABLE_RECURSIVE_HOOKS
        timeout "$TIMEOUT_SECS" /usr/bin/time -l "$BINARY" --db-path "$db" --projects-dir "$PROJECTS_DIR" "$@" \
            <"$stdin_file" >"$outfile" 2>"$timefile"
    ) || status=$?
    cp "$timefile" "$LAST_STDERR"
    local rss wall fp
    rss=$(awk '/maximum resident set size/{print $1; exit}' "$timefile")
    wall=$(awk '/ real /{print $1; exit}' "$timefile")
    fp=$(awk '/peak memory footprint/{print $1; exit}' "$timefile")
    local reason=""
    if [[ $status -ne 0 ]]; then
        reason="exit status $status"
    elif [[ -z "$rss" || -z "$wall" || -z "$fp" ]]; then
        reason="could not parse /usr/bin/time -l output"
    elif [[ -n "$expect" ]] && ! grep -Eq -- "$expect" "$outfile" "$timefile"; then
        reason="expected marker /$expect/ not found in stdout/stderr"
    fi
    if [[ -n "$reason" ]]; then
        log "FAILED trial ($reason): $BINARY --db-path $db $*"
        grep -v 'maximum resident\|page reclaims\|page faults\|swaps\|block \|messages \|signals \|context switches\|instructions retired\|cycles elapsed\|peak memory\|^ *[0-9.]* real ' "$timefile" | tail -15 >&2 || true
        echo "0 0 0 fail"
        return
    fi
    rm -f "$outfile"
    echo "$rss $wall $fp ok"
}

median_of() {
    # args: N numbers -> prints median (bash 3.2 compatible: no mapfile)
    local -a sorted
    sorted=($(printf '%s\n' "$@" | sort -n))
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

# Run N timed trials of a scenario. Prints "median_rss_mb median_wall_s
# median_footprint_mb" when every trial passed, or "fail" if any trial failed.
median_trials() {
    local trials="$1" stdin_file="$2" expect="$3" db="$4"
    shift 4
    local -a rss_vals=()
    local -a wall_vals=()
    local -a fp_vals=()
    local i out r w f s
    for ((i = 1; i <= trials; i++)); do
        out=$(run_timed_once "$stdin_file" "$expect" "$db" "$@")
        r=$(echo "$out" | awk '{print $1}')
        w=$(echo "$out" | awk '{print $2}')
        f=$(echo "$out" | awk '{print $3}')
        s=$(echo "$out" | awk '{print $4}')
        if [[ "$s" != "ok" ]]; then
            echo "fail"
            return
        fi
        rss_vals+=("$r")
        wall_vals+=("$w")
        fp_vals+=("$f")
        log "trial $i/$trials: rss=${r}B footprint=${f}B wall=${w}s -- $BINARY --db-path $db $*"
    done
    local med_rss med_wall med_fp
    med_rss=$(median_of "${rss_vals[@]}")
    med_wall=$(median_of "${wall_vals[@]}")
    med_fp=$(median_of "${fp_vals[@]}")
    echo "$(bytes_to_mb "$med_rss") $med_wall $(bytes_to_mb "$med_fp")"
}

# Fresh SQLite backup of DB_PATH plus a copy of its index dir, into $1.
# Prints the new db path, or nothing (with diagnostics) on failure.
snapshot_db() {
    local dir="$1"
    mkdir -p "$dir"
    local db="$dir/csr-engine.db"
    if ! "$SQLITE3" "$DB_PATH" ".backup '$db'" 2>"$dir/backup_err.log"; then
        log "sqlite3 .backup failed: $(tr '\n' ' ' <"$dir/backup_err.log")"
        return
    fi
    if [[ -d "$(dirname "$DB_PATH")/index" ]]; then
        cp -R "$(dirname "$DB_PATH")/index" "$dir/index"
    fi
    echo "$db"
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
# Row 1: bare MCP initialize (read-only: runs against <db-path> in place)
# ---------------------------------------------------------------------------
log "=== Row 1: bare MCP initialize (median of 3) ==="
INIT_INPUT="$WORK_DIR/init_input.jsonl"
{
    printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"profile-harness","version":"0.1.0"}}}'
    printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
} >"$INIT_INPUT"
INIT_EXPECT='"serverInfo"'
ROW1_RESULT="$(median_trials 3 "$INIT_INPUT" "$INIT_EXPECT" "$DB_PATH")"
ROW1_RSS_MB="unmeasured"; ROW1_WALL_S="n/a"; ROW1_FP_MB="n/a"
if [[ "$ROW1_RESULT" != "fail" ]]; then
    read -r ROW1_RSS_MB ROW1_WALL_S ROW1_FP_MB <<<"$ROW1_RESULT"
fi

# ---------------------------------------------------------------------------
# Rows 2-3: hooks, against a fresh snapshot (hooks may write to the DB)
# ---------------------------------------------------------------------------
HOOK_DB="$(snapshot_db "$WORK_DIR/hooks")"
ROW2_RSS_MB="unmeasured"; ROW2_WALL_S="n/a"; ROW2_FP_MB="n/a"; ROW2_NOTE="-"
ROW3_RSS_MB="unmeasured"; ROW3_WALL_S="n/a"; ROW3_FP_MB="n/a"; ROW3_NOTE="-"
if [[ -z "$HOOK_DB" ]]; then
    ROW2_NOTE="snapshot of the db failed (see stderr)"
    ROW3_NOTE="$ROW2_NOTE"
else
    # Row 2: post-tool-use for an Edit — the installed matcher's case. cwd sits
    # inside the isolated HOME, which is what the hook's cwd guard requires.
    log "=== Row 2: hook post-tool-use Edit, installed matcher (median of 3) ==="
    PTU_INPUT="$WORK_DIR/post_tool_use.json"
    printf '{"session_id":"probe","tool_name":"Edit","tool_input":{"file_path":"%s"},"cwd":"%s"}' \
        "$HOOK_CWD/probe.rs" "$HOOK_CWD" >"$PTU_INPUT"
    ROW2_RESULT="$(median_trials 3 "$PTU_INPUT" 'CSR hook post-tool-use' "$HOOK_DB" hook post-tool-use)"
    if [[ "$ROW2_RESULT" != "fail" ]]; then
        read -r ROW2_RSS_MB ROW2_WALL_S ROW2_FP_MB <<<"$ROW2_RESULT"
    else
        ROW2_NOTE="a trial failed (see stderr)"
    fi

    # Row 3: prompt-submit with a real, search-worthy prompt so the hook runs
    # its full path (intent classification + injection search), not the
    # short-prompt early return.
    log "=== Row 3: hook prompt-submit (median of 3) ==="
    PS_INPUT="$WORK_DIR/prompt_submit.json"
    printf '{"session_id":"probe","prompt":"why does the engine load the whole HNSW index at startup and how do I reduce its memory","cwd":"%s"}' \
        "$HOOK_CWD" >"$PS_INPUT"
    ROW3_RESULT="$(median_trials 3 "$PS_INPUT" 'CSR hook prompt-submit' "$HOOK_DB" hook prompt-submit)"
    if [[ "$ROW3_RESULT" != "fail" ]]; then
        read -r ROW3_RSS_MB ROW3_WALL_S ROW3_FP_MB <<<"$ROW3_RESULT"
    else
        ROW3_NOTE="a trial failed (see stderr)"
    fi
fi

# ---------------------------------------------------------------------------
# Row 4: steady-state private footprint after the MCP handshake (vmmap @ t=6s)
# ---------------------------------------------------------------------------
log "=== Row 4: steady-state private footprint (vmmap @ t=6s after initialize) ==="
VMMAP_OUT="$WORK_DIR/vmmap.txt"
ROW4_STATUS="ok"
ROW4_FOOTPRINT="n/a"
ROW4_MALLOC_DIRTY="n/a"
ROW4_MAPPED_FILE="n/a"
(
    export HOME="$FAKE_HOME"
    unset CSR_DISABLE_RECURSIVE_HOOKS
    { cat "$INIT_INPUT"; /bin/sleep 60; } | "$BINARY" --db-path "$DB_PATH" --projects-dir "$PROJECTS_DIR" \
        >"$WORK_DIR/steady_stdout.log" 2>"$WORK_DIR/steady_stderr.log" &
    SERVER_PID=$!
    sleep 6
    if kill -0 "$SERVER_PID" 2>/dev/null; then
        vmmap --summary "$SERVER_PID" >"$VMMAP_OUT" 2>&1 || true
    fi
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
) &
STEADY_WRAPPER_PID=$!
wait "$STEADY_WRAPPER_PID" || true

if ! grep -q '"serverInfo"' "$WORK_DIR/steady_stdout.log" 2>/dev/null; then
    ROW4_STATUS="unmeasured: server never answered initialize"
    tail -5 "$WORK_DIR/steady_stderr.log" >&2 || true
elif [[ -s "$VMMAP_OUT" ]]; then
    RAW_FOOTPRINT=$(awk '/^Physical footprint:/{print $3; exit}' "$VMMAP_OUT")
    RAW_MALLOC_DIRTY=$(parse_vmmap_col "$VMMAP_OUT" "MALLOC_SMALL " 3)
    RAW_MAPPED_FILE=$(parse_vmmap_col "$VMMAP_OUT" "mapped file" 2)
    if [[ -n "$RAW_FOOTPRINT" ]]; then
        ROW4_FOOTPRINT="$(vmmap_size_to_mb "$RAW_FOOTPRINT")"
    else
        ROW4_STATUS="unmeasured: could not find 'Physical footprint:' in vmmap output"
    fi
    [[ -n "$RAW_MALLOC_DIRTY" ]] && ROW4_MALLOC_DIRTY="$(vmmap_size_to_mb "$RAW_MALLOC_DIRTY") MB"
    [[ -n "$RAW_MAPPED_FILE" ]] && ROW4_MAPPED_FILE="$(vmmap_size_to_mb "$RAW_MAPPED_FILE") MB"
else
    ROW4_STATUS="unmeasured: vmmap produced no output (process may have exited before t=6s)"
fi

# ---------------------------------------------------------------------------
# Row 5: drift +1 — one chunk_embeddings row the on-disk HNSW cache doesn't
# know about, on a fresh snapshot. The startup line is checked so the row
# says whether the engine took the additive-reconciliation path ("cached")
# or fell back to a full rebuild.
# ---------------------------------------------------------------------------
log "=== Row 5: drift +1 (bare initialize against a db with one un-cached chunk) ==="
ROW5_STATUS="ok"
ROW5_RSS_MB="n/a"
ROW5_WALL_S="n/a"
ROW5_FP_MB="n/a"
ROW5_DELTA_MB="n/a"
ROW5_PATH="unknown"
DRIFT_ID="csr-profile-drift-synthetic-$(date +%s)"
DRIFT_DB="$(snapshot_db "$WORK_DIR/drift")"

if [[ -n "$DRIFT_DB" ]]; then
    # Schema-inspect chunk_embeddings first rather than assuming shape:
    # chunk_id TEXT PK REFERENCES chunks(id), embedding BLOB (little-endian
    # f32, one row per chunk — 384 dims == 1536 bytes, matching
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
                DRIFT_OUT="$(run_timed_once "$INIT_INPUT" "$INIT_EXPECT" "$DRIFT_DB")"
                if [[ "$(echo "$DRIFT_OUT" | awk '{print $4}')" == "ok" ]]; then
                    read -r ROW5_RSS_MB ROW5_WALL_S ROW5_FP_MB _ <<<"$DRIFT_OUT"
                    ROW5_RSS_MB=$(bytes_to_mb "$ROW5_RSS_MB")
                    ROW5_FP_MB=$(bytes_to_mb "$ROW5_FP_MB")
                    if grep -q 'CSR startup:.*cached)' "$LAST_STDERR"; then
                        ROW5_PATH="cached+backfill"
                    elif grep -q 'CSR startup:.*rebuilt)' "$LAST_STDERR"; then
                        ROW5_PATH="full rebuild"
                    fi
                    if [[ "$ROW1_RSS_MB" != "unmeasured" ]]; then
                        ROW5_DELTA_MB=$(awk -v a="$ROW5_RSS_MB" -v b="$ROW1_RSS_MB" 'BEGIN{printf "%+.1f", a-b}')
                    fi
                else
                    ROW5_STATUS="unmeasured: the drift initialize trial failed (see stderr)"
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
    ROW5_STATUS="unmeasured: snapshot of the db failed (see stderr)"
fi

# ---------------------------------------------------------------------------
# Live data dir must be untouched
# ---------------------------------------------------------------------------
LIVE_TIMING_MTIME_AFTER="$(live_mtime hook-timing.log)"
LIVE_STAMP_MTIME_AFTER="$(live_mtime mcp-binary.txt)"
LIVE_DIR_NOTE="untouched (hook-timing.log, mcp-binary.txt mtimes unchanged)"
if [[ "$LIVE_TIMING_MTIME_BEFORE" != "$LIVE_TIMING_MTIME_AFTER" || "$LIVE_STAMP_MTIME_BEFORE" != "$LIVE_STAMP_MTIME_AFTER" ]]; then
    LIVE_DIR_NOTE="WRITTEN DURING THE RUN — isolation failed (hook-timing.log $LIVE_TIMING_MTIME_BEFORE -> $LIVE_TIMING_MTIME_AFTER, mcp-binary.txt $LIVE_STAMP_MTIME_BEFORE -> $LIVE_STAMP_MTIME_AFTER)"
    log "$LIVE_DIR_NOTE"
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
echo "- Isolation: child HOME = scratch dir (model cache copied in, probe caches copied in); hook rows on a fresh snapshot"
echo "- Live data dir ($LIVE_DATA_DIR): $LIVE_DIR_NOTE"
echo
echo "| # | Scenario | Peak RSS (MB) | Peak footprint (MB) | Wall (s) | Notes |"
echo "|---|----------|--------------:|--------------------:|---------:|-------|"
printf '| 1 | bare MCP initialize (median of 3) | %s | %s | %s | - |\n' "$ROW1_RSS_MB" "$ROW1_FP_MB" "$ROW1_WALL_S"
printf '| 2 | hook post-tool-use Edit, installed matcher (median of 3) | %s | %s | %s | %s |\n' "$ROW2_RSS_MB" "$ROW2_FP_MB" "$ROW2_WALL_S" "$ROW2_NOTE"
printf '| 3 | hook prompt-submit, real prompt (median of 3) | %s | %s | %s | %s |\n' "$ROW3_RSS_MB" "$ROW3_FP_MB" "$ROW3_WALL_S" "$ROW3_NOTE"
if [[ "$ROW4_STATUS" == "ok" ]]; then
    printf '| 4 | steady-state private footprint @ t=6s after initialize | n/a | %s | n/a | vmmap: MALLOC_SMALL dirty=%s; mapped file resident=%s |\n' \
        "$ROW4_FOOTPRINT" "$ROW4_MALLOC_DIRTY" "$ROW4_MAPPED_FILE"
else
    printf '| 4 | steady-state private footprint @ t=6s after initialize | n/a | unmeasured | n/a | %s |\n' "$ROW4_STATUS"
fi
if [[ "$ROW5_STATUS" == "ok" ]]; then
    printf '| 5 | drift +1 bare initialize | %s | %s | %s | RSS delta vs row 1 = %s MB; startup path = %s |\n' "$ROW5_RSS_MB" "$ROW5_FP_MB" "$ROW5_WALL_S" "$ROW5_DELTA_MB" "$ROW5_PATH"
else
    printf '| 5 | drift +1 bare initialize | unmeasured | n/a | n/a | %s |\n' "$ROW5_STATUS"
fi
