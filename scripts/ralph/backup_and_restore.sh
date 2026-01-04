#!/bin/bash
# =============================================================================
# Ralph Memory Integration - Backup and Restore Script
# =============================================================================
# Usage:
#   ./backup_and_restore.sh backup           # Create full backup
#   ./backup_and_restore.sh restore <dir>    # Restore from backup directory
#   ./backup_and_restore.sh verify <dir>     # Verify backup integrity
#   ./backup_and_restore.sh list             # List available backups
# =============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BACKUP_BASE="$HOME/.claude-self-reflect/backups"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

# =============================================================================
# BACKUP FUNCTION
# =============================================================================
do_backup() {
    local BACKUP_DIR="$BACKUP_BASE/$(date +%Y%m%d_%H%M%S)_pre_ralph_memory"
    mkdir -p "$BACKUP_DIR"

    log_info "Creating backup at: $BACKUP_DIR"

    # 1. Stop services for consistent backup
    log_info "Stopping services for consistent backup..."
    docker stop claude-reflection-batch-watcher claude-reflection-batch-monitor 2>/dev/null || true
    sleep 2

    # 2. Backup Qdrant data volume
    log_info "Backing up Qdrant data volume..."
    if docker volume inspect qdrant_data > /dev/null 2>&1; then
        docker run --rm \
            -v qdrant_data:/data:ro \
            -v "$BACKUP_DIR":/backup \
            alpine tar czf /backup/qdrant_data.tar.gz -C /data .
        log_info "Qdrant backup: $(du -h "$BACKUP_DIR/qdrant_data.tar.gz" | cut -f1)"
    else
        log_warn "Qdrant volume not found, skipping"
    fi

    # 3. Backup CSR config directory
    log_info "Backing up CSR config..."
    if [ -d "$HOME/.claude-self-reflect/config" ]; then
        tar czf "$BACKUP_DIR/csr_config.tar.gz" -C "$HOME/.claude-self-reflect" config
    else
        log_warn "CSR config not found, skipping"
    fi

    # 4. Backup batch queue
    if [ -d "$HOME/.claude-self-reflect/batch_queue" ]; then
        tar czf "$BACKUP_DIR/csr_batch_queue.tar.gz" -C "$HOME/.claude-self-reflect" batch_queue
    fi

    # 5. Backup batch state
    if [ -d "$HOME/.claude-self-reflect/batch_state" ]; then
        tar czf "$BACKUP_DIR/csr_batch_state.tar.gz" -C "$HOME/.claude-self-reflect" batch_state
    fi

    # 6. Save git state
    log_info "Saving git state..."
    cd "$PROJECT_ROOT"
    echo "$(git rev-parse HEAD)" > "$BACKUP_DIR/git_head.txt"
    echo "$(git branch --show-current)" > "$BACKUP_DIR/git_branch.txt"
    git diff > "$BACKUP_DIR/git_diff.patch" 2>/dev/null || true
    git diff --cached > "$BACKUP_DIR/git_staged.patch" 2>/dev/null || true

    # 7. Restart services
    log_info "Restarting services..."
    docker start claude-reflection-batch-watcher claude-reflection-batch-monitor 2>/dev/null || true

    # 8. Create manifest
    cat > "$BACKUP_DIR/manifest.json" << EOF
{
    "created": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
    "git_commit": "$(cat "$BACKUP_DIR/git_head.txt")",
    "git_branch": "$(cat "$BACKUP_DIR/git_branch.txt")",
    "qdrant_backup": $([ -f "$BACKUP_DIR/qdrant_data.tar.gz" ] && echo "true" || echo "false"),
    "config_backup": $([ -f "$BACKUP_DIR/csr_config.tar.gz" ] && echo "true" || echo "false"),
    "project_root": "$PROJECT_ROOT"
}
EOF

    log_info "Backup complete!"
    echo ""
    echo "Backup directory: $BACKUP_DIR"
    echo "Files:"
    ls -lh "$BACKUP_DIR"
    echo ""
    echo "To restore: $0 restore $BACKUP_DIR"
}

