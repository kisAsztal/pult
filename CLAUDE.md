# CLAUDE.md

pult: persistent terminal sessions and live status tabs for Claude Code. One Rust binary (`pult/`) plus a WezTerm
config (`wezterm/`). Read `README.md` for use and `docs/architecture.md` for the design.

## Commands

- `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt` (max width 120)
- `scripts/install.sh [--no-hooks] [--no-service] [--no-wezterm] [--uninstall]`

## Things to keep in mind

- `pult hook` must exit 0, print nothing to stdout and be fast.
- The daemon never blocks on a client; per-client queues are bounded. Use `LockExt::lock_ok` (survives a poisoned
  mutex) instead of `lock().unwrap()`.
- Restarting a running `pultd` ends every session. Never restart it casually, and say so in instructions.
- The wire protocol (`src/proto.rs`) and the config formats are shared with other machines running possibly older
  versions: be careful with changes.
- `wezterm.lua` is evaluated in several places (GUI, mux): `wezterm.gui` may be missing. Setting a config override
  reloads the config, so compare before setting, or it loops.
- Terminal replies must not count as typing (`proto::is_terminal_reply`), or clients resize each other forever.
- Personal settings (fonts, host aliases) belong in `~/.config/pult/wezterm.lua`, not in the repo. No machine names,
  addresses or secrets anywhere in this repository.
