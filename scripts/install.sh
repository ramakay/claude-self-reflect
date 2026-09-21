#!/bin/sh
# Claude Self-Reflect — install script
# Downloads the csr-engine binary from GitHub Releases.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh
#
# Environment variables:
#   CSR_INSTALL_DIR    — Binary install directory (default: ~/.local/bin)
#   CSR_SKIP_SETUP=1   — Download only; never run `csr-engine setup`
#   CSR_AUTO_SETUP=1   — Run setup without prompting (automation)

set -e

REPO="ramakay/claude-self-reflect"
INSTALL_DIR="${CSR_INSTALL_DIR:-$HOME/.local/bin}"
BINARY_NAME="csr-engine"
# How to spell `csr-engine setup` in advice we print. check_shadow replaces this
# with the absolute path whenever the bare name would resolve to another copy.
SETUP_CMD="$BINARY_NAME"

# --- Helpers ---

info()  { printf '  \033[1;34m%s\033[0m %s\n' "$1" "$2"; }
ok()    { printf '  \033[1;32m%s\033[0m %s\n' "$1" "$2"; }
warn()  { printf '  \033[1;33m%s\033[0m %s\n' "WARNING:" "$1" >&2; }
err()   { printf '  \033[1;31m%s\033[0m %s\n' "ERROR:" "$1" >&2; exit 1; }

# --- Detect platform ---

detect_platform() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "$OS" in
        Darwin) OS_NAME="apple-darwin" ;;
        Linux)  OS_NAME="unknown-linux-gnu" ;;
        *)      err "Unsupported OS: $OS. Only macOS and Linux are supported." ;;
    esac

    case "$ARCH" in
        arm64|aarch64) ARCH_NAME="aarch64" ;;
        x86_64|amd64)  ARCH_NAME="x86_64" ;;
        *)             err "Unsupported architecture: $ARCH. Only arm64 and x86_64 are supported." ;;
    esac

    TARGET="${ARCH_NAME}-${OS_NAME}"
}

# --- Find latest release ---

get_latest_version() {
    if command -v curl >/dev/null 2>&1; then
        VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')"
    elif command -v wget >/dev/null 2>&1; then
        VERSION="$(wget -qO- "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')"
    else
        err "Neither curl nor wget found. Please install one."
    fi

    if [ -z "$VERSION" ]; then
        err "Could not determine latest release version. Check https://github.com/${REPO}/releases"
    fi
}

# --- Download and install ---

download_and_install() {
    TARBALL="csr-engine-${TARGET}.tar.gz"
    CHECKSUM_FILE="checksums.txt"
    URL="https://github.com/${REPO}/releases/download/${VERSION}/${TARBALL}"
    CHECKSUM_URL="https://github.com/${REPO}/releases/download/${VERSION}/${CHECKSUM_FILE}"

    TMPDIR="$(mktemp -d)"
    trap 'rm -rf "$TMPDIR"' EXIT

    info "Downloading" "${BINARY_NAME} ${VERSION} for ${TARGET}..."

    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$URL" -o "${TMPDIR}/${TARBALL}" || err "Download failed. Is ${VERSION} released for ${TARGET}?"
        curl -fsSL "$CHECKSUM_URL" -o "${TMPDIR}/${CHECKSUM_FILE}" 2>/dev/null || err "Checksum file not available for ${VERSION}; refusing to install unverified binary."
    else
        wget -q "$URL" -O "${TMPDIR}/${TARBALL}" || err "Download failed. Is ${VERSION} released for ${TARGET}?"
        wget -q "$CHECKSUM_URL" -O "${TMPDIR}/${CHECKSUM_FILE}" 2>/dev/null || err "Checksum file not available for ${VERSION}; refusing to install unverified binary."
    fi

    # Verify checksum from the release's published checksums.txt.
    EXPECTED="$(awk -v file="$TARBALL" '
        ($2 == file || $2 == "*" file) && length($1) == 64 && $1 !~ /[^A-Fa-f0-9]/ {
            print tolower($1)
            found=1
            exit
        }
        END { if (!found) exit 1 }
    ' "${TMPDIR}/${CHECKSUM_FILE}")" || err "Checksum entry not found for ${TARBALL}"

    if command -v sha256sum >/dev/null 2>&1; then
        ACTUAL="$(sha256sum "${TMPDIR}/${TARBALL}" | awk '{print tolower($1)}')"
    elif command -v shasum >/dev/null 2>&1; then
        ACTUAL="$(shasum -a 256 "${TMPDIR}/${TARBALL}" | awk '{print tolower($1)}')"
    else
        err "No sha256sum or shasum found; refusing to install unverified binary."
    fi

    if [ "$EXPECTED" = "$ACTUAL" ]; then
        ok "Checksum" "verified"
    else
        err "Checksum mismatch! Expected ${EXPECTED}, got ${ACTUAL}"
    fi

    # Extract
    info "Extracting" "${TARBALL}..."
    mkdir -p "$INSTALL_DIR"
    tar -xzf "${TMPDIR}/${TARBALL}" -C "$TMPDIR"

    # Find the binary (might be at root or in a subdirectory)
    BINARY_PATH="$(find "$TMPDIR" -name "$BINARY_NAME" -type f | head -1)"
    if [ -z "$BINARY_PATH" ]; then
        err "Binary not found in archive"
    fi

    cp "$BINARY_PATH" "${INSTALL_DIR}/${BINARY_NAME}"
    chmod +x "${INSTALL_DIR}/${BINARY_NAME}"

    ok "Installed" "${INSTALL_DIR}/${BINARY_NAME}"
}

