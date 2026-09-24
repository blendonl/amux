# amux

A terminal multiplexer written in Rust.

## Usage

```sh
cargo run --                     # start the server if needed, create a session and attach
cargo run -- new -s work         # create a session named "work"
cargo run -- attach -t work      # reattach (aliases: a, attach-session)
cargo run -- ls                  # list sessions
cargo run -- kill-server         # stop the server and all sessions
```

Inside a session, press `Ctrl-b d` to detach and `Ctrl-b Ctrl-b` to send a literal `Ctrl-b`.

`-L <name>` picks a named server socket in the runtime directory and `-S <path>` sets an explicit socket path. Both work like tmux's flags, so you can run an isolated dev server next to your usual one.

## Architecture

amux uses a client/server model like tmux. The server owns the shells and the client is only a view.

```
 terminal ── client ── unix socket ── server ─┬─ session ── pane ── PTY ── $SHELL
  (raw mode)                                  └─ session ── pane ── PTY ── $SHELL
```

- The **server** is started on demand as a detached `amux server` process. It listens on `$XDG_RUNTIME_DIR/amux-$UID/<name>` and logs to the same path with `.log` appended.
- Each **pane** spawns the user's shell in a PTY. Output goes through a `vt100` parser, so the server always holds the full screen state. That state is how a client gets a redraw when it reattaches.
- Each attached **client** gets frames as a diff against the last screen it was sent. Keystrokes go back as raw bytes, and resizes are sent when `SIGWINCH` arrives.
- The **protocol** uses length-prefixed `postcard` frames that carry `ClientMessage` and `ServerMessage`.

| Path                       | Responsibility                                      |
| -------------------------- | --------------------------------------------------- |
| `src/cli.rs`               | Command-line interface                              |
| `src/paths.rs`             | Runtime directory, socket and log paths             |
| `src/protocol.rs`          | Wire messages and framing                           |
| `src/server/mod.rs`        | Accept loop, session registry, shutdown             |
| `src/server/connection.rs` | Per-client request handling and the attach loop     |
| `src/server/session.rs`    | Session state                                       |
| `src/server/pane.rs`       | PTY, shell process, terminal emulation              |
| `src/client/mod.rs`        | Commands, server bootstrap, attach relay            |
| `src/client/terminal.rs`   | Raw mode, alternate screen, stdin reader            |
| `src/client/keys.rs`       | Prefix key handling                                 |

Set `AMUX_LOG=debug` before the server starts to get more verbose logs.

## Roadmap

- [ ] Windows within a session (`Ctrl-b c`, `n`, `p`)
- [ ] Pane splits with a layout tree and a cell-level compositor
- [ ] Status bar
- [ ] Config file (prefix key, shell, key bindings)
- [ ] Scrollback and copy mode
- [ ] Integration tests driving a real PTY
