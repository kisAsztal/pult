#!/usr/bin/env bash
# Install pult on this machine (Linux / macOS). Idempotent: safe to run again to update.
#
#   1. the `pult` binary                  -> ~/.cargo/bin/pult
#   2. WezTerm config (if WezTerm exists) -> ~/.config/wezterm/wezterm.lua (symlink to this checkout)
#   3. Claude Code hooks                  -> ~/.claude/settings.json (backup first, existing hooks are kept)
#   4. pultd as a systemd user service    (Linux with systemd only)
#
# Options: --no-hooks, --no-service, --no-wezterm, --uninstall, -h|--help
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOOK_EVENTS='"SessionStart","SessionEnd","UserPromptSubmit","PreToolUse","PostToolUse","PostToolUseFailure","PermissionRequest","PermissionDenied","Notification","Stop","StopFailure","Elicitation","PreCompact"'
SETTINGS="$HOME/.claude/settings.json"
WEZ_CONFIG="$HOME/.config/wezterm/wezterm.lua"
UNIT="$HOME/.config/systemd/user/pultd.service"

do_hooks=1 do_service=1 do_wezterm=1 uninstall=0
for arg in "$@"; do
  case "$arg" in
    --no-hooks) do_hooks=0 ;;
    --no-service) do_service=0 ;;
    --no-wezterm) do_wezterm=0 ;;
    --uninstall) uninstall=1 ;;
    -h|--help) sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $arg (see --help)" >&2; exit 2 ;;
  esac
done

have_systemd_user() {
  [ "$(uname)" = Linux ] && command -v systemctl >/dev/null && systemctl --user show-environment >/dev/null 2>&1
}

backup_settings() {
  # settings.json may hold tokens: the backup and the new file are readable by the user only
  (umask 077; cp "$SETTINGS" "$SETTINGS.bak-pult-$(date +%Y%m%d%H%M%S)")
}

write_settings() { # jq options and filter, as for jq itself; the input is the settings file
  local tmp="$SETTINGS.new"
  (umask 077; jq "$@" "$SETTINGS" > "$tmp")
  mv "$tmp" "$SETTINGS"
}

if [ "$uninstall" = 1 ]; then
  if [ -f "$SETTINGS" ] && command -v jq >/dev/null; then
    echo "== removing the Claude Code hooks"
    backup_settings
    write_settings '.hooks |= (if . == null then . else
        with_entries(.value |= map(select(any(.hooks[]?; (.command | tostring | test("pult hook"))) | not)))
        | with_entries(select(.value | length > 0)) end)'
  fi
  if have_systemd_user && [ -f "$UNIT" ]; then
    echo "== removing the pultd service (running sessions end)"
    systemctl --user disable --now pultd.service >/dev/null 2>&1 || true
    rm -f "$UNIT"
    systemctl --user daemon-reload
  fi
  if [ -L "$WEZ_CONFIG" ] && [ "$(readlink "$WEZ_CONFIG")" = "$ROOT/wezterm/wezterm.lua" ]; then
    echo "== removing the WezTerm config symlink"
    rm -f "$WEZ_CONFIG"
  fi
  echo "== removing the pult binary"
  cargo uninstall pult 2>/dev/null || true
  echo "done. State and caches stay in ~/.local/state/pult and ~/.cache/pult."
  exit 0
fi

command -v cargo >/dev/null || { echo "Rust is required: https://rustup.rs" >&2; exit 1; }
if [ "$do_hooks" = 1 ]; then
  command -v jq >/dev/null || { echo "jq is required (apt install jq / brew install jq), or pass --no-hooks" >&2; exit 1; }
fi

echo "== pult binary"
cargo install --path "$ROOT/pult" --locked --quiet

if [ "$do_wezterm" = 1 ]; then
  if command -v wezterm >/dev/null || [ -x "$HOME/.local/lib/pult-term/wezterm" ]; then
    echo "== WezTerm config"
    mkdir -p "$(dirname "$WEZ_CONFIG")"
    if [ -e "$WEZ_CONFIG" ] && [ ! -L "$WEZ_CONFIG" ]; then
      mv "$WEZ_CONFIG" "$WEZ_CONFIG.bak-$(date +%Y%m%d%H%M%S)"
    fi
    ln -sfn "$ROOT/wezterm/wezterm.lua" "$WEZ_CONFIG"
    # the tab backgrounds are drawn by wezterm/bg.py; without Pillow the background stays plain
    if ! python3 -c 'import PIL' 2>/dev/null; then
      echo "   no python3 Pillow: the tab backgrounds stay plain. Install it, then restart WezTerm:"
      if [ "$(uname)" = Darwin ]; then echo "     python3 -m pip install --user Pillow"
      else echo "     sudo apt install python3-pil   (or your distribution's Pillow package)"; fi
    fi
  else
    echo "== no WezTerm found, skipping the config"
  fi
fi

if [ "$do_hooks" = 1 ]; then
  echo "== Claude Code hooks"
  mkdir -p "$(dirname "$SETTINGS")"
  [ -f "$SETTINGS" ] || (umask 077; echo '{}' > "$SETTINGS")
  backup_settings
  # keeps the existing hooks; adds pult's once per event
  write_settings --argjson h '[{"hooks":[{"type":"command","command":"~/.cargo/bin/pult hook","timeout":5}]}]' \
    ".hooks = (reduce ($HOOK_EVENTS) as \$e
      (.hooks // {}; .[\$e] = ((.[\$e] // []) | if any(.[].hooks[]?; .command | tostring | test(\"pult hook\")) then . else . + \$h end)))"
fi

if [ "$do_service" = 1 ] && have_systemd_user; then
  echo "== pultd systemd user service"
  mkdir -p "$(dirname "$UNIT")"
  cp "$ROOT/scripts/pultd.service" "$UNIT"
  systemctl --user daemon-reload
  systemctl --user enable pultd.service >/dev/null 2>&1
  # a pultd that is already running is not restarted: that would end every session
  if [ -S "${XDG_RUNTIME_DIR:-/nonexistent}/pult.sock" ]; then
    echo "   pultd is already running; the new version starts on its next launch"
    echo "   (systemctl --user restart pultd ends ALL sessions)"
  else
    systemctl --user start pultd.service
  fi
fi
echo "done."