# =============================================================================
# RESTORE FUNCTION
# =============================================================================
do_restore() {
    local BACKUP_DIR="$1"

    if [ -z "$BACKUP_DIR" ]; then
        log_error "Usage: $0 restore <backup_directory>"
        exit 1
    fi

    if [ ! -d "$BACKUP_DIR" ]; then
        log_error "Backup directory not found: $BACKUP_DIR"
        exit 1
    fi

    if [ ! -f "$BACKUP_DIR/manifest.json" ]; then
        log_error "Invalid backup: manifest.json not found"
        exit 1
    fi

    log_warn "This will restore system state from backup."
    log_warn "Current changes will be lost!"
    echo ""
    read -p "Are you sure? (type 'yes' to confirm): " confirm
    if [ "$confirm" != "yes" ]; then
        log_info "Restore cancelled"
        exit 0
    fi

    echo ""
    log_info "Starting restore from: $BACKUP_DIR"

    # 1. Stop all services
    log_info "Stopping all services..."
    docker stop claude-reflection-batch-watcher claude-reflection-batch-monitor claude-reflection-qdrant 2>/dev/null || true
    sleep 3

    # 2. Restore Qdrant data
    if [ -f "$BACKUP_DIR/qdrant_data.tar.gz" ]; then
        log_info "Restoring Qdrant data..."
        docker run --rm \
            -v qdrant_data:/data \
            -v "$BACKUP_DIR":/backup \
            alpine sh -c "rm -rf /data/* && tar xzf /backup/qdrant_data.tar.gz -C /data"
    fi

    # 3. Restore CSR config
    if [ -f "$BACKUP_DIR/csr_config.tar.gz" ]; then
        log_info "Restoring CSR config..."
        rm -rf "$HOME/.claude-self-reflect/config"
        tar xzf "$BACKUP_DIR/csr_config.tar.gz" -C "$HOME/.claude-self-reflect/"
    fi

    # 4. Restore batch queue
    if [ -f "$BACKUP_DIR/csr_batch_queue.tar.gz" ]; then
        rm -rf "$HOME/.claude-self-reflect/batch_queue"
        tar xzf "$BACKUP_DIR/csr_batch_queue.tar.gz" -C "$HOME/.claude-self-reflect/"
    fi

    # 5. Restore batch state
    if [ -f "$BACKUP_DIR/csr_batch_state.tar.gz" ]; then
        rm -rf "$HOME/.claude-self-reflect/batch_state"
        tar xzf "$BACKUP_DIR/csr_batch_state.tar.gz" -C "$HOME/.claude-self-reflect/"
    fi

    # 6. Restore git state
    log_info "Restoring git state..."
    cd "$PROJECT_ROOT"
    ORIGINAL_COMMIT=$(cat "$BACKUP_DIR/git_head.txt")
    git reset --hard "$ORIGINAL_COMMIT"

    # 7. Restart services
    log_info "Restarting services..."
    docker start claude-reflection-qdrant 2>/dev/null || true
    sleep 5
    docker start claude-reflection-batch-watcher claude-reflection-batch-monitor 2>/dev/null || true

    log_info "Restore complete!"
    echo ""
    echo "Verifying services..."
    docker ps --filter "name=claude" --format "table {{.Names}}\t{{.Status}}"
}

# =============================================================================
# VERIFY FUNCTION
# =============================================================================
do_verify() {
    local BACKUP_DIR="$1"

    if [ -z "$BACKUP_DIR" ]; then
        log_error "Usage: $0 verify <backup_directory>"
        exit 1
    fi

    if [ ! -d "$BACKUP_DIR" ]; then
        log_error "Backup directory not found: $BACKUP_DIR"
        exit 1
    fi

    echo "=========================================="
    echo "Verifying backup: $BACKUP_DIR"
    echo "=========================================="
    echo ""

    local ALL_OK=true

    # Check manifest
    if [ -f "$BACKUP_DIR/manifest.json" ]; then
        log_info "✓ Manifest found"
        cat "$BACKUP_DIR/manifest.json" | python3 -m json.tool 2>/dev/null || log_warn "Manifest is not valid JSON"
    else
        log_error "✗ Manifest missing"
        ALL_OK=false
    fi

    # Check Qdrant backup
    if [ -f "$BACKUP_DIR/qdrant_data.tar.gz" ]; then
        local SIZE=$(du -h "$BACKUP_DIR/qdrant_data.tar.gz" | cut -f1)
        log_info "✓ Qdrant backup ($SIZE)"
        # Verify tar integrity
        tar tzf "$BACKUP_DIR/qdrant_data.tar.gz" > /dev/null 2>&1 && log_info "  ✓ Archive integrity OK" || { log_error "  ✗ Archive corrupted"; ALL_OK=false; }
    else
        log_warn "✗ Qdrant backup missing"
    fi

    # Check config backup
    if [ -f "$BACKUP_DIR/csr_config.tar.gz" ]; then
        local SIZE=$(du -h "$BACKUP_DIR/csr_config.tar.gz" | cut -f1)
        log_info "✓ Config backup ($SIZE)"
    else
        log_warn "○ Config backup missing (optional)"
    fi

    # Check git state
    if [ -f "$BACKUP_DIR/git_head.txt" ]; then
        log_info "✓ Git state saved: $(cat "$BACKUP_DIR/git_head.txt" | head -c 8)..."
    else
        log_error "✗ Git state missing"
        ALL_OK=false
    fi

    echo ""
    if $ALL_OK; then
        log_info "Backup verification: PASSED"
    else
        log_error "Backup verification: FAILED"
        exit 1
    fi
}

# =============================================================================
# LIST FUNCTION
# =============================================================================
do_list() {
    echo "=========================================="
    echo "Available Backups"
    echo "=========================================="

    if [ ! -d "$BACKUP_BASE" ]; then
        log_info "No backups found at $BACKUP_BASE"
        exit 0
    fi

    for dir in "$BACKUP_BASE"/*; do
        if [ -d "$dir" ] && [ -f "$dir/manifest.json" ]; then
            local NAME=$(basename "$dir")
            local CREATED=$(python3 -c "import json; print(json.load(open('$dir/manifest.json'))['created'])" 2>/dev/null || echo "unknown")
            local SIZE=$(du -sh "$dir" | cut -f1)
            echo "  $NAME  ($SIZE, created: $CREATED)"
        fi
    done

    echo ""
    echo "To verify: $0 verify <backup_directory>"
    echo "To restore: $0 restore <backup_directory>"
}

# =============================================================================
# MAIN
# =============================================================================
case "$1" in
    backup)
        do_backup
        ;;
    restore)
        do_restore "$2"
        ;;
    verify)
        do_verify "$2"
        ;;
    list)
        do_list
        ;;
    *)
        echo "Usage: $0 {backup|restore|verify|list}"
        echo ""
        echo "Commands:"
        echo "  backup           Create full backup of Docker volumes and git state"
        echo "  restore <dir>    Restore from specified backup directory"
        echo "  verify <dir>     Verify backup integrity"
        echo "  list             List available backups"
        exit 1
        ;;
esac
