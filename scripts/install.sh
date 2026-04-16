#!/bin/sh
# Claude Self-Reflect — install script
# Downloads the csr-engine binary from GitHub Releases.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh

set -e

REPO="ramakay/claude-self-reflect"
INSTALL_DIR="${CSR_INSTALL_DIR:-$HOME/.local/bin}"
BINARY_NAME="csr-engine"

# --- Helpers ---

info()  { printf '  \033[1;34m%s\033[0m %s\n' "$1" "$2"; }
ok()    { printf '  \033[1;32m%s\033[0m %s\n' "$1" "$2"; }
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
        curl -fsSL "$CHECKSUM_URL" -o "${TMPDIR}/${CHECKSUM_FILE}" 2>/dev/null || true
    else
        wget -q "$URL" -O "${TMPDIR}/${TARBALL}" || err "Download failed. Is ${VERSION} released for ${TARGET}?"
        wget -q "$CHECKSUM_URL" -O "${TMPDIR}/${CHECKSUM_FILE}" 2>/dev/null || true
    fi

    # Verify checksum if available
    if [ -f "${TMPDIR}/${CHECKSUM_FILE}" ]; then
        EXPECTED="$(grep "${TARBALL}" "${TMPDIR}/${CHECKSUM_FILE}" | awk '{print $1}')"
        if [ -n "$EXPECTED" ]; then
            if command -v sha256sum >/dev/null 2>&1; then
                ACTUAL="$(sha256sum "${TMPDIR}/${TARBALL}" | awk '{print $1}')"
            elif command -v shasum >/dev/null 2>&1; then
                ACTUAL="$(shasum -a 256 "${TMPDIR}/${TARBALL}" | awk '{print $1}')"
            else
                ACTUAL=""
            fi

            if [ -n "$ACTUAL" ]; then
                if [ "$EXPECTED" = "$ACTUAL" ]; then
                    ok "Checksum" "verified"
                else
                    err "Checksum mismatch! Expected ${EXPECTED}, got ${ACTUAL}"
                fi
            fi
        fi
    fi

    # Extract
    info "Extracting" "to ${INSTALL_DIR}..."
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

verify() {
    if "${INSTALL_DIR}/${BINARY_NAME}" status --compact >/dev/null 2>&1; then
        ok "Verified" "binary works"
    elif "${INSTALL_DIR}/${BINARY_NAME}" --help >/dev/null 2>&1; then
        ok "Verified" "binary works"
    else
        err "Binary installed but failed to execute. Check architecture compatibility."
    fi
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

    printf '\n  \033[1mNext steps:\033[0m\n'
    printf '    %s setup\n' "$BINARY_NAME"
    printf '    # Restart Claude Code\n\n'
}

main
