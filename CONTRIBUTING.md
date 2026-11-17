# Contributing

Bug reports and pull requests are welcome.

## Build and test

```sh
cargo build
cargo test                                   # unit tests + end-to-end tests against a real daemon
cargo clippy --all-targets -- -D warnings
cargo fmt
```

The end-to-end tests (`pult/tests/daemon.rs`) start `pult daemon` on a private temporary socket and talk to it over
the wire protocol; they do not touch your real sessions or config.

To try a change on your own machine: `scripts/install.sh --no-hooks --no-service` installs the binary and config without
touching `~/.claude` or systemd. Note that restarting a running `pultd` ends all of its sessions.

## Guidelines

- Keep the code small and direct; comments explain *why*, not what.
- A hook (`pult hook`) must never fail, print to stdout, or slow Claude down.
- The daemon must never block on a client. Anything per-client is bounded.
- Changes to the wire protocol or the config formats need a note in the pull request.
- Add a test for behaviour you fix or add. `proto.rs` and `hosts.rs` are easy to unit test; session behaviour belongs in
  `pult/tests/daemon.rs`.
- Commit messages in English, one logical change per commit.

## Lua config

`wezterm/wezterm.lua` can be tried without installing it:

```sh
PULT_APPEARANCE=Dark wezterm --config-file wezterm/wezterm.lua start
```

If you change `wezterm/bg.py`, bump the `-v3` suffix of `bg_prefix` in the config, so the images are regenerated.