# --- Ensure PATH ---

check_path() {
    case ":$PATH:" in
        *":${INSTALL_DIR}:"*) return ;;
    esac

    info "Note:" "${INSTALL_DIR} is not in your PATH."

    SHELL_NAME="$(basename "${SHELL:-/bin/sh}")"
    case "$SHELL_NAME" in
        zsh)  RC="$HOME/.zshrc" ;;
        bash) RC="$HOME/.bashrc" ;;
        fish) RC="$HOME/.config/fish/config.fish" ;;
        *)    RC="$HOME/.profile" ;;
    esac

    if [ -f "$RC" ] && grep -q "$INSTALL_DIR" "$RC" 2>/dev/null; then
        info "Found" "PATH entry in $RC (restart your shell)"
    else
        printf '\n  Add this to %s:\n' "$RC"
        if [ "$SHELL_NAME" = "fish" ]; then
            printf '    fish_add_path %s\n\n' "$INSTALL_DIR"
        else
            printf '    export PATH="%s:$PATH"\n\n' "$INSTALL_DIR"
        fi
    fi
}

# --- Verify ---

# `--version` and `--help` are the only invocations here: they exit before the
# database, the HNSW index or the model cache is touched. `status` opens the
# user's live database, which an installer has no business doing.
verify() {
    if "${INSTALL_DIR}/${BINARY_NAME}" --version >/dev/null 2>&1; then
        ok "Verified" "binary works"
    elif "${INSTALL_DIR}/${BINARY_NAME}" --help >/dev/null 2>&1; then
        ok "Verified" "binary works"
    else
        err "Binary installed but failed to execute. Check architecture compatibility."
    fi
}

# --- Shadowed installation ---

# Resolve symlinks where the platform can, otherwise return the input. Only
# used to compare two paths, so degrading to a string compare is safe.
resolve_path() {
    if command -v realpath >/dev/null 2>&1; then
        realpath "$1" 2>/dev/null || printf '%s\n' "$1"
    elif readlink -f / >/dev/null 2>&1; then
        readlink -f "$1" 2>/dev/null || printf '%s\n' "$1"
    else
        printf '%s\n' "$1"
    fi
}

# csr-engine paths a Claude Code config file has registered. Deliberately crude
# (no jq dependency) and fail-open: a missing, unreadable or unexpected file
# yields nothing, never an error.
registered_paths() {
    [ -r "$1" ] || return 0
    grep -o '"[^"]*/csr-engine[^"]*"' "$1" 2>/dev/null |
        tr -d '"' | awk '{print $1}' | grep '/csr-engine$' | sort -u || true
}

