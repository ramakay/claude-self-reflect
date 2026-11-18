#!/usr/bin/env bash
# Installs or rolls back the csr-pane mod through a local marketplace.
#   install.sh install    copy the mod, write the marketplace, add it, install the plugin
#   install.sh rollback   uninstall the plugin, remove the marketplace (keeps $DEST)
# CSR_PANE_DEST overrides the marketplace folder. CSR_PANE_DRY=1 prints the claude commands.
set -euo pipefail

MOD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEST="${CSR_PANE_DEST:-$HOME/.claude-self-reflect/csr-pane-mod/marketplace}"
MARKET="self-reflect-mods"
PLUGIN="csr-pane"

# Every claude call goes through here, so the dry run cannot leak a real one.
cc() {
  if [ "${CSR_PANE_DRY:-0}" = "1" ]; then
    echo "[dry] claude $*"
  else
    claude "$@"
  fi
}

# Runs a claude call that may find nothing to do; says so instead of failing.
tolerant() {
  if ! cc "$@"; then echo "note: 'claude $*' did nothing (not installed?)"; fi
}

install_mod() {
  local version
  version="$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$MOD/.claude-plugin/plugin.json" | head -1)"
  [ -n "$version" ] || { echo "no version in $MOD/.claude-plugin/plugin.json" >&2; exit 1; }

  rm -rf "$DEST/plugins/$PLUGIN"
  mkdir -p "$DEST/plugins/$PLUGIN" "$DEST/.claude-plugin"
  # Tracked and untracked-but-not-ignored files; install.sh itself stays behind.
  (cd "$MOD" && git ls-files -z --cached --others --exclude-standard | grep -zv '^install\.sh$' | tar --null -T - -cf -) |
    tar -xf - -C "$DEST/plugins/$PLUGIN"

  cat > "$DEST/.claude-plugin/marketplace.json" <<JSON
{
  "name": "$MARKET",
  "owner": { "name": "ramakay" },
  "plugins": [
    { "name": "$PLUGIN", "source": "./plugins/$PLUGIN", "version": "$version", "description": "The Self-Reflect pane inside Claude Code's own pane." }
  ]
}
JSON

  if [ "${CSR_PANE_DRY:-0}" = "1" ] || ! claude plugin marketplace list 2>/dev/null | grep -q "$MARKET"; then
    cc plugin marketplace add "$DEST"
  else
    echo "marketplace $MARKET already added"
  fi
  cc plugin install "$PLUGIN@$MARKET"
  echo "installed $PLUGIN $version from $DEST"
  echo "rollback: $MOD/install.sh rollback"
}

rollback_mod() {
  tolerant plugin uninstall "$PLUGIN@$MARKET"
  tolerant plugin marketplace remove "$MARKET"
  echo "rolled back; $DEST is kept"
}

case "${1:-}" in
  install) install_mod ;;
  rollback) rollback_mod ;;
  *) echo "usage: $0 install|rollback" >&2; exit 2 ;;
esac
