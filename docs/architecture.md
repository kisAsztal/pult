# Architecture

One binary, `pult`, with two roles: a short-lived client (`pult new`, `attach`, `ls`, `kill`, `hook`) and the
long-lived daemon (`pult daemon`, called **pultd**). The terminal is a WezTerm config that only draws and
dispatches keys.

```
Claude Code ──hook──▶ pult hook ──OSC 1337 SetUserVar──▶ terminal tab (colour, notifications)
                          └──▶ ~/.local/state/pult/sessions/<claude-session>.json

terminal tab ──▶ pult new/attach ──unix socket──▶ pultd ──PTY──▶ shell / claude
                                                  (vt100 emulator keeps the screen)
other host   ◀──────────────── ssh ───────────────▶ pult on that host
```

## Source map

| File | Role |
|------|------|
| `src/main.rs` | CLI dispatch, the Claude hook, state files, OSC user vars |
| `src/daemon.rs` | pultd: sessions, clients, snapshots, process inspection |
| `src/client.rs` | `new` / `attach` (raw terminal, frame pump), `ls`, `kill`, image paste |
| `src/proto.rs` | wire protocol, socket path, terminal-reply detection |
| `src/hosts.rs` | host list, SSH execution, project directories |
| `src/awake.rs` | keep-awake while a Claude works |
| `wezterm/wezterm.lua` | tab colours, pickers, notifications, keys, restore at startup |
| `wezterm/bg.py` | generated background images |

## Wire protocol

A connection to the daemon socket starts with one JSON line, the request:
`{"cmd": "ls" | "kill" | "new" | "attach", ...}`, answered by one JSON line. `ls` and `kill` end there. After a
successful `new` or `attach` the connection carries binary frames in both directions:

```
[kind u8][length u32 big endian][payload]
kind 0 DATA    client → daemon: keystrokes     daemon → client: terminal output
kind 1 RESIZE  client → daemon: cols u16, rows u16
kind 2 EXIT    daemon → client: the session ended
```

Frames are limited to 16 MiB, request lines to 1 MiB.

## Sessions

A session is a PTY with a child process (the default shell, or the command after `--`). The PTY output goes to a
`vt100` parser with 10 000 lines of scrollback and to every attached client. When a client attaches, the daemon
builds a **snapshot** (scrollback, screen, cursor, modes, the pult user vars, the directory as OSC 7, the title) from
the emulator and sends it first, under the same lock that subscribes the client, so no output is lost or doubled.

**Slow clients** never slow the session down. Each client has a bounded queue (64 chunks, ~4 MB). A client that
falls behind loses its backlog and receives a fresh snapshot as soon as it reads again.

**Several clients** see the same screen and any of them may type. The PTY size belongs to whoever typed or attached
last. Automatic terminal replies (focus reports, device attributes, cursor position, colour answers; see
`proto::is_terminal_reply`) do not count as typing, and those from a client that does not own the size are dropped.
Without that, two windows of different sizes would resize the session back and forth forever, because a TUI
re-queries the terminal after every resize.

**Lifetime**: when the child exits the session ends. When the last client leaves and nothing runs in a plain shell
(the shell is in the foreground and has no children) the session is stopped; anything running keeps it alive. A
command session (`pult new -- cmd`) is always kept until the command ends.

The daemon also polls each attached session's foreground process once a second and tells the clients its
directory with OSC 7, which the terminal uses for the tab title and for the directory of new tabs.

## Status

`pult hook` maps Claude Code hook events to a status (`status_for` in `main.rs`):

| Status | Events |
|--------|--------|
| `working` | UserPromptSubmit, PreToolUse, PostToolUse, PostToolUseFailure, PermissionDenied, PreCompact, SubagentStart |
| `waiting` | PermissionRequest, Elicitation, Notification (permission / elicitation) |
| `done` | Stop, Notification (idle_prompt) |
| `error` | StopFailure |
| `idle` | SessionStart |

SessionEnd removes the state. The hook publishes `pult_status`, `pult_since`, `pult_ask` (what is being asked, one
line) and `pult_ask_kind` as user vars on the tab's terminal. Claude runs hooks without a controlling terminal, so the
hook finds the tab's pts device by walking up its parent processes. It also writes a state file per Claude session
(mode 0600) which links the Claude session to the pult session through `$PULT_SESSION`. The daemon reads these files for
`ls`, for the snapshot and for keep-awake.

A hook must never disturb Claude: it swallows every error, prints nothing and always exits 0.

## Several hosts

`~/.config/pult/hosts` maps names to SSH targets. Remote operations run `~/.cargo/bin/pult …` on the host over SSH
with `BatchMode=yes` and a shared `ControlMaster` connection (10 minutes), so repeated queries are fast.
`pult ls --all` queries all hosts in parallel with a ~2 second limit. `pult attach host:3` replaces the local
process with `ssh -t host pult attach 3`: the session is attached by the pult on the remote host, over a plain SSH
terminal.

## Keep-awake

While a pult session has a Claude whose latest state is a fresh `working` (no older than 30 minutes) and the
session is alive and busy, pultd holds `systemd-inhibit` (Linux) or `caffeinate` (macOS) against sleep, idle and
lid close. The inhibitor waits on pultd's pid, so it goes away with the daemon.

## Terminal side

`wezterm.lua` reads the user vars. It colours tabs, draws the right-hand status, builds the pickers from
`pult ls --all --json`, saves the tab order to `~/.local/state/pult/tabs.json` and, on startup, reopens the live
sessions that no window shows. Notification buttons and the native tab animation need the
[pult-term](https://github.com/kisAsztal/pult-term) fork; stock WezTerm falls back to Lua-driven animation and no
notification buttons.
