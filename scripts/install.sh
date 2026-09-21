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
BINARY_NAME="csr-engine"
# How to spell `csr-engine setup` in advice we print. check_shadow replaces this
# with the absolute path whenever the bare name would resolve to another copy.
SETUP_CMD="$BINARY_NAME"

# --- Helpers ---

info()  { printf '  \033[1;34m%s\033[0m %s\n' "$1" "$2"; }
ok()    { printf '  \033[1;32m%s\033[0m %s\n' "$1" "$2"; }
warn()  { printf '  \033[1;33m%s\033[0m %s\n' "WARNING:" "$1" >&2; }
err()   { printf '  \033[1;31m%s\033[0m %s\n' "ERROR:" "$1" >&2; exit 1; }

# --- Install directory ---

# Without HOME there is no sane default: the old expansion produced /.local/bin
# and the config scans read /.claude.json. Ask for a destination instead.
if [ -n "${CSR_INSTALL_DIR:-}" ]; then
    INSTALL_DIR="$CSR_INSTALL_DIR"
elif [ -n "${HOME:-}" ]; then
    INSTALL_DIR="$HOME/.local/bin"
else
    err "HOME is not set. Set CSR_INSTALL_DIR to choose an install directory."
fi

# Absolute, so every path we print is runnable from anywhere.
case "$INSTALL_DIR" in
    /*) ;;
    *)  INSTALL_DIR="$PWD/$INSTALL_DIR" ;;
esac

# Quote a path for pasting into a shell, but only when it needs it, so the
# ordinary hint stays readable. An install directory containing a space would
# otherwise produce a command that runs only its first word.
SQ="'"
shell_quote() {
    case "$1" in
        *[!A-Za-z0-9_./-]*) ;;
        *) printf '%s\n' "$1"; return 0 ;;
    esac
    _rest="$1"
    _out="$SQ"
    while :; do
        case "$_rest" in
            *"$SQ"*)
                _out="${_out}${_rest%%"$SQ"*}${SQ}\\${SQ}${SQ}"
                _rest="${_rest#*"$SQ"}"
                ;;
            *)
                _out="${_out}${_rest}${SQ}"
                break
                ;;
        esac
    done
    printf '%s\n' "$_out"
}

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
    STAGE_DIR=""
    trap 'rm -rf "$TMPDIR"; [ -n "$STAGE_DIR" ] && rm -rf "$STAGE_DIR"' EXIT

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

    # `mv file some-directory` moves the file *into* the directory, which would
    # report a successful install and leave a stray temp file behind. `-d`
    # follows symlinks, so this also refuses a destination link pointing at a
    # directory — which would land the binary outside INSTALL_DIR entirely. A
    # link to a *file* is still fine: the rename replaces the link itself.
    if [ -d "${INSTALL_DIR}/${BINARY_NAME}" ]; then
        err "${INSTALL_DIR}/${BINARY_NAME} is a directory (or a link to one). Remove it, or set CSR_INSTALL_DIR elsewhere."
    fi

    # Scope note: none of this defends against a principal that can already
    # write to INSTALL_DIR. Such a principal can rename the staging directory
    # from its writable parent, or swap the destination between the check above
    # and the move below. The default ~/.local/bin is user-owned, so that
    # principal is the user; pointing CSR_INSTALL_DIR at a shared writable
    # directory is the user's choice and is not a threat model we cover. What
    # follows guards against the accidents — a symlink or hard link left at the
    # destination, a half-written binary — not against a co-resident attacker.
    #
    # Stage, then rename over the destination. Copying onto the destination
    # would follow a symlink or hard link sitting there and overwrite a binary
    # elsewhere on the system, and an interrupted copy would truncate the
    # existing executable. Rename is atomic and cannot hit ETXTBSY on Linux.
    #
    # The stage lives in its own 0700 directory that mktemp creates. Staging a
    # bare file would mean reopening the name mktemp just closed, and anyone who
    # can write to INSTALL_DIR could unlink it in between and leave a symlink
    # for `cat` to follow. Nobody else can enter the private directory, and it
    # is on the same filesystem, so the move is still a rename.
    STAGE_DIR="$(mktemp -d "${INSTALL_DIR}/.${BINARY_NAME}.XXXXXX")" ||
        err "Could not create a staging directory in ${INSTALL_DIR}"
    chmod 700 "$STAGE_DIR"
    cat "$BINARY_PATH" > "${STAGE_DIR}/${BINARY_NAME}"
    chmod 755 "${STAGE_DIR}/${BINARY_NAME}"
    mv -f "${STAGE_DIR}/${BINARY_NAME}" "${INSTALL_DIR}/${BINARY_NAME}"
    rmdir "$STAGE_DIR"
    STAGE_DIR=""

    ok "Installed" "${INSTALL_DIR}/${BINARY_NAME}"
}

# --- Ensure PATH ---

check_path() {
    case ":$PATH:" in
        *":${INSTALL_DIR}:"*) return ;;
    esac

    info "Note:" "${INSTALL_DIR} is not in your PATH."

    SHELL_NAME="$(basename "${SHELL:-/bin/sh}")"
    # Quoted: this line is meant to be pasted, and an install directory with
    # a space would otherwise put only its first word on PATH.
    if [ "$SHELL_NAME" = "fish" ]; then
        PATH_LINE="fish_add_path $(shell_quote "$INSTALL_DIR")"
    else
        PATH_LINE="export PATH=$(shell_quote "$INSTALL_DIR"):\"\$PATH\""
    fi

    # Only reachable with CSR_INSTALL_DIR set (the top of the script refuses
    # to run without HOME otherwise). There is no rc file to name, and
    # "/.zshrc" would be a wrong instruction, so give the line on its own.
    if [ -z "${HOME:-}" ]; then
        printf '\n  Add this to your shell configuration:\n    %s\n\n' "$PATH_LINE"
        return
    fi

    case "$SHELL_NAME" in
        zsh)  RC="$HOME/.zshrc" ;;
        bash) RC="$HOME/.bashrc" ;;
        fish) RC="$HOME/.config/fish/config.fish" ;;
        *)    RC="$HOME/.profile" ;;
    esac

    if [ -f "$RC" ] && grep -q "$INSTALL_DIR" "$RC" 2>/dev/null; then
        info "Found" "PATH entry in $RC (restart your shell)"
    else
        printf '\n  Add this to %s:\n    %s\n\n' "$RC" "$PATH_LINE"
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

# A JSON reader, or nothing. Grep cannot tell the user-scope MCP entry from a
# project key or a working directory that merely ends in /csr-engine, and it
# loses paths containing spaces, so abstain rather than guess.
JSON_READER=""
pick_json_reader() {
    if usable_python3; then
        JSON_READER="python3"
    elif command -v node >/dev/null 2>&1; then
        JSON_READER="node"
    fi
}

usable_python3() {
    command -v python3 >/dev/null 2>&1 || return 1
    # On macOS /usr/bin/python3 is a stub that pops the Xcode Command Line Tools
    # installer when the tools are absent. Never run it in that state — this is
    # a `curl | sh` installer. `xcode-select -p` only reports, it never prompts.
    if [ "$(uname -s)" = "Darwin" ] && [ "$(command -v python3)" = "/usr/bin/python3" ]; then
        xcode-select -p >/dev/null 2>&1 || return 1
    fi
    return 0
}

# Print the executables Claude Code has registered, one per line: the binary
# behind every hook command ($2 = hooks) or the user-scope MCP server command
# ($2 = mcp).
#
# Both readers open the file once with O_NONBLOCK, fstat that descriptor and
# read at most the cap plus one byte from it — checking the pathname and then
# reopening it would let the file grow past the cap, or be swapped for a FIFO,
# between the two syscalls. Fail-open throughout: a non-regular or oversized
# file, an unreadable one and malformed JSON all yield nothing.
#
# They also decode the executable, because setup now writes it POSIX-quoted
# (`'/tmp/o'\''brien/CSR Tools/csr-engine' hook stop`) and unpicking `'\''` in
# sh would be worse than doing it twice here. Decoding stays lenient: earlier
# releases wrote the bare unquoted form and those settings files still exist.
read_commands() {
    case "$JSON_READER" in
        python3)
            python3 - "$1" "$2" 2>/dev/null <<'PY' || true
import json, os, stat, sys

CAP = 33554432
path, mode = sys.argv[1], sys.argv[2]

try:
    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
except Exception:
    sys.exit(0)
try:
    info = os.fstat(fd)
    if not stat.S_ISREG(info.st_mode):
        sys.exit(0)
    raw = b""
    while len(raw) <= CAP:
        chunk = os.read(fd, min(1 << 20, CAP + 1 - len(raw)))
        if not chunk:
            break
        raw += chunk
    if len(raw) > CAP:
        sys.exit(0)
    data = json.loads(raw.decode("utf-8", "replace"))
except Exception:
    sys.exit(0)
finally:
    os.close(fd)

if not isinstance(data, dict):
    sys.exit(0)


def executable(command):
    command = command.strip()
    if command.startswith("'"):
        out = []
        i = 1
        while i < len(command):
            if command[i] != "'":
                out.append(command[i])
                i += 1
            elif command[i:i + 4] == "'\\''":
                out.append("'")
                i += 4
            else:
                # A quoted word has to end the word: '/opt/csr-engine'junk runs
                # /opt/csr-enginejunk, so decoding it as ours would invent a
                # stale registration that does not exist.
                rest = command[i + 1:]
                if rest and not rest.startswith(" hook "):
                    return ""
                return "".join(out)
        return ""
    marker = command.find(" hook ")
    token = command[:marker] if marker != -1 else command.split(" ")[0]
    token = token.strip()
    if len(token) >= 2 and token[0] == token[-1] and token[0] == '"':
        token = token[1:-1]
    return token


out = []
if mode == "hooks":
    hooks = data.get("hooks")
    if isinstance(hooks, dict):
        for entries in hooks.values():
            if not isinstance(entries, list):
                continue
            for entry in entries:
                inner = entry.get("hooks") if isinstance(entry, dict) else None
                for hook in inner if isinstance(inner, list) else []:
                    command = hook.get("command") if isinstance(hook, dict) else None
                    if isinstance(command, str):
                        out.append(executable(command))
else:
    servers = data.get("mcpServers")
    server = servers.get("claude-self-reflect") if isinstance(servers, dict) else None
    command = server.get("command") if isinstance(server, dict) else None
    if isinstance(command, str):
        out.append(command.strip())

# One path per line. A path containing a newline cannot be framed this way and
# is skipped on purpose: the shell reads this with `IFS= read -r`, and no
# encoding scheme is worth carrying for a filename nobody has.
for line in out:
    if line and "\n" not in line:
        print(line)
PY
            ;;
        node)
            node - "$1" "$2" 2>/dev/null <<'JS' || true
const fs = require("fs");
const CAP = 33554432;
const path = process.argv[2];
const mode = process.argv[3];

let data;
let fd = null;
try {
  fd = fs.openSync(path, fs.constants.O_RDONLY | fs.constants.O_NONBLOCK);
  const info = fs.fstatSync(fd);
  if (!info.isFile()) process.exit(0);
  const buffer = Buffer.allocUnsafe(CAP + 1);
  let total = 0;
  for (;;) {
    const read = fs.readSync(fd, buffer, total, Math.min(1 << 20, CAP + 1 - total), null);
    if (read <= 0) break;
    total += read;
    if (total > CAP) process.exit(0);
  }
  data = JSON.parse(buffer.subarray(0, total).toString("utf8"));
} catch {
  process.exit(0);
} finally {
  if (fd !== null) {
    try {
      fs.closeSync(fd);
    } catch {}
  }
}
if (!data || typeof data !== "object") process.exit(0);

function executable(command) {
  const trimmed = command.trim();
  if (trimmed.startsWith("'")) {
    let out = "";
    let i = 1;
    while (i < trimmed.length) {
      if (trimmed[i] !== "'") {
        out += trimmed[i];
        i += 1;
      } else if (trimmed.startsWith("'\\''", i)) {
        out += "'";
        i += 4;
      } else {
        // A quoted word has to end the word: '/opt/csr-engine'junk runs
        // /opt/csr-enginejunk, so decoding it as ours would invent a stale
        // registration that does not exist.
        const rest = trimmed.slice(i + 1);
        if (rest !== "" && !rest.startsWith(" hook ")) return "";
        return out;
      }
    }
    return "";
  }
  const marker = trimmed.indexOf(" hook ");
  let token = (marker === -1 ? trimmed.split(" ")[0] : trimmed.slice(0, marker)).trim();
  if (token.length >= 2 && token[0] === '"' && token[token.length - 1] === '"') {
    token = token.slice(1, -1);
  }
  return token;
}

const out = [];
if (mode === "hooks") {
  const hooks = data.hooks;
  if (hooks && typeof hooks === "object") {
    for (const entries of Object.values(hooks)) {
      if (!Array.isArray(entries)) continue;
      for (const entry of entries) {
        const inner = entry && Array.isArray(entry.hooks) ? entry.hooks : [];
        for (const hook of inner) {
          if (hook && typeof hook.command === "string") out.push(executable(hook.command));
        }
      }
    }
  }
} else {
  const servers = data.mcpServers;
  const server = servers && typeof servers === "object" ? servers["claude-self-reflect"] : null;
  if (server && typeof server.command === "string") out.push(server.command.trim());
}
// One path per line. A path containing a newline cannot be framed this way and
// is skipped on purpose: the shell reads this with `IFS= read -r`, and no
// encoding scheme is worth carrying for a filename nobody has.
for (const line of out) if (line && !line.includes("\n")) console.log(line);
JS
            ;;
        *) return 0 ;;
    esac
}

# Keep only absolute paths, and for hooks only ones actually named csr-engine.
registered_executables() {
    _file="$1"
    _mode="$2"
    [ -f "$_file" ] || return 0
    read_commands "$_file" "$_mode" | while IFS= read -r _exe; do
        case "$_exe" in
            /*/"$BINARY_NAME") printf '%s\n' "$_exe" ;;
            /*)
                # Any absolute command registered under our own MCP name is
                # ours to report, whatever it is called.
                if [ "$_mode" = "mcp" ]; then
                    printf '%s\n' "$_exe"
                fi
                ;;
        esac
    done
}

# Registrations that point somewhere other than the binary we just installed.
# Sets STALE_HOOKS and STALE_MCP. Read-only and fail-open throughout.
scan_registrations() {
    STALE_HOOKS=""
    STALE_MCP=""
    [ -n "${HOME:-}" ] || return 0

    while IFS= read -r p; do
        [ -n "$p" ] || continue
        if [ "$(resolve_path "$p")" != "$DEST_REAL" ]; then
            STALE_HOOKS="$p"
        fi
    done <<EOF
$(registered_executables "$HOME/.claude/settings.json" hooks)
EOF

    while IFS= read -r p; do
        [ -n "$p" ] || continue
        if [ "$(resolve_path "$p")" != "$DEST_REAL" ]; then
            STALE_MCP="$p"
        fi
    done <<EOF
$(registered_executables "$HOME/.claude.json" mcp)
EOF
}

# Warn when something other than the binary we just wrote is the one that will
# actually run: hooks and the MCP server are registered with the absolute path
# of whichever copy ran setup. Read-only, never fatal — keeping a different
# build earlier on PATH is a legitimate choice.
check_shadow() {
    DEST="${INSTALL_DIR}/${BINARY_NAME}"
    DEST_REAL="$(resolve_path "$DEST")"
    DEST_QUOTED="$(shell_quote "$DEST")"
    SETUP_CMD="$BINARY_NAME"

    SHADOW=""
    ON_PATH="$(command -v "$BINARY_NAME" 2>/dev/null || true)"
    if [ -z "$ON_PATH" ]; then
        SETUP_CMD="$DEST_QUOTED"
    elif [ "$(resolve_path "$ON_PATH")" != "$DEST_REAL" ]; then
        SHADOW="$ON_PATH"
        SETUP_CMD="$DEST_QUOTED"
    fi

    pick_json_reader
    scan_registrations

    if [ -z "$SHADOW" ] && [ -z "$STALE_HOOKS" ] && [ -z "$STALE_MCP" ]; then
        return 0
    fi
    SETUP_CMD="$DEST_QUOTED"

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
        # One setup run repoints both: the hook merge evicts CSR entries by
        # command content rather than by path, and register_mcp_server removes
        # an existing user-scope registration before re-adding it.
        printf '\n  To point Claude Code at the binary just installed:\n\n'
        printf '    %s setup\n' "$DEST_QUOTED"
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
    # `-w /dev/tty` can pass while the process has no controlling terminal, and
    # the first `> /dev/tty` would then abort the install under `set -e`. Open
    # it for real in a subshell: a failure there is just a false condition.
    elif [ -r /dev/tty ] && ( : > /dev/tty ) 2>/dev/null; then
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
            # Setup exiting 0 is not evidence that the registrations moved.
            # Re-read them: a hook or MCP entry still naming another binary
            # means Claude Code keeps launching that one.
            scan_registrations
            if [ -n "$STALE_HOOKS" ] || [ -n "$STALE_MCP" ]; then
                printf '\n  \033[1;33m⚠  Not active yet.\033[0m Setup ran, but Claude Code still points at\n'
                printf '  another csr-engine:\n\n'
                if [ -n "$STALE_HOOKS" ]; then
                    printf '    %s  (Claude Code hooks)\n' "$STALE_HOOKS"
                fi
                if [ -n "$STALE_MCP" ]; then
                    printf '    %s  (MCP server)\n' "$STALE_MCP"
                fi
                printf '\n  Register it by hand, then restart Claude Code:\n\n'
                printf '    claude mcp remove claude-self-reflect -s user\n'
                printf '    %s setup\n\n' "$SETUP_CMD"
                exit 1
            fi
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
