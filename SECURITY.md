# Security

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting (the *Security* tab of this repository) instead of a public
issue. You can expect a first answer within a week.

## Threat model

pult is a single-user tool. It assumes your own account and your own machines are trusted, and tries to keep
other local users and the network out.

- **No network port.** The daemon listens on a unix socket, `$XDG_RUNTIME_DIR/pult.sock` (on macOS
  `$TMPDIR/pult-<uid>.sock`), created with mode 0600. The client refuses a socket owned by another user.
- **Other machines** are reached through your own `ssh`, in `BatchMode` (no password prompts), using your keys and
  your `~/.ssh/config`. pult adds no authentication of its own. Arguments sent to a remote host are shell-quoted,
  and a host target starting with `-` is rejected.
- **State files** (`~/.local/state/pult`) are created with mode 0700 / 0600. They contain the text of pending
  permission requests, such as a shell command Claude wants to run.
- **Hooks** never write to stdout (which could influence Claude), never fail, and only touch the tab's terminal and the state directory.

## Things to be aware of

- **Whatever runs in your account can use the socket**, and can therefore read and type into every session. That is
  inherent to a same-user daemon, like tmux.
- **Sessions receive the environment of the client that created them**, so secrets in that environment are visible to
  the session (but not logged). Variables of a parent Claude (`CLAUDECODE`, `CLAUDE_CODE_*`) are removed.
- **Any program in a tab can set the status user vars** by printing an escape sequence, so a tab's displayed status
  and request text are only hints. The *Allow* button in a notification therefore sends the approving key only if the
  daemon, which reads the hook's own state file, confirms that exactly that permission request is pending. Otherwise it
  just opens the tab. Only one-line, untruncated requests offer the button.
- **Image paste** copies the clipboard image to `~/.cache/pult/paste` on the remote host (mode 0600, deleted after 7 days).
- **`pult new --run CMD`** types `CMD` into the shell, as if you had typed it.
- `scripts/install.sh` edits `~/.claude/settings.json`; it saves a backup first, readable only by you.

Directory names are percent-encoded before they are sent to the terminal as OSC 7, so a directory named with escape
characters cannot inject sequences into your terminal.