# Warn when something other than the binary we just wrote is the one that will
# actually run: hooks and the MCP server are registered with the absolute path
# of whichever copy ran setup. Read-only, never fatal — keeping a different
# build earlier on PATH is a legitimate choice.
check_shadow() {
    DEST="${INSTALL_DIR}/${BINARY_NAME}"
    DEST_REAL="$(resolve_path "$DEST")"
    SETUP_CMD="$BINARY_NAME"

    SHADOW=""
    ON_PATH="$(command -v "$BINARY_NAME" 2>/dev/null || true)"
    if [ -z "$ON_PATH" ]; then
        SETUP_CMD="$DEST"
    elif [ "$(resolve_path "$ON_PATH")" != "$DEST_REAL" ]; then
        SHADOW="$ON_PATH"
        SETUP_CMD="$DEST"
    fi

    STALE_HOOKS=""
    for p in $(registered_paths "$HOME/.claude/settings.json"); do
        if [ "$(resolve_path "$p")" != "$DEST_REAL" ]; then
            STALE_HOOKS="$p"
        fi
    done

    STALE_MCP=""
    for p in $(registered_paths "$HOME/.claude.json"); do
        if [ "$(resolve_path "$p")" != "$DEST_REAL" ]; then
            STALE_MCP="$p"
        fi
    done

    if [ -z "$SHADOW" ] && [ -z "$STALE_HOOKS" ] && [ -z "$STALE_MCP" ]; then
        return 0
    fi
    SETUP_CMD="$DEST"

    printf '\n  \033[1;33mWARNING: a different csr-engine is still the one in use.\033[0m\n\n'
    printf '  Just installed:  %s\n' "$DEST"
    if [ -n "$SHADOW" ]; then
        printf '  First on PATH:   %s\n' "$SHADOW"
    fi
    if [ -n "$STALE_HOOKS" ]; then
        printf '  Claude Code hooks: %s\n' "$STALE_HOOKS"
    fi
    if [ -n "$STALE_MCP" ]; then
        printf '  MCP server:        %s\n' "$STALE_MCP"
    fi
    printf '\n  Nothing outside %s was changed — the other copy was left in place.\n' "$INSTALL_DIR"

    if [ -n "$STALE_HOOKS" ] || [ -n "$STALE_MCP" ]; then
        printf '\n  To point Claude Code at the binary just installed:\n\n'
        if [ -n "$STALE_MCP" ]; then
            # `claude mcp add` refuses to overwrite an existing entry, so
            # re-running setup on its own cannot repoint the MCP server.
            printf '    claude mcp remove claude-self-reflect -s user\n'
        fi
        printf '    %s setup\n' "$DEST"
        printf '    # Then restart Claude Code\n'
    fi
    if [ -n "$SHADOW" ]; then
        printf '\n  The bare %s command still resolves to %s.\n' "$BINARY_NAME" "$SHADOW"
        printf '  Run %s by absolute path, or put %s earlier on PATH.\n' "$DEST" "$INSTALL_DIR"
    fi
    printf '\n'
}

# --- Main ---

main() {
    printf '\n  \033[1mClaude Self-Reflect Installer\033[0m\n\n'

    detect_platform

    # Intel Mac: no prebuilt binaries (ort/ONNX dropped x86_64-apple-darwin)
    if [ "$TARGET" = "x86_64-apple-darwin" ]; then
        err "Intel Mac (x86_64) binaries are not provided.
Build from source instead:
  git clone https://github.com/${REPO}.git
  cd claude-self-reflect/csr-engine
  cargo build --release
  cp target/release/csr-engine ~/.local/bin/"
    fi

    get_latest_version
    download_and_install
    verify
    check_path
    check_shadow

    # Setup writes hooks into ~/.claude/settings.json, registers the MCP
    # server, and imports conversation transcripts — that needs explicit
    # consent. Prompt when a terminal is available; otherwise (CI, piped
    # non-interactive shells) leave activation to the user.
    RUN_SETUP=no
    if [ "${CSR_SKIP_SETUP:-}" = "1" ]; then
        RUN_SETUP=no
    elif [ "${CSR_AUTO_SETUP:-}" = "1" ]; then
        RUN_SETUP=yes
    elif [ -r /dev/tty ] && [ -w /dev/tty ]; then
        printf '\n  Setup registers the MCP server, installs 6 Claude Code hooks\n' > /dev/tty
        printf '  into ~/.claude/settings.json, and imports your conversations\n' > /dev/tty
        printf '  from ~/.claude/projects/ into a local index.\n\n' > /dev/tty
        printf '  \033[1mRun setup now? [Y/n]\033[0m ' > /dev/tty
        answer=""
        # A failed read is not consent — only a successful (possibly empty)
        # response may select the yes-default.
        if read -r answer < /dev/tty; then
            case "$answer" in
                [Nn]*) RUN_SETUP=no ;;
                *)     RUN_SETUP=yes ;;
            esac
        else
            RUN_SETUP=no
        fi
    fi

    if [ "$RUN_SETUP" = "yes" ]; then
        printf '\n  \033[1mRunning setup...\033[0m\n\n'
        if "${INSTALL_DIR}/${BINARY_NAME}" setup 2>&1; then
            printf '\n  \033[32m✓\033[0m  Done. Restart Claude Code to activate.\n\n'
        else
            printf '\n  \033[33m⚠\033[0m  Setup encountered errors. Try manually:\n'
            printf '    %s setup\n' "$SETUP_CMD"
            printf '    # Then restart Claude Code\n\n'
            exit 1
        fi
    else
        printf '\n  \033[1mNot yet active.\033[0m To register MCP, install hooks, and import conversations:\n'
        printf '    %s setup\n' "$SETUP_CMD"
        printf '    # Then restart Claude Code\n\n'
    fi
}

main
