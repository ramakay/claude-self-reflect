#!/bin/bash

# Script to add PCRE2 vulnerability mitigation to all affected Dockerfiles
# CVE-2025-58050: Heap buffer overflow in PCRE2 10.45-1

echo "Adding PCRE2 vulnerability mitigation to Dockerfiles..."
echo "======================================================="

# List of affected slim-based Dockerfiles
SLIM_DOCKERFILES=(
    "Dockerfile.async-importer"
    "Dockerfile.importer"
    "Dockerfile.importer-isolated"
    "Dockerfile.mcp-server"
    "Dockerfile.safe-watcher"
    "Dockerfile.streaming-importer"
    "Dockerfile.watcher"
)

for dockerfile in "${SLIM_DOCKERFILES[@]}"; do
    echo "Processing $dockerfile..."
    
    # Check if mitigation comment already exists
    if grep -q "CVE-2025-58050" "$dockerfile" 2>/dev/null; then
        echo "  ✓ Already has mitigation comment"
    else
        # Add mitigation as a temporary measure
        # We'll add a comment and explicit upgrade attempt for PCRE2
        cat > "${dockerfile}.tmp" << 'EOF'
# SECURITY: CVE-2025-58050 mitigation
# Heap buffer overflow in PCRE2 10.45-1
# Attempting explicit upgrade of libpcre2-8-0
# TODO: Remove when base image includes PCRE2 10.46+
RUN apt-get update && \
    (apt-get install -y --only-upgrade libpcre2-8-0 2>/dev/null || \
     echo "Warning: PCRE2 patch not yet available in Debian stable") && \
    apt-get upgrade -y && \
    rm -rf /var/lib/apt/lists/*

EOF
        
        # Insert after the FROM line
        awk '/^FROM/ {print; getline; print; system("cat " FILENAME ".tmp"); next} 1' "$dockerfile" > "${dockerfile}.new"
        mv "${dockerfile}.new" "$dockerfile"
        rm "${dockerfile}.tmp"
        
        echo "  ✓ Added mitigation"
    fi
done

echo ""
echo "Mitigation added to all affected Dockerfiles."
echo "Note: This is a temporary measure until Debian releases the patched package."